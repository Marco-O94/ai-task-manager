//! Fingerprint of the Claude configuration of a checkout or of a commit (spec §8.9, M6):
//! `.claude/**`, `.mcp.json` and the files inside the tree that this configuration runs, which
//! the CLI loads (hooks, `env`, `apiKeyHelper`, MCP servers, commands, …) whenever the project
//! is Trusted. Computed on the commit worktrees are created from (the tip of the project's
//! target branch, read from the object database: [`commit_snapshot_blocking`]) when the user
//! approves the policy, then on the worktree's files before every turn
//! ([`config_snapshot_blocking`]): any difference runs the turn Isolated. Both walks share the
//! code below and differ only in their [`Source`], so a clean checkout of a commit has the
//! fingerprint of that commit.
//!
//! Records, sorted by `(path, kind)` (bytes), each hashed as `path \0 kind \0 len \0 content`
//! (`len` in decimal ASCII), SHA-256 of the whole in lowercase hex:
//! - `d` directory, content empty: every directory walked counts against the limits (a tree of
//!   empty directories would otherwise be walked for free), and an empty one appearing or going
//!   changes the fingerprint. A submodule of a commit is an empty directory, as in a new
//!   worktree;
//! - `f` regular file, `x` regular file with an executable bit (a hook script turned
//!   executable changes the fingerprint; blob mode `100755` in a commit), content = its bytes;
//! - `l` symlink, content = its target string (spec §8.9; a symlink blob's content in a
//!   commit). A symlink is also followed, since the CLI reads through it: its target must
//!   resolve inside the root (else the configuration cannot be fingerprinted: error), and what
//!   it resolves to is recorded under the link's own path (the file's `f`/`x` record, or the
//!   directory's content). A dangling symlink is only its `l` record: the day its target
//!   appears the fingerprint changes;
//! - `o` anything else (FIFO, socket, device), content empty: never opened, so a FIFO cannot
//!   stall the check;
//! - `e` (checkout only) a file the configuration runs (below) whose path is inside the root but
//!   resolves out of it through a link (a virtualenv's `python`): content = the absolute path
//!   where it leaves the root, the rest of the path applied as text. What is out of the root is
//!   neither hashed nor looked at, as with `node` or `uv` on `PATH`. A commit has no location,
//!   so there such a link cannot be fingerprinted (error).
//!
//! **Files the configuration runs.** The settings the CLI loads from the checkout
//! ([`SETTINGS_FILES`]: `hooks[*].hooks[*].command`, `statusLine.command`, any other
//! `command`, `apiKeyHelper` and the other helper commands, `env` values, the `path` of an
//! `extraKnownMarketplaces` entry with a `directory` or `file` source) and every string of
//! `.mcp.json` (`command`, `args`, `env`, …) are split into shell words; every word that names
//! a regular file inside the root (relative to it, absolute under a checkout's root, or after
//! `$CLAUDE_PROJECT_DIR`/`$PWD`) adds that file's record under its canonical path, and a
//! marketplace directory is walked whole. So `node tools/mcp.js` or a `SessionStart` hook
//! `./scripts/setup.sh` cannot be edited in a worktree without the turn running Isolated.
//! Known limit (spec §8.9): indirect execution is not followed (`npm run x`, `npx pkg@latest`,
//! `bash -c "cd tools && node server.js"`, a hook that runs repo tooling, the files those load).
//!
//! **Billing.** [`ConfigSnapshot::billing`] lists what in the settings would bill the agents
//! outside the Claude subscription ([`BILLING_SETTINGS_KEYS`], [`BILLING_ENV_VARS`]), or a
//! settings file that is not JSON (which cannot be checked for them): such a configuration is
//! never approved, and a worktree that has one never runs Trusted.
//!
//! **Names.** The CLI opens its files by name, and a filesystem that folds case and Unicode
//! (APFS, HFS+) opens `.claude/Settings.json` or `.claude/ſettings.json` (U+017F) as
//! `.claude/settings.json`: which entry is a configuration file ([`CONFIG_ENTRIES`] at the
//! root, the [`SETTINGS_FILES`] in `.claude`) is decided by what would be opened under its
//! name, never by the name listed ([`Source::opened_as`]). A checkout asks its filesystem
//! (the entry with the same device and inode as the name looked up) and records the entry
//! under the configuration file's own path, so that it is parsed; a commit, which has no
//! filesystem yet, cannot be verified if it has another entry that some filesystem could
//! open under such a name ([`may_fold_to`]).
//!
//! **Checkout.** Paths are relative to the root, as seen through the links. Every open is
//! relative to a descriptor of the root, one component at a time with `O_NOFOLLOW` (the kernel
//! never follows a link on its own, not even in a parent directory: a directory swapped for a
//! link to `~/.claude` mid-walk is an error, never read through), links being resolved by the
//! walk itself, component by component from the root's descriptor like a commit's (never
//! `realpath`, which would touch what a link out of the root names: an automounted network
//! share, a hung volume), and checked to stay inside the root. The tree is walked first; then
//! the records are sorted and each file is read and hashed in turn, so at most one file is in
//! memory (the parsed settings excepted, which are kept to be hashed as they were parsed).
//!
//! **Commit.** Trees and blobs come from `git cat-file --batch` (the hardened runner, read
//! only, no lazy fetch): tree entries by mode, symlink blobs by their target, resolved within
//! the commit's tree like a path in a checkout of it (at most [`MAX_LINK_HOPS`] links; an
//! absolute target or a `..` above the root leaves it). Content is the blob as stored: an
//! end-of-line conversion or a smudge filter (Git LFS) that changes a configuration file on
//! checkout makes every worktree differ, hence run Isolated (a known limit). The trees read
//! are kept for the walk, [`MAX_TREE_BYTES`] of them at most in all.
//!
//! Limits (errors, so the answer is "not trusted", never a partial hash): [`MAX_CONFIG_FILES`]
//! records, [`MAX_CONFIG_BYTES`] of content, [`MAX_CONFIG_DEPTH`] directory levels (a link
//! cycle hits one of them), [`MAX_COMMAND_WORDS`] words of commands resolved as paths,
//! [`MAX_WALK_STEPS`] path components resolved, [`MAX_WALK_TIME`] in all. A word longer than
//! `PATH_MAX` names no file the kernel would open, and is skipped.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use atm_types::AppError;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{CatFile, CatObject};

/// Top-level entries of the root that make up the configuration.
pub const CONFIG_ENTRIES: &[&str] = &[CLAUDE_DIR, ".mcp.json"];
/// The directory of [`SETTINGS_FILES`].
const CLAUDE_DIR: &str = ".claude";
/// The settings files the CLI loads from a checkout (project and local sources), parsed for
/// the files they run and for [`BILLING_SETTINGS_KEYS`] / [`BILLING_ENV_VARS`]. All directly
/// in [`CLAUDE_DIR`].
pub const SETTINGS_FILES: &[&str] = &[".claude/settings.json", ".claude/settings.local.json"];
/// Parsed for the files its servers run.
pub const MCP_FILE: &str = ".mcp.json";
/// Spec §8.9: at most 2000 records (files, links, directories, other entries).
pub const MAX_CONFIG_FILES: usize = 2000;
/// Content read at most, over all files.
pub const MAX_CONFIG_BYTES: u64 = 64 << 20;
/// Directory levels below the root.
pub const MAX_CONFIG_DEPTH: usize = 32;
/// Links followed at most while resolving one path of a commit (`MAXSYMLINKS` of macOS).
pub const MAX_LINK_HOPS: usize = 32;
/// Settings keys whose string values are commands the CLI runs (`command` anywhere: hooks,
/// `statusLine`, …).
const COMMAND_KEYS: &[&str] = &[
    "command",
    "apiKeyHelper",
    "awsAuthRefresh",
    "awsCredentialExport",
    "otelHeadersHelper",
];
/// Top-level keys of [`SETTINGS_FILES`] that would bill the agents outside the Claude
/// subscription (spec §8.9, §10.2; the user's requirement of 2026-09-29: agents run only on the
/// subscription, never billed through the API). Present with any non-null value, the
/// configuration is never approved and a worktree that has it runs Isolated:
/// - `apiKeyHelper`: a command whose output the CLI sends as the API key (Console billing;
///   `system/init` then says `apiKeySource: "apiKeyHelper"`);
/// - `awsAuthRefresh`, `awsCredentialExport`: refresh or export AWS credentials, which the CLI
///   uses only with Amazon Bedrock (billed by AWS).
///
/// `otelHeadersHelper` (telemetry headers) and `statusLine` bill nothing; the list follows the
/// credential helpers of the Claude Code settings (CLI 2.1.28x) and grows with them.
pub const BILLING_SETTINGS_KEYS: &[&str] =
    &["apiKeyHelper", "awsAuthRefresh", "awsCredentialExport"];
/// Names in the `env` object of [`SETTINGS_FILES`] (which the CLI applies to its own process)
/// that move billing or the credentials away from the subscription, refused like
/// [`BILLING_SETTINGS_KEYS`] whatever their value (how the CLI reads a value such as `0` is not
/// the app's to guess):
/// - `ANTHROPIC_API_KEY`: an API key, billed to its Console account;
/// - `ANTHROPIC_AUTH_TOKEN`: a bearer token for the API or a gateway, same;
/// - `ANTHROPIC_BASE_URL`: another endpoint for every request, a proxy or a gateway that
///   bills its own way and receives the user's subscription credentials;
/// - `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`, `CLAUDE_CODE_USE_FOUNDRY`: a
///   third-party provider (Amazon Bedrock, Google Vertex AI, Microsoft Foundry), billed by
///   that cloud. In the user's own environment they are shown, not removed (spec §7.2).
///
/// `.mcp.json` is not checked: its `env` reaches an MCP server's process, not the CLI's
/// requests.
pub const BILLING_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];
/// The tree objects one walk of a commit reads, together (directory listings, kept in memory
/// for the walk; not charged to [`MAX_CONFIG_BYTES`]). The root of a very large repository and
/// the directories a configuration names stay well below.
pub const MAX_TREE_BYTES: u64 = 16 << 20;
/// Distinct words of the configuration's commands looked up as paths inside the root.
pub const MAX_COMMAND_WORDS: usize = 4096;
/// Path components one walk resolves at most (a checkout's re-opens from the root included):
/// with [`MAX_COMMAND_WORDS`], what a crafted configuration can cost, the same way every time.
pub const MAX_WALK_STEPS: u64 = 1 << 20;
/// A walk that has not ended by then is an error (checked between its steps).
pub const MAX_WALK_TIME: Duration = Duration::from_secs(60);
/// The longest symlink target (`PATH_MAX` of macOS): a checkout cannot create a longer one.
const MAX_LINK_TARGET: u64 = 1024;

/// A configuration: the fingerprint, one digest per record (to name what differs), what its
/// settings allow without asking (for the confirmation, spec §10.2) and what would bill
/// outside the subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSnapshot {
    /// SHA-256 hex of the records (module docs).
    pub fingerprint: String,
    /// Sorted like the fingerprint.
    pub records: Vec<RecordDigest>,
    /// `permissions.allow` rules of [`SETTINGS_FILES`] that allow a whole tool or anything:
    /// `Bash`, `Bash(*)`, `Bash(:*)`, `*`, `mcp__server`, …
    pub broad_allow_rules: Vec<String>,
    /// `permissions.additionalDirectories` is not empty.
    pub additional_directories: bool,
    /// `enableAllProjectMcpServers: true`.
    pub all_project_mcp_servers: bool,
    /// What in [`SETTINGS_FILES`] would bill the agents outside the subscription, one Italian
    /// phrase each (`.claude/settings.json imposta apiKeyHelper`, `… imposta
    /// env.ANTHROPIC_BASE_URL`), or a settings file that is not JSON; sorted. Not empty: never
    /// approved, never Trusted.
    pub billing: Vec<String>,
}

/// One record of a [`ConfigSnapshot`]: path (lossy UTF-8), kind, SHA-256 of the hashed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDigest {
    pub path: String,
    pub kind: char,
    pub digest: [u8; 32],
}

/// The paths whose records differ between two snapshots (added, removed or changed), sorted.
pub fn differing_paths(a: &ConfigSnapshot, b: &ConfigSnapshot) -> Vec<String> {
    let set = |s: &ConfigSnapshot| {
        s.records
            .iter()
            .map(|r| (r.path.clone(), r.kind, r.digest))
            .collect::<BTreeSet<_>>()
    };
    let (a, b) = (set(a), set(b));
    a.symmetric_difference(&b)
        .map(|(path, _, _)| path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Blocking body of `git::config_fingerprint`: [`config_snapshot_blocking`]'s fingerprint.
pub fn config_fingerprint_blocking(root: &Path) -> Result<String, AppError> {
    config_snapshot_blocking(root).map(|s| s.fingerprint)
}

/// Blocking body of `git::config_snapshot`: the configuration of the checkout at `root`.
/// Errors: `Invalid` (a limit, a link out of `root`, the tree changed during the walk), `Io`
/// (unreadable entry, `root` missing).
pub fn config_snapshot_blocking(root: &Path) -> Result<ConfigSnapshot, AppError> {
    snapshot(Disk::at(root)?)
}

/// Blocking body of `Git::commit_config_snapshot`: the configuration of `commit` as a
/// checkout of it has it on disk, read through `cat` (a `cat-file --batch` of its repository)
/// whose async reads `rt` drives. Errors: `NotFound` (no such commit), `Invalid` (a limit, a
/// link out of the tree or a loop), `Git` (the reader failed or timed out).
pub fn commit_snapshot_blocking(
    rt: &tokio::runtime::Handle,
    cat: &mut CatFile,
    commit: &str,
) -> Result<ConfigSnapshot, AppError> {
    snapshot(Tree::open(rt, cat, commit)?)
}

/// Walks the configuration of `src`, then hashes it (module docs).
fn snapshot<S: Source>(src: S) -> Result<ConfigSnapshot, AppError> {
    let mut walk = Walk {
        src,
        records: Vec::new(),
        seen: HashSet::new(),
        bytes: 0,
        words: 0,
        parsed: Parsed::default(),
    };
    let root = Path::new("");
    let top = walk.src.list(root)?;
    for name in CONFIG_ENTRIES {
        if let Some(real) = walk.src.opened_as(root, &top, name)? {
            walk.visit(Path::new(name), Path::new(&real), 0)?;
        }
    }
    walk.references()?;
    walk.records
        .sort_by(|a, b| (&a.path, a.kind).cmp(&(&b.path, b.kind)));
    let mut hash = Sha256::new();
    let mut records = Vec::with_capacity(walk.records.len());
    for i in 0..walk.records.len() {
        let content = walk.content(i)?;
        let r = &walk.records[i];
        let mut one = Sha256::new();
        let len = content.len().to_string();
        let parts: [&[u8]; 5] = [&r.path, &[0, r.kind, 0], len.as_bytes(), &[0], &content];
        for part in parts {
            hash.update(part);
            one.update(part);
        }
        records.push(RecordDigest {
            path: String::from_utf8_lossy(&r.path).into_owned(),
            kind: char::from(r.kind),
            digest: one.finalize().into(),
        });
    }
    let parsed = walk.parsed;
    Ok(ConfigSnapshot {
        fingerprint: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        records,
        broad_allow_rules: parsed.broad_allow.into_iter().collect(),
        additional_directories: parsed.additional_directories,
        all_project_mcp_servers: parsed.all_project_mcp_servers,
        billing: parsed.billing.into_iter().collect(),
    })
}

fn io_error(path: &Path, e: &io::Error) -> AppError {
    AppError::io(format!("{}: {e}", path.display()))
}

fn changed_during(path: &Path) -> AppError {
    AppError::invalid(format!(
        "La configurazione Claude del repository è cambiata durante il controllo: {}",
        path.display()
    ))
}

fn too_big() -> AppError {
    AppError::invalid(format!(
        "La configurazione Claude del repository supera {} MiB: non si può verificare",
        MAX_CONFIG_BYTES >> 20
    ))
}

/// A walk error on `path`: a link met where a directory or file was just seen (`ELOOP`,
/// `ENOTDIR` under `O_NOFOLLOW`) means the tree changed meanwhile.
fn walk_error(path: &Path, e: &io::Error) -> AppError {
    match e.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => changed_during(path),
        _ => io_error(path, e),
    }
}

enum Content {
    /// Empty (`d`, `o`) or the link target / resolved path (`l`, `e`) or a parsed settings
    /// file, hashed as is.
    Inline(Vec<u8>),
    /// A regular file of a checkout at this path relative to the root (no link in it), read
    /// when hashed.
    File(PathBuf),
    /// A blob of a commit (hex id), read when hashed.
    Blob(String),
}

struct Record {
    path: Vec<u8>,
    kind: u8,
    content: Content,
}

#[derive(Default)]
struct Parsed {
    /// Strings to split into words, each word a possible file inside the root.
    commands: BTreeSet<String>,
    /// Marketplace paths (`directory`/`file` sources), walked whole.
    trees: BTreeSet<String>,
    broad_allow: BTreeSet<String>,
    additional_directories: bool,
    all_project_mcp_servers: bool,
    billing: BTreeSet<String>,
}

/// `lstat` of an entry.
#[derive(Clone, Copy)]
struct Stat {
    kind: EntryKind,
    executable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
    Link,
    Other,
}

impl Stat {
    fn of(st: &libc::stat) -> Stat {
        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => EntryKind::Dir,
            libc::S_IFREG => EntryKind::File,
            libc::S_IFLNK => EntryKind::Link,
            _ => EntryKind::Other,
        };
        Stat {
            kind,
            executable: st.st_mode & 0o111 != 0,
        }
    }

    /// A tree entry's mode: a submodule (`160000`) is the empty directory a new worktree has,
    /// a blob is executable when git checks it out so (`100755`).
    fn of_mode(mode: u32) -> Stat {
        let kind = match mode & 0o170_000 {
            0o040_000 | 0o160_000 => EntryKind::Dir,
            0o100_000 => EntryKind::File,
            0o120_000 => EntryKind::Link,
            _ => EntryKind::Other,
        };
        Stat {
            kind,
            executable: kind == EntryKind::File && mode & 0o100 != 0,
        }
    }
}

/// Where a path leads once its links are followed.
enum Resolved {
    /// Inside the root: the canonical path relative to it (empty = the root).
    Inside(PathBuf),
    /// Out of the root; for a checkout, the absolute path where it leaves the root, with
    /// the rest of the path applied as text (nothing out of the root is looked at).
    Outside(Option<PathBuf>),
}

/// The work of one walk besides its records and bytes: [`MAX_WALK_STEPS`] steps (a limit like
/// the others, reached the same way every time) within [`MAX_WALK_TIME`].
struct Budget {
    deadline: Instant,
    steps: u64,
}

impl Budget {
    fn new() -> Budget {
        Budget {
            deadline: Instant::now() + MAX_WALK_TIME,
            steps: 0,
        }
    }

    /// Charges `n` steps. Errors: `Invalid` past [`MAX_WALK_STEPS`], `Io` past the deadline
    /// (not a property of the configuration: a slow disk or a busy machine).
    fn step(&mut self, n: u64) -> Result<(), AppError> {
        self.steps = self.steps.saturating_add(n);
        if self.steps > MAX_WALK_STEPS {
            return Err(AppError::invalid(
                "La configurazione Claude del repository richiede troppi passi per essere \
                 verificata (percorsi nei comandi troppo lunghi o troppi link): non si può \
                 verificare",
            ));
        }
        if Instant::now() > self.deadline {
            return Err(AppError::io(format!(
                "Il controllo della configurazione Claude non si è concluso entro {} s",
                MAX_WALK_TIME.as_secs()
            )));
        }
        Ok(())
    }
}

/// `name` is not `target` byte for byte, but a filesystem that folds case or Unicode may open
/// it as `target` (lowercase ASCII, as every configuration name): `target` with other ASCII
/// case, or a name with non-ASCII bytes whose ASCII bytes, lowercased, could be what is left
/// of `target` once the non-ASCII characters are folded, since such a character folds to
/// letters (`ſ` → `s`, the Kelvin sign → `k`, `ﬁ` → `fi`) or to nothing (HFS+ ignores some
/// code points), never to a `.`: they are a subsequence of `target` with all its dots.
/// Conservative: `.claudé` counts too.
fn may_fold_to(name: &[u8], target: &str) -> bool {
    let target = target.as_bytes();
    if name == target {
        return false;
    }
    if name.is_ascii() {
        return name.eq_ignore_ascii_case(target);
    }
    let ascii: Vec<u8> = name
        .iter()
        .filter(|b| b.is_ascii())
        .map(u8::to_ascii_lowercase)
        .collect();
    let dots = |s: &[u8]| s.iter().filter(|&&b| b == b'.').count();
    let mut rest = target.iter();
    dots(&ascii) == dots(target) && ascii.iter().all(|b| rest.any(|t| t == b))
}

/// The tree a snapshot reads: a checkout on disk ([`Disk`]) or a commit ([`Tree`]). `real`
/// paths are relative to the root, made of normal components only, with no link among their
/// parents (they come from the walk or from [`Source::resolve`]); empty = the root.
trait Source {
    /// A checkout's canonical root, under which an absolute word of a command is inside it; a
    /// commit has no location.
    fn root(&self) -> Option<&Path>;
    /// `lstat` of `real`; `None` when nothing is there.
    fn stat(&mut self, real: &Path) -> Result<Option<Stat>, AppError>;
    /// The target of the link at `real`.
    fn read_link(&mut self, real: &Path) -> Result<Vec<u8>, AppError>;
    /// The names in the directory `real`, unsorted.
    fn list(&mut self, real: &Path) -> Result<Vec<OsString>, AppError>;
    /// The entry of the directory `real`, listed as `names`, that the CLI opens by the name
    /// `wanted` (module docs, **Names**): `wanted` itself when listed; else, in a checkout, the
    /// one its filesystem opens under that name. `None` when there is none. Errors: in a
    /// commit, another entry that a filesystem may open as `wanted`.
    fn opened_as(
        &mut self,
        real: &Path,
        names: &[OsString],
        wanted: &str,
    ) -> Result<Option<OsString>, AppError>;
    /// Where `path` (relative to the root, `.`/`..` allowed) leads, links followed one
    /// component at a time from the root, like the kernel: `None` when nothing is there (a
    /// missing component, or a file used as a directory). Only metadata and link targets
    /// inside the root are read on the way, never any content, nothing out of the root.
    fn resolve(&mut self, path: &Path) -> Result<Option<Resolved>, AppError>;
    /// A handle on the regular file at `real`, read by [`Source::read`] when hashed.
    fn file(&mut self, real: &Path) -> Result<Content, AppError>;
    /// At most `budget + 1` bytes of the file `content` names (the walk charges them).
    fn read(&mut self, content: &Content, budget: u64) -> Result<Vec<u8>, AppError>;
    /// The `e` record of a file the configuration runs at `lexical`, inside the root, that
    /// resolves out of it (to `resolved`).
    fn escaping(&self, lexical: &Path, resolved: Option<&Path>) -> Result<Vec<u8>, AppError>;
    /// The walk's [`Budget`], which [`Source::resolve`] charges too.
    fn budget(&mut self) -> &mut Budget;
}

struct Walk<S> {
    src: S,
    records: Vec<Record>,
    /// `(path, kind)` recorded: a file both in `.claude/` and named by a command is one record.
    seen: HashSet<(Vec<u8>, u8)>,
    bytes: u64,
    /// Words of commands looked up as paths ([`MAX_COMMAND_WORDS`]).
    words: usize,
    parsed: Parsed,
}

impl<S: Source> Walk<S> {
    fn push(&mut self, logical: &Path, kind: u8, content: Content) -> Result<(), AppError> {
        let path = logical.as_os_str().as_bytes().to_vec();
        if !self.seen.insert((path.clone(), kind)) {
            return Ok(());
        }
        if self.records.len() >= MAX_CONFIG_FILES {
            return Err(AppError::invalid(format!(
                "La configurazione Claude del repository (.claude/, .mcp.json e i file che \
                 esegue) ha più di {MAX_CONFIG_FILES} voci tra file e cartelle: non si può \
                 verificare"
            )));
        }
        self.records.push(Record {
            path,
            kind,
            content,
        });
        Ok(())
    }

    /// The file `content` names, within the byte budget.
    fn read(&mut self, content: &Content) -> Result<Vec<u8>, AppError> {
        let budget = MAX_CONFIG_BYTES.saturating_sub(self.bytes);
        let bytes = self.src.read(content, budget)?;
        self.bytes += bytes.len() as u64;
        if self.bytes > MAX_CONFIG_BYTES {
            return Err(too_big());
        }
        Ok(bytes)
    }

    /// `logical` (relative to the root) found at `real`, not followed yet.
    fn visit(&mut self, logical: &Path, real: &Path, depth: usize) -> Result<(), AppError> {
        self.src.budget().step(1)?;
        let Some(stat) = self.src.stat(real)? else {
            return Ok(());
        };
        if stat.kind != EntryKind::Link {
            return self.entry(logical, real, stat, depth);
        }
        let target = self.src.read_link(real)?;
        self.push(logical, b'l', Content::Inline(target.clone()))?;
        let resolved = match self.src.resolve(real)? {
            // Dangling: nothing to read through it (yet).
            None => return Ok(()),
            Some(Resolved::Inside(resolved)) => resolved,
            Some(Resolved::Outside(_)) => {
                return Err(AppError::invalid(format!(
                    "La configurazione Claude del repository contiene un link che esce dal \
                     repository: {} → {}",
                    logical.display(),
                    OsStr::from_bytes(&target).to_string_lossy()
                )));
            }
        };
        // Canonical: no link left in it, unless one appeared since (then it is an `o`, or an
        // error when met in a parent, never followed out of the root).
        let Some(stat) = self.src.stat(&resolved)? else {
            return Err(changed_during(&resolved));
        };
        match stat.kind {
            EntryKind::Link => self.push(logical, b'o', Content::Inline(Vec::new())),
            _ => self.entry(logical, &resolved, stat, depth),
        }
    }

    /// A directory, file or other entry at `real` (never a link to follow).
    fn entry(
        &mut self,
        logical: &Path,
        real: &Path,
        stat: Stat,
        depth: usize,
    ) -> Result<(), AppError> {
        match stat.kind {
            EntryKind::Dir => self.dir(logical, real, depth),
            EntryKind::File => {
                let kind = if stat.executable { b'x' } else { b'f' };
                let file = self.src.file(real)?;
                let parsed = SETTINGS_FILES
                    .iter()
                    .chain([&MCP_FILE])
                    .any(|p| logical == Path::new(p));
                if !parsed {
                    return self.push(logical, kind, file);
                }
                // Read now, so that what is parsed is what is hashed.
                let content = self.read(&file)?;
                self.parse(logical, &content);
                self.push(logical, kind, Content::Inline(content))
            }
            EntryKind::Link | EntryKind::Other => {
                self.push(logical, b'o', Content::Inline(Vec::new()))
            }
        }
    }

    fn dir(&mut self, logical: &Path, real: &Path, depth: usize) -> Result<(), AppError> {
        if depth >= MAX_CONFIG_DEPTH {
            return Err(AppError::invalid(format!(
                "La configurazione Claude del repository è annidata oltre {MAX_CONFIG_DEPTH} \
                 livelli: {}",
                logical.display()
            )));
        }
        self.push(logical, b'd', Content::Inline(Vec::new()))?;
        let mut names = self.src.list(real)?;
        names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        // The entries the CLI opens as its settings files, under another name on a
        // filesystem that folds case or Unicode: recorded (hence parsed) as those files.
        let mut aliases: Vec<(OsString, &str)> = Vec::new();
        if logical == Path::new(CLAUDE_DIR) {
            for file in SETTINGS_FILES {
                let wanted = &file[CLAUDE_DIR.len() + 1..];
                if let Some(name) = self.src.opened_as(real, &names, wanted)?
                    && name != wanted
                {
                    aliases.push((name, wanted));
                }
            }
        }
        for name in names {
            let as_name = aliases
                .iter()
                .find(|(alias, _)| *alias == name)
                .map_or(name.as_os_str(), |(_, wanted)| OsStr::new(wanted));
            self.visit(&logical.join(as_name), &real.join(&name), depth + 1)?;
        }
        Ok(())
    }

    /// The bytes of record `i`, read now for a file.
    fn content(&mut self, i: usize) -> Result<Vec<u8>, AppError> {
        match std::mem::replace(&mut self.records[i].content, Content::Inline(Vec::new())) {
            Content::Inline(bytes) => Ok(bytes),
            file => self.read(&file),
        }
    }

    /// Collects what a settings file or `.mcp.json` runs, allows and bills (module docs); a
    /// file that is not JSON runs nothing the CLI could parse either, but a settings file
    /// that is not JSON cannot be checked for billing keys.
    fn parse(&mut self, logical: &Path, content: &[u8]) {
        let p = &mut self.parsed;
        let settings = logical != Path::new(MCP_FILE);
        let v = match serde_json::from_slice::<Value>(content) {
            Ok(v) => v,
            Err(_) if settings && !content.trim_ascii().is_empty() => {
                p.billing.insert(format!(
                    "{} non è JSON valido (non si può controllare se fattura via API)",
                    logical.display()
                ));
                return;
            }
            Err(_) => return,
        };
        if !settings {
            strings(&v, &mut p.commands);
            return;
        }
        for key in BILLING_SETTINGS_KEYS {
            if v.get(key).is_some_and(|value| !value.is_null()) {
                p.billing
                    .insert(format!("{} imposta {key}", logical.display()));
            }
        }
        for var in v["env"].as_object().into_iter().flat_map(|env| env.keys()) {
            if BILLING_ENV_VARS.contains(&var.as_str()) {
                p.billing
                    .insert(format!("{} imposta env.{var}", logical.display()));
            }
        }
        keyed_strings(&v, COMMAND_KEYS, &mut p.commands);
        strings(&v["env"], &mut p.commands);
        for market in v["extraKnownMarketplaces"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
        {
            let source = &market["source"];
            if matches!(source["source"].as_str(), Some("directory" | "file"))
                && let Some(path) = source["path"].as_str()
            {
                p.trees.insert(path.to_owned());
            }
        }
        let permissions = &v["permissions"];
        for rule in permissions["allow"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if is_broad_rule(rule) {
                p.broad_allow.insert(rule.to_owned());
            }
        }
        p.additional_directories |= permissions["additionalDirectories"]
            .as_array()
            .is_some_and(|dirs| !dirs.is_empty());
        p.all_project_mcp_servers |= v["enableAllProjectMcpServers"] == Value::Bool(true);
    }

    /// Records the files inside the root that the parsed configuration names (module docs).
    /// A word longer than `PATH_MAX` names nothing the kernel would open.
    fn references(&mut self) -> Result<(), AppError> {
        let words: BTreeSet<String> = self
            .parsed
            .commands
            .iter()
            .flat_map(|c| shell_words(c))
            .filter(|w| w.len() <= libc::PATH_MAX as usize)
            .collect();
        for word in words {
            self.reference(&word, false)?;
        }
        let trees = std::mem::take(&mut self.parsed.trees);
        for tree in &trees {
            self.reference(tree, true)?;
        }
        self.parsed.trees = trees;
        Ok(())
    }

    /// One word of a command: a regular file inside the root → its records; with `tree`, a
    /// directory is walked whole too. Anything else (a program on `PATH`, a flag, a path out
    /// of the root, nothing there) adds nothing.
    fn reference(&mut self, word: &str, tree: bool) -> Result<(), AppError> {
        let Some(relative) = self.inside(word) else {
            return Ok(());
        };
        let Some(lexical) = normalize(&relative) else {
            return Ok(());
        };
        self.words += 1;
        if self.words > MAX_COMMAND_WORDS {
            return Err(AppError::invalid(format!(
                "La configurazione Claude del repository nomina più di {MAX_COMMAND_WORDS} \
                 percorsi nei suoi comandi: non si può verificare"
            )));
        }
        self.src.budget().step(1)?;
        let real = match self.src.resolve(&relative)? {
            None => return Ok(()),
            Some(Resolved::Inside(real)) => real,
            Some(Resolved::Outside(resolved)) => {
                let content = self.src.escaping(&lexical, resolved.as_deref())?;
                return self.push(&lexical, b'e', Content::Inline(content));
            }
        };
        if real.as_os_str().is_empty() {
            return Ok(());
        }
        let Some(stat) = self.src.stat(&real)? else {
            return Ok(());
        };
        match stat.kind {
            // Not parsed again: only what the CLI loads as settings is.
            EntryKind::File => {
                let kind = if stat.executable { b'x' } else { b'f' };
                let file = self.src.file(&real)?;
                self.push(&real, kind, file)
            }
            EntryKind::Dir if tree => self.dir(&real, &real, 0),
            _ => Ok(()),
        }
    }

    /// `word` as a path relative to the root: relative already, absolute under a checkout's
    /// root, or after `$CLAUDE_PROJECT_DIR` / `$PWD` (the CLI's cwd is the checkout). `None`
    /// otherwise.
    fn inside(&self, word: &str) -> Option<PathBuf> {
        for var in ["CLAUDE_PROJECT_DIR", "PWD"] {
            for prefix in [format!("${var}"), format!("${{{var}}}")] {
                if let Some(rest) = word.strip_prefix(&prefix) {
                    return rest
                        .strip_prefix('/')
                        .map(PathBuf::from)
                        .filter(|p| !p.as_os_str().is_empty());
                }
            }
        }
        if word.starts_with(['$', '~']) || word.starts_with('-') {
            return None;
        }
        let path = Path::new(word);
        if path.is_absolute() {
            let root = self.src.root()?;
            return path.strip_prefix(root).ok().map(Path::to_path_buf);
        }
        Some(path.to_path_buf())
    }
}

// ---- a checkout on disk ----------------------------------------------------------------------

struct Disk {
    /// Canonical.
    root: PathBuf,
    root_fd: OwnedFd,
    budget: Budget,
}

impl Disk {
    fn at(root: &Path) -> Result<Disk, AppError> {
        let canonical = fs::canonicalize(root).map_err(|e| io_error(root, &e))?;
        let root_fd = open_at(libc::AT_FDCWD, canonical.as_os_str(), libc::O_DIRECTORY)
            .map_err(|e| io_error(&canonical, &e))?;
        Ok(Disk {
            root: canonical,
            root_fd,
            budget: Budget::new(),
        })
    }

    /// The directory holding `real` (relative to the root, only normal components), opened
    /// from the root one component at a time without following links, and `real`'s last
    /// component.
    fn parent(&self, real: &Path) -> io::Result<(OwnedFd, OsString)> {
        let mut names = Vec::new();
        for c in real.components() {
            match c {
                Component::Normal(name) => names.push(name),
                _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
            }
        }
        let last = names
            .pop()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut dir = self.root_fd.try_clone()?;
        for name in names {
            dir = open_at(dir.as_raw_fd(), name, libc::O_DIRECTORY)?;
        }
        Ok((dir, last.to_owned()))
    }

    /// Opens `real` without following any link (`flags` plus `O_NOFOLLOW`).
    fn open(&self, real: &Path, flags: libc::c_int) -> io::Result<OwnedFd> {
        if real.as_os_str().is_empty() {
            return self.root_fd.try_clone();
        }
        let (dir, name) = self.parent(real)?;
        open_at(dir.as_raw_fd(), &name, flags)
    }
}

impl Source for Disk {
    fn root(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn stat(&mut self, real: &Path) -> Result<Option<Stat>, AppError> {
        let stat = if real.as_os_str().is_empty() {
            fstat(self.root_fd.as_raw_fd())
        } else {
            self.parent(real)
                .and_then(|(dir, name)| lstat_at(dir.as_raw_fd(), &name))
        };
        match stat {
            Ok(stat) => Ok(Some(stat)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(walk_error(real, &e)),
        }
    }

    fn read_link(&mut self, real: &Path) -> Result<Vec<u8>, AppError> {
        let (dir, name) = self.parent(real).map_err(|e| walk_error(real, &e))?;
        read_link_at(dir.as_raw_fd(), &name).map_err(|e| walk_error(real, &e))
    }

    fn list(&mut self, real: &Path) -> Result<Vec<OsString>, AppError> {
        let dir = self
            .open(real, libc::O_DIRECTORY)
            .map_err(|e| walk_error(real, &e))?;
        list_dir(&dir).map_err(|e| walk_error(real, &e))
    }

    /// The filesystem decides: `wanted` looked up in the directory (without following a
    /// link), then the listed entry with its device and inode, a name that may fold to it
    /// first. Two entries (hard links under other names, which no checkout creates) cannot be
    /// told apart: error.
    fn opened_as(
        &mut self,
        real: &Path,
        names: &[OsString],
        wanted: &str,
    ) -> Result<Option<OsString>, AppError> {
        if names.iter().any(|n| n == wanted) {
            return Ok(Some(wanted.into()));
        }
        let dir = self
            .open(real, libc::O_DIRECTORY)
            .map_err(|e| walk_error(real, &e))?;
        let at = real.join(wanted);
        let id = match stat_at(dir.as_raw_fd(), OsStr::new(wanted)) {
            Ok(st) => file_id(&st),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(walk_error(&at, &e)),
        };
        let (likely, others): (Vec<&OsString>, Vec<&OsString>) = names
            .iter()
            .partition(|n| may_fold_to(n.as_bytes(), wanted));
        for candidates in [likely, others] {
            let mut same = Vec::new();
            for name in candidates {
                match stat_at(dir.as_raw_fd(), name) {
                    Ok(st) if file_id(&st) == id => same.push(name),
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(walk_error(&real.join(name), &e)),
                }
            }
            match same.as_slice() {
                [] => {}
                [one] => return Ok(Some((*one).clone())),
                _ => {
                    return Err(AppError::invalid(format!(
                        "La configurazione Claude del repository ha più voci per {}: non si può \
                         verificare",
                        at.display()
                    )));
                }
            }
        }
        // There, but not listed: it appeared after the listing.
        Err(changed_during(&at))
    }

    /// Links resolved by the walk from the root's descriptor, one component at a time (each
    /// looked up again from the root, and charged as such to the budget): a target out of the
    /// root is never looked at, only its text.
    fn resolve(&mut self, path: &Path) -> Result<Option<Resolved>, AppError> {
        let mut dirs: Vec<OsString> = Vec::new();
        let mut last: Option<OsString> = None;
        let mut todo = parts(path.as_os_str().as_bytes());
        let mut hops = 0;
        while let Some(part) = todo.pop_front() {
            self.budget.step(1)?;
            if last.is_some() {
                // `file/…`: `ENOTDIR`, nothing there.
                return Ok(None);
            }
            match part {
                Part::Current => {}
                Part::Root => {
                    return Ok(Some(Resolved::Outside(Some(lexical(
                        PathBuf::from("/"),
                        todo,
                    )))));
                }
                Part::Parent => {
                    if dirs.pop().is_none() {
                        let above = self.root.parent().unwrap_or(Path::new("/"));
                        return Ok(Some(Resolved::Outside(Some(lexical(
                            above.to_path_buf(),
                            todo,
                        )))));
                    }
                }
                Part::Name(name) => {
                    let real: PathBuf = dirs.iter().chain([&name]).collect();
                    self.budget.step(dirs.len() as u64)?;
                    let Some(stat) = self.stat(&real)? else {
                        return Ok(None);
                    };
                    match stat.kind {
                        EntryKind::Link => {
                            hops += 1;
                            if hops > MAX_LINK_HOPS {
                                return Err(link_loop(path));
                            }
                            let mut next = parts(&self.read_link(&real)?);
                            next.extend(todo);
                            todo = next;
                        }
                        EntryKind::Dir => dirs.push(name),
                        EntryKind::File | EntryKind::Other => last = Some(name),
                    }
                }
            }
        }
        let mut inside: PathBuf = dirs.into_iter().collect();
        if let Some(name) = last {
            inside.push(name);
        }
        Ok(Some(Resolved::Inside(inside)))
    }

    fn file(&mut self, real: &Path) -> Result<Content, AppError> {
        Ok(Content::File(real.to_path_buf()))
    }

    /// A regular file, opened without following a link and without blocking (a file swapped
    /// for a FIFO or a link since it was seen is never read through).
    fn read(&mut self, content: &Content, budget: u64) -> Result<Vec<u8>, AppError> {
        let Content::File(real) = content else {
            return Err(AppError::internal("fingerprint: contenuto non su disco"));
        };
        let fd = self
            .open(real, libc::O_NONBLOCK)
            .map_err(|e| walk_error(real, &e))?;
        let file = File::from(fd);
        let meta = file.metadata().map_err(|e| io_error(real, &e))?;
        if !meta.is_file() {
            return Err(changed_during(real));
        }
        let mut bytes = Vec::new();
        file.take(budget + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(real, &e))?;
        Ok(bytes)
    }

    fn escaping(&self, _lexical: &Path, resolved: Option<&Path>) -> Result<Vec<u8>, AppError> {
        Ok(resolved
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .unwrap_or_default())
    }

    fn budget(&mut self) -> &mut Budget {
        &mut self.budget
    }
}

/// `base` with `rest` applied as text (`/` restarts at the root, `..` drops a component):
/// where a path goes once it has left the root, without looking.
fn lexical(mut base: PathBuf, rest: VecDeque<Part>) -> PathBuf {
    for part in rest {
        match part {
            Part::Root => base = PathBuf::from("/"),
            Part::Current => {}
            Part::Parent => {
                base.pop();
            }
            Part::Name(name) => base.push(name),
        }
    }
    base
}

fn link_loop(path: &Path) -> AppError {
    AppError::invalid(format!(
        "La configurazione Claude del repository ha un ciclo di link: {}",
        path.display()
    ))
}

// ---- a commit ----------------------------------------------------------------------------------

/// One entry of a tree object.
#[derive(Clone)]
struct TreeEntry {
    name: OsString,
    mode: u32,
    /// Hex id.
    oid: String,
}

const MODE_TREE: u32 = 0o040_000;

/// A commit's tree, read through `git cat-file --batch`; trees are cached by id, their
/// entries sorted by name ([`parse_tree`]).
struct Tree<'a> {
    rt: &'a tokio::runtime::Handle,
    cat: &'a mut CatFile,
    /// The root tree's id.
    root: String,
    trees: HashMap<String, Rc<[TreeEntry]>>,
    /// Bytes of the tree objects read ([`MAX_TREE_BYTES`]).
    tree_bytes: u64,
    budget: Budget,
}

/// The entry named `name` of a tree's (sorted) entries.
fn find_entry<'e>(entries: &'e [TreeEntry], name: &OsStr) -> Option<&'e TreeEntry> {
    entries
        .binary_search_by(|e| e.name.as_bytes().cmp(name.as_bytes()))
        .ok()
        .map(|i| &entries[i])
}

/// One part of a path being resolved in a commit.
enum Part {
    Root,
    Current,
    Parent,
    Name(OsString),
}

/// `path`'s parts, split at every `/` as the kernel does: `a/./b`, `file/.` and `file/` keep
/// what makes them different from `a/b` and `file`.
fn parts(path: &[u8]) -> VecDeque<Part> {
    let mut out = VecDeque::new();
    if path.starts_with(b"/") {
        out.push_back(Part::Root);
    }
    for name in path.split(|&b| b == b'/').filter(|n| !n.is_empty()) {
        out.push_back(match name {
            b"." => Part::Current,
            b".." => Part::Parent,
            name => Part::Name(OsStr::from_bytes(name).to_owned()),
        });
    }
    if path.len() > 1 && path.ends_with(b"/") {
        out.push_back(Part::Current);
    }
    out
}

impl<'a> Tree<'a> {
    fn open(
        rt: &'a tokio::runtime::Handle,
        cat: &'a mut CatFile,
        commit: &str,
    ) -> Result<Tree<'a>, AppError> {
        let budget = Budget::new();
        let spec = format!("{commit}^{{tree}}");
        match rt.block_on(cat.get(&spec, MAX_TREE_BYTES))? {
            CatObject::Found { oid, kind, data } if kind == "tree" => {
                let entries = parse_tree(&data, oid.len() / 2)?;
                let trees = HashMap::from([(oid.clone(), entries)]);
                Ok(Tree {
                    rt,
                    cat,
                    root: oid,
                    trees,
                    tree_bytes: data.len() as u64,
                    budget,
                })
            }
            CatObject::Found { .. } | CatObject::Missing => Err(AppError::not_found(format!(
                "Il commit {commit} non si trova nel repository"
            ))),
            CatObject::TooLarge(_) => Err(tree_too_big(Path::new(""))),
        }
    }

    /// The entries of the tree `oid`, within what is left of [`MAX_TREE_BYTES`].
    fn tree(&mut self, oid: &str, at: &Path) -> Result<Rc<[TreeEntry]>, AppError> {
        if let Some(entries) = self.trees.get(oid) {
            return Ok(Rc::clone(entries));
        }
        let left = MAX_TREE_BYTES.saturating_sub(self.tree_bytes);
        let entries = match self.rt.block_on(self.cat.get(oid, left))? {
            CatObject::Found { kind, data, .. } if kind == "tree" => {
                self.tree_bytes += data.len() as u64;
                parse_tree(&data, oid.len() / 2)?
            }
            CatObject::TooLarge(_) => return Err(tree_too_big(at)),
            _ => return Err(missing_object(oid, at)),
        };
        self.trees.insert(oid.to_owned(), Rc::clone(&entries));
        Ok(entries)
    }

    /// The blob `oid`, at most `max` bytes (`None` beyond).
    fn blob(&mut self, oid: &str, max: u64, at: &Path) -> Result<Option<Vec<u8>>, AppError> {
        match self.rt.block_on(self.cat.get(oid, max))? {
            CatObject::Found { kind, data, .. } if kind == "blob" => Ok(Some(data)),
            CatObject::TooLarge(_) => Ok(None),
            _ => Err(missing_object(oid, at)),
        }
    }

    /// The entry at `real` (the root is a tree entry without a name); `None` when nothing is
    /// there, or inside a submodule.
    fn entry_at(&mut self, real: &Path) -> Result<Option<TreeEntry>, AppError> {
        let mut names = Vec::new();
        for c in real.components() {
            match c {
                Component::Normal(name) => names.push(name),
                _ => return Err(changed_during(real)),
            }
        }
        let mut entry = TreeEntry {
            name: OsString::new(),
            mode: MODE_TREE,
            oid: self.root.clone(),
        };
        for name in names {
            match Stat::of_mode(entry.mode).kind {
                EntryKind::Dir if entry.mode == MODE_TREE => {}
                // A submodule is an empty directory.
                EntryKind::Dir => return Ok(None),
                // Never a link or a file among the parents of a path of the walk.
                _ => return Err(changed_during(real)),
            }
            let entries = self.tree(&entry.oid, real)?;
            match find_entry(&entries, name) {
                Some(e) => entry = e.clone(),
                None => return Ok(None),
            }
        }
        Ok(Some(entry))
    }

    /// The entry at `real`, which the walk has just seen.
    fn seen(&mut self, real: &Path) -> Result<TreeEntry, AppError> {
        self.entry_at(real)?.ok_or_else(|| changed_during(real))
    }

    /// The target of the link `entry` at `at`: not empty, no NUL, at most [`MAX_LINK_TARGET`]
    /// bytes (a checkout could create no other).
    fn link_target(&mut self, entry: &TreeEntry, at: &Path) -> Result<Vec<u8>, AppError> {
        match self.blob(&entry.oid, MAX_LINK_TARGET, at)? {
            Some(target) if !target.is_empty() && !target.contains(&0) => Ok(target),
            _ => Err(AppError::invalid(format!(
                "La configurazione Claude del commit contiene un link non valido: {}",
                at.display()
            ))),
        }
    }
}

impl Source for Tree<'_> {
    fn root(&self) -> Option<&Path> {
        None
    }

    fn stat(&mut self, real: &Path) -> Result<Option<Stat>, AppError> {
        Ok(self.entry_at(real)?.map(|e| Stat::of_mode(e.mode)))
    }

    fn read_link(&mut self, real: &Path) -> Result<Vec<u8>, AppError> {
        let entry = self.seen(real)?;
        if Stat::of_mode(entry.mode).kind != EntryKind::Link {
            return Err(changed_during(real));
        }
        self.link_target(&entry, real)
    }

    fn list(&mut self, real: &Path) -> Result<Vec<OsString>, AppError> {
        let entry = self.seen(real)?;
        match Stat::of_mode(entry.mode).kind {
            EntryKind::Dir if entry.mode == MODE_TREE => {}
            // A submodule: empty.
            EntryKind::Dir => return Ok(Vec::new()),
            _ => return Err(changed_during(real)),
        }
        let entries = self.tree(&entry.oid, real)?;
        Ok(entries.iter().map(|e| e.name.clone()).collect())
    }

    /// No filesystem yet: `wanted` byte for byte, and no other entry that a checkout on a
    /// filesystem folding case or Unicode could open under that name (it would be the one
    /// the CLI reads there, or collide with `wanted` on checkout).
    fn opened_as(
        &mut self,
        real: &Path,
        names: &[OsString],
        wanted: &str,
    ) -> Result<Option<OsString>, AppError> {
        if let Some(other) = names.iter().find(|n| may_fold_to(n.as_bytes(), wanted)) {
            return Err(AppError::invalid(format!(
                "La configurazione Claude del commit non si può verificare: {} è un altro nome \
                 di {} su un disco che non distingue maiuscole, minuscole o forme Unicode \
                 (come APFS), dove Claude Code lo leggerebbe come tale",
                real.join(other).display(),
                real.join(wanted).display()
            )));
        }
        Ok(names.iter().find(|n| *n == wanted).cloned())
    }

    fn budget(&mut self) -> &mut Budget {
        &mut self.budget
    }

    fn resolve(&mut self, path: &Path) -> Result<Option<Resolved>, AppError> {
        // The directories resolved so far below the root, with their tree (`None`: a
        // submodule, empty), and a regular file (or other entry) that must end the path.
        let mut dirs: Vec<(OsString, Option<String>)> = Vec::new();
        let mut last: Option<OsString> = None;
        let mut todo = parts(path.as_os_str().as_bytes());
        let mut hops = 0;
        while let Some(part) = todo.pop_front() {
            self.budget.step(1)?;
            if last.is_some() {
                // `file/…`: `ENOTDIR`, nothing there.
                return Ok(None);
            }
            match part {
                Part::Current => {}
                Part::Root => return Ok(Some(Resolved::Outside(None))),
                Part::Parent => {
                    if dirs.pop().is_none() {
                        return Ok(Some(Resolved::Outside(None)));
                    }
                }
                Part::Name(name) => {
                    let tree = match dirs.last() {
                        None => Some(self.root.clone()),
                        Some((_, tree)) => tree.clone(),
                    };
                    let Some(tree) = tree else {
                        return Ok(None);
                    };
                    let entries = self.tree(&tree, path)?;
                    let Some(entry) = find_entry(&entries, &name).cloned() else {
                        return Ok(None);
                    };
                    match Stat::of_mode(entry.mode).kind {
                        EntryKind::Link => {
                            hops += 1;
                            if hops > MAX_LINK_HOPS {
                                return Err(AppError::invalid(format!(
                                    "La configurazione Claude del commit ha un ciclo di link: \
                                     {}",
                                    path.display()
                                )));
                            }
                            let target = self.link_target(&entry, path)?;
                            let mut next = parts(&target);
                            next.extend(todo);
                            todo = next;
                        }
                        EntryKind::Dir => {
                            let tree = (entry.mode == MODE_TREE).then_some(entry.oid);
                            dirs.push((name, tree));
                        }
                        EntryKind::File | EntryKind::Other => last = Some(name),
                    }
                }
            }
        }
        let mut inside: PathBuf = dirs.into_iter().map(|(name, _)| name).collect();
        if let Some(name) = last {
            inside.push(name);
        }
        Ok(Some(Resolved::Inside(inside)))
    }

    fn file(&mut self, real: &Path) -> Result<Content, AppError> {
        Ok(Content::Blob(self.seen(real)?.oid))
    }

    fn read(&mut self, content: &Content, budget: u64) -> Result<Vec<u8>, AppError> {
        let Content::Blob(oid) = content else {
            return Err(AppError::internal("fingerprint: contenuto non nel commit"));
        };
        self.blob(oid, budget, Path::new(oid))?.ok_or_else(too_big)
    }

    fn escaping(&self, lexical: &Path, _resolved: Option<&Path>) -> Result<Vec<u8>, AppError> {
        Err(AppError::invalid(format!(
            "La configurazione Claude del commit esegue {}, che un link porta fuori dal \
             repository: non si può verificare",
            lexical.display()
        )))
    }
}

fn tree_too_big(at: &Path) -> AppError {
    AppError::invalid(format!(
        "Le cartelle del commit da leggere per la configurazione Claude superano {} MiB: {}",
        MAX_TREE_BYTES >> 20,
        at.display()
    ))
}

fn missing_object(oid: &str, at: &Path) -> AppError {
    AppError::git(format!(
        "oggetto {oid} mancante o di tipo inatteso nel repository ({})",
        at.display()
    ))
}

/// The entries of a tree object (`<octal mode> <name>\0<id, oid_len bytes>` each), sorted
/// by name for [`find_entry`]. A name git would never check out as such (empty, `.`, `..`,
/// with a `/`) or twice in the same tree (which `git fsck` rejects): `Invalid`, since what a
/// checkout would make of it cannot be told.
fn parse_tree(data: &[u8], oid_len: usize) -> Result<Rc<[TreeEntry]>, AppError> {
    let bad = || AppError::git("oggetto tree non valido nel repository");
    let mut entries = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest.iter().position(|&b| b == b' ').ok_or_else(bad)?;
        let mode = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .ok_or_else(bad)?;
        rest = &rest[space + 1..];
        let nul = rest.iter().position(|&b| b == 0).ok_or_else(bad)?;
        let name = OsStr::from_bytes(&rest[..nul]).to_owned();
        rest = &rest[nul + 1..];
        if oid_len == 0 || rest.len() < oid_len {
            return Err(bad());
        }
        let oid = rest[..oid_len].iter().map(|b| format!("{b:02x}")).collect();
        rest = &rest[oid_len..];
        entries.push(TreeEntry { name, mode, oid });
    }
    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let odd = |n: &[u8]| n.is_empty() || n == b"." || n == b".." || n.contains(&b'/');
    if entries.iter().any(|e| odd(e.name.as_bytes()))
        || entries.windows(2).any(|w| w[0].name == w[1].name)
    {
        return Err(AppError::invalid(
            "Una cartella del commit ha voci con nomi non validi o ripetuti: la configurazione \
             Claude non si può verificare",
        ));
    }
    Ok(entries.into())
}

/// `path` without `.` and with `..` applied, as text; `None` if it climbs out of the root or
/// is the root itself.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// A rule that allows a whole tool, or anything within it: no `(…)`, or `*`, `:*`, `**`.
pub fn is_broad_rule(rule: &str) -> bool {
    let rule = rule.trim();
    match rule.split_once('(') {
        None => !rule.is_empty(),
        Some((_, spec)) => matches!(spec.trim_end_matches(')').trim(), "" | "*" | ":*" | "**"),
    }
}

/// The words of a command, roughly as a shell splits them: quotes and backslashes dropped,
/// split at blanks and at `; & | ( ) < > \` = , :`, `$(pwd)` read as `$PWD`.
pub fn shell_words(command: &str) -> Vec<String> {
    let command = command.replace("$(pwd)", "$PWD").replace("`pwd`", "$PWD");
    let mut words = Vec::new();
    let mut word = String::new();
    for c in command.chars() {
        match c {
            '"' | '\'' | '\\' => {}
            c if c.is_whitespace() || ";&|()<>`=,:".contains(c) => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Every string in `v` (itself, or inside its arrays and objects).
fn strings(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) => {
            out.insert(s.clone());
        }
        Value::Array(items) => items.iter().for_each(|i| strings(i, out)),
        Value::Object(map) => map.values().for_each(|i| strings(i, out)),
        _ => {}
    }
}

/// Every string under one of `keys`, at any depth of `v`.
fn keyed_strings(v: &Value, keys: &[&str], out: &mut BTreeSet<String>) {
    match v {
        Value::Array(items) => items.iter().for_each(|i| keyed_strings(i, keys, out)),
        Value::Object(map) => {
            for (key, value) in map {
                if keys.contains(&key.as_str()) {
                    strings(value, out);
                } else {
                    keyed_strings(value, keys, out);
                }
            }
        }
        _ => {}
    }
}

// ---- descriptor-relative syscalls ------------------------------------------------------------

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// `openat(dir, name, flags | O_NOFOLLOW | O_CLOEXEC)`, read-only.
fn open_at(dir: RawFd, name: &OsStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    let name = c_name(name)?;
    let flags = flags | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `name` is NUL-terminated and outlives the call; the returned descriptor is new
    // and owned by nobody else.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and is not owned elsewhere.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn lstat_at(dir: RawFd, name: &OsStr) -> io::Result<Stat> {
    stat_at(dir, name).map(|st| Stat::of(&st))
}

/// `(device, inode)`: the same file.
fn file_id(st: &libc::stat) -> (u64, u64) {
    (st.st_dev as u64, st.st_ino)
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
fn stat_at(dir: RawFd, name: &OsStr) -> io::Result<libc::stat> {
    let name = c_name(name)?;
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `name` is NUL-terminated, `st` is valid for writes of a `stat`.
    let rc = unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstatat` succeeded and filled `st`.
    Ok(unsafe { st.assume_init() })
}

fn fstat(fd: RawFd) -> io::Result<Stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `st` is valid for writes of a `stat`.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fstat` succeeded and filled `st`.
    let st = unsafe { st.assume_init() };
    Ok(Stat::of(&st))
}

fn read_link_at(dir: RawFd, name: &OsStr) -> io::Result<Vec<u8>> {
    let name = c_name(name)?;
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: `name` is NUL-terminated, `buf` is valid for writes of `buf.len()` bytes.
    let n = unsafe { libc::readlinkat(dir, name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(n as usize);
    Ok(buf)
}

#[cfg(target_os = "macos")]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: returns this thread's errno slot.
    unsafe { libc::__error() }
}

#[cfg(not(target_os = "macos"))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: returns this thread's errno slot.
    unsafe { libc::__errno_location() }
}

/// The names in the directory `dir` (without `.` and `..`), unsorted.
fn list_dir(dir: &OwnedFd) -> io::Result<Vec<OsString>> {
    // `fdopendir` owns the descriptor it is given: a duplicate, closed by `closedir`.
    let dup = dir.try_clone()?;
    let raw = std::os::fd::IntoRawFd::into_raw_fd(dup);
    // SAFETY: `raw` is an open directory descriptor we own; on success the stream owns it.
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `raw` is still ours to close.
        unsafe { libc::close(raw) };
        return Err(e);
    }
    // A duplicate shares its file offset with `dir` (and with every other duplicate of the
    // root's descriptor): start from the first entry whatever an earlier listing consumed.
    // SAFETY: `stream` is a valid open directory stream.
    unsafe { libc::rewinddir(stream) };
    let mut names = Vec::new();
    let result = loop {
        // SAFETY: errno is thread-local; reset it to tell the end of the stream from an error.
        unsafe { *errno_location() = 0 };
        // SAFETY: `stream` is a valid open directory stream until `closedir` below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let e = io::Error::last_os_error();
            break if e.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(e)
            };
        }
        // SAFETY: `entry` points to a valid dirent whose `d_name` is NUL-terminated, valid
        // until the next `readdir` on this stream.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(OsStr::from_bytes(name).to_owned());
        }
    };
    // SAFETY: `stream` is valid and closed exactly once.
    unsafe { libc::closedir(stream) };
    result.map(|()| names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_of_commands() {
        assert_eq!(
            shell_words(r#"node "tools/mcp.js" --port=3; ./scripts/setup.sh|tee x"#),
            [
                "node",
                "tools/mcp.js",
                "--port",
                "3",
                "./scripts/setup.sh",
                "tee",
                "x"
            ]
        );
        assert_eq!(
            shell_words(r#""$CLAUDE_PROJECT_DIR"/.claude/hooks/a.sh && $(pwd)/b.sh"#),
            ["$CLAUDE_PROJECT_DIR/.claude/hooks/a.sh", "$PWD/b.sh"]
        );
    }

    #[test]
    fn broad_rules() {
        for rule in ["Bash", "Bash(*)", "Bash(:*)", "*", "mcp__evil", "Edit(**)"] {
            assert!(is_broad_rule(rule), "{rule}");
        }
        for rule in ["Bash(npm test:*)", "Read(src/**)", "Bash(git status)", ""] {
            assert!(!is_broad_rule(rule), "{rule}");
        }
    }

    /// Names a folding filesystem may open as a configuration file's: other ASCII case, and
    /// non-ASCII names whose ASCII letters and dots fit; never the name itself, and never a
    /// name that cannot fold into it (another length, other letters, no dot).
    #[test]
    fn names_that_may_fold_to_a_config_name() {
        for (name, target) in [
            ("Settings.json", "settings.json"),
            ("SETTINGS.LOCAL.JSON", "settings.local.json"),
            ("\u{17F}ettings.json", "settings.json"),
            ("settings.jso\u{212A}", "settings.json"),
            ("\u{200C}.claude", ".claude"),
            (".CLAUDE", ".claude"),
            (".mcp.jsön", ".mcp.json"),
        ] {
            assert!(may_fold_to(name.as_bytes(), target), "{name} → {target}");
        }
        for (name, target) in [
            ("settings.json", "settings.json"),
            ("settings.local.json", "settings.json"),
            ("settings.json.bak", "settings.json"),
            ("日本語", ".claude"),
            ("Documentación", ".claude"),
            ("CLAUDE.md", ".claude"),
            ("über.json", "settings.json"),
            ("日本語.md", "settings.json"),
        ] {
            assert!(!may_fold_to(name.as_bytes(), target), "{name} → {target}");
        }
    }

    #[test]
    fn a_walk_has_a_step_budget() {
        let mut budget = Budget::new();
        budget.step(MAX_WALK_STEPS).unwrap();
        let err = budget.step(1).unwrap_err();
        assert_eq!(err.code, atm_types::ErrorCode::Invalid, "{err}");
        let mut late = Budget::new();
        late.deadline = Instant::now() - Duration::from_secs(1);
        assert_eq!(late.step(1).unwrap_err().code, atm_types::ErrorCode::Io);
    }

    #[test]
    fn trees_are_sorted_and_names_checked() {
        let tree = |names: &[&str]| {
            let mut data = Vec::new();
            for name in names {
                data.extend_from_slice(format!("100644 {name}\0").as_bytes());
                data.extend_from_slice(&[7; 20]);
            }
            parse_tree(&data, 20)
        };
        let entries = tree(&["b", "a", ".claude"]).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, [".claude", "a", "b"]);
        assert!(find_entry(&entries, OsStr::new("a")).is_some());
        assert!(find_entry(&entries, OsStr::new("c")).is_none());
        for bad in [&["a", "a"][..], &[".."], &["."], &["a/b"]] {
            assert!(tree(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn normalized_paths() {
        let n = |p: &str| normalize(Path::new(p));
        assert_eq!(
            n("./tools/../tools/x.js"),
            Some(PathBuf::from("tools/x.js"))
        );
        assert_eq!(n("../x"), None);
        assert_eq!(n("."), None);
        assert_eq!(n("a/.."), None);
    }

    /// A commit's paths split as the kernel reads them: a trailing `/` or `/.` is kept (a
    /// file cannot be traversed), an absolute target starts at the (outer) root.
    #[test]
    fn parts_of_paths() {
        let show = |p: &str| {
            parts(p.as_bytes())
                .into_iter()
                .map(|part| match part {
                    Part::Root => "/".to_owned(),
                    Part::Current => ".".to_owned(),
                    Part::Parent => "..".to_owned(),
                    Part::Name(n) => n.to_string_lossy().into_owned(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(show("a//b/./c/.."), ["a", "b", ".", "c", ".."]);
        assert_eq!(show("file/"), ["file", "."]);
        assert_eq!(show("file/."), ["file", "."]);
        assert_eq!(show("/etc/hosts"), ["/", "etc", "hosts"]);
        assert_eq!(show("/"), ["/"]);
    }

    #[test]
    fn tree_objects_and_modes() {
        let mut data = Vec::new();
        for (mode, name, id) in [
            ("100755", "a.sh", 1u8),
            ("40000", "dir", 2),
            ("120000", "l", 3),
        ] {
            data.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            data.extend_from_slice(&[id; 20]);
        }
        let entries = parse_tree(&data, 20).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "a.sh");
        assert_eq!(entries[0].oid, "01".repeat(20));
        assert!(Stat::of_mode(entries[0].mode).executable);
        assert!(Stat::of_mode(entries[1].mode).kind == EntryKind::Dir);
        assert!(Stat::of_mode(entries[2].mode).kind == EntryKind::Link);
        assert!(Stat::of_mode(0o160_000).kind == EntryKind::Dir);
        assert!(!Stat::of_mode(0o100_644).executable);
        assert!(parse_tree(&data[..data.len() - 1], 20).is_err());
    }
}
