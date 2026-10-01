//! Hardened git CLI runner and the worktree / commit / diff / merge operations (spec §8).
//! Owner: M2-GIT; the fingerprint (§8.9, `git/fingerprint.rs`) is M6's, the project overview
//! (`git/overview.rs`) CORE-OVERVIEW's.
//!
//! Locking is the caller's (spec §8.1): Core holds the per-attempt mutex and, for
//! `worktree add/remove`, merge and branch deletion, the per-repo mutex (order attempt → repo).
//! Paths and branch names are passed as UTF-8 (`Invalid` otherwise); every operation that
//! takes paths uses `-z` and `--`, every revision goes after `--end-of-options`.
//!
//! Never run here (spec §8.8): `reset --hard`, `push`, `fetch`, `stash`, `checkout`, a global
//! `worktree prune`, `branch -D` outside `atm/*`, `rm -rf` outside the worktree root.

mod diff;
mod fingerprint;
mod merge;
pub mod overview;
mod parse;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use atm_types::{AppError, BranchList, ErrorCode, WorktreeState};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub use fingerprint::{
    BILLING_ENV_VARS, BILLING_SETTINGS_KEYS, CONFIG_ENTRIES, ConfigSnapshot, MAX_COMMAND_WORDS,
    MAX_CONFIG_BYTES, MAX_CONFIG_DEPTH, MAX_CONFIG_FILES, MAX_LINK_HOPS, MAX_TREE_BYTES,
    MAX_WALK_STEPS, MAX_WALK_TIME, MCP_FILE, RecordDigest, SETTINGS_FILES,
    commit_snapshot_blocking, config_fingerprint_blocking, config_snapshot_blocking,
    differing_paths, is_broad_rule, shell_words,
};
pub use parse::{parse_hunks, parse_worktree_list};

/// `merge-tree --write-tree` (2.38, spec §2.1) and `GIT_NO_LAZY_FETCH` (2.44): without it the
/// lazy fetch of a partial clone runs a command of the repository's configuration
/// (`remote.<name>.uploadpack`, a transport) from any read of a missing object. A [`Git`]
/// found older runs nothing ([`Git::gated`]).
pub const MIN_GIT_VERSION: &str = "2.44";
/// Used when `git` is not on the login-shell PATH.
pub const FALLBACK_GIT: &str = "/usr/bin/git";

/// Timeouts (spec §7.11): default, `worktree add`, diff.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
pub const WORKTREE_ADD_TIMEOUT: Duration = Duration::from_secs(600);
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(120);
/// stdout of one git call is capped (spec §8.1).
pub const MAX_OUTPUT: usize = 64 << 20;

/// Diff budget (spec §8.6): per-file patch → `too_large`; total or file count → `omitted`.
pub const MAX_FILE_PATCH: usize = 256 << 10;
pub const MAX_TOTAL_PATCH: usize = 4 << 20;
pub const MAX_DIFF_FILES: usize = 300;

/// Identity used only for commits on `atm/*` branches when the user has none (spec §8.5).
pub const FALLBACK_USER_NAME: &str = "AI Task Manager";
pub const FALLBACK_USER_EMAIL: &str = "atm@localhost";
/// Commit made before removing a dirty worktree (spec §8.4).
pub const SNAPSHOT_MESSAGE: &str = "atm: wip snapshot";

/// Variables removed from every git and agent environment (spec §7.2), plus every
/// `GIT_CONFIG_KEY_*` / `GIT_CONFIG_VALUE_*`.
pub const SCRUBBED_GIT_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

/// Leading options of every call (spec §8.1). `maintenance.auto=false` is added because
/// `gc.auto=0` alone still lets `commit` and `merge` spawn `git maintenance run --auto`.
const HARDENING: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.quotePath=false",
    "-c",
    "color.ui=never",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
];

/// `git --version` (spec §7.11).
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// Replaces every config-defined merge driver: a failing driver leaves the file conflicted.
const MERGE_DRIVER_OFF: &str = "/usr/bin/false";
/// stderr kept per call (the rest is drained and dropped).
const MAX_STDERR: usize = 64 << 10;
/// Suffixes tried by [`Git::unique_branch`].
const MAX_BRANCH_SUFFIX: u32 = 100;

/// Shared with `claude::ChildEnv`.
pub fn is_scrubbed_git_var(name: &str) -> bool {
    SCRUBBED_GIT_VARS.contains(&name)
        || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
}

/// `command -v git` over the login-shell `PATH` (spec §8.1), else [`FALLBACK_GIT`].
/// Relative `PATH` entries are skipped.
pub fn find_git(path_var: &OsStr) -> PathBuf {
    std::env::split_paths(path_var)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|git| {
            git.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .unwrap_or_else(|| PathBuf::from(FALLBACK_GIT))
}

/// Options of one git call.
#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    /// `None` = [`DEFAULT_TIMEOUT`].
    pub timeout: Option<Duration>,
    /// Adds `GIT_OPTIONAL_LOCKS=0` so reads never contend `index.lock` with the agent.
    pub read_only: bool,
    /// `GIT_INDEX_FILE` for the snapshot-tree diff (spec §8.6).
    pub index_file: Option<PathBuf>,
}

impl RunOpts {
    fn read() -> RunOpts {
        RunOpts {
            read_only: true,
            ..RunOpts::default()
        }
    }
}

/// Raw result of a git call that ran to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Exit code (`-1` if killed by a signal).
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitOutput {
    /// stdout as lossy UTF-8 without the trailing newline.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim_end().to_owned()
    }

    /// stdout as a path, without its single trailing newline (names may end in spaces).
    fn path(&self) -> PathBuf {
        let out = self.stdout.strip_suffix(b"\n").unwrap_or(&self.stdout);
        PathBuf::from(OsStr::from_bytes(out))
    }
}

/// Result of [`Git::validate_repo`] (spec §8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// Canonical `rev-parse --show-toplevel` of the main checkout.
    pub toplevel: PathBuf,
    /// Checked-out branch (short name), `None` if detached: the default target.
    pub current_branch: Option<String>,
    /// Non-blocking (Italian), e.g. `.claude/` or `.mcp.json` present.
    pub warnings: Vec<String>,
}

/// One record of `worktree list --porcelain -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Short branch name (`refs/heads/` stripped).
    pub branch: Option<String>,
    pub locked: bool,
    pub prunable: bool,
}

/// Commit created by [`Git::autocommit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCommit {
    pub commit: String,
    /// Paths in `status --porcelain` before the commit.
    pub files: u32,
}

/// The git binary plus the environment every call runs with (spec §8.1).
#[derive(Debug, Clone)]
pub struct Git {
    bin: PathBuf,
    path_var: OsString,
    extra_env: Vec<(OsString, OsString)>,
    /// Set by [`Git::gated`] for a git older than [`MIN_GIT_VERSION`]: why every call fails.
    refused: Option<String>,
}

impl Git {
    /// `path_var` becomes `PATH` of every call.
    pub fn new(bin: PathBuf, path_var: OsString) -> Git {
        Git {
            bin,
            path_var,
            extra_env: Vec::new(),
            refused: None,
        }
    }

    /// This runner, refusing every call (`Git`, with the reason) but [`Git::version`] when git
    /// answers `--version` with a version older than [`MIN_GIT_VERSION`]: such a git ignores
    /// `GIT_NO_LAZY_FETCH` (spec §8.1). A git that does not answer is left alone (its calls
    /// fail on their own).
    pub async fn gated(mut self) -> Git {
        if let Ok((version, found)) = self.found_version().await
            && Some(found) < parse::major_minor(MIN_GIT_VERSION)
        {
            self.refused = Some(too_old(&version));
        }
        self
    }

    /// Adds `vars` to the inherited environment of every call, before the scrub and the
    /// forced variables (`CoreConfig::extra_env`; tests: a hostile `GIT_CONFIG_GLOBAL`,
    /// `HOME`, `LANG=it_IT.UTF-8`).
    pub fn with_env(mut self, vars: Vec<(OsString, OsString)>) -> Git {
        self.extra_env = vars;
        self
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// `git --version` → `"2.54.0"`. Errors: `Git` if missing or older than [`MIN_GIT_VERSION`].
    pub async fn version(&self) -> Result<String, AppError> {
        let (version, found) = self.found_version().await?;
        if Some(found) < parse::major_minor(MIN_GIT_VERSION) {
            return Err(AppError::git(too_old(&version)));
        }
        Ok(version)
    }

    /// `git --version`, parsed: the version and its major and minor numbers.
    async fn found_version(&self) -> Result<(String, (u32, u32)), AppError> {
        let opts = RunOpts {
            timeout: Some(VERSION_TIMEOUT),
            ..RunOpts::read()
        };
        let (out, _) = self
            .spawn(None, &["--version"], &opts, &[], MAX_OUTPUT)
            .await?;
        let text = out.text();
        parse::parse_version(&text)
            .filter(|_| out.code == 0)
            .ok_or_else(|| {
                AppError::git(format!(
                    "{} non risponde come git: {text}",
                    self.bin.display()
                ))
            })
    }

    /// Runs `git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c core.quotePath=false
    /// -c color.ui=never -c gc.auto=0 -c maintenance.auto=false -C <dir> <args>` with stdin
    /// null, `kill_on_drop`, the scrubbed environment plus `LC_ALL=C LANGUAGE=C
    /// GIT_TERMINAL_PROMPT=0 GIT_EDITOR=true GIT_PAGER=cat GIT_NO_LAZY_FETCH=1` (spec §8.1;
    /// the last: a partial clone never fetches a missing object, which would run the
    /// configuration's transport and `uploadpack` commands). Unless the call is
    /// `read_only` without an `index_file` (it cannot write refs or an index) and is not
    /// `merge-tree`, the commands defined in the configuration that these flags do not cover
    /// are listed first and disabled: hooks (`hook.<name>.command`) and merge drivers
    /// (`merge.<name>.driver`). Clean/smudge filters stay on (git-lfs needs them). A non-zero
    /// exit is returned in `code`. Errors: `Git` on spawn failure, timeout (the process is
    /// killed), stdout over [`MAX_OUTPUT`] or a git older than [`MIN_GIT_VERSION`]
    /// ([`Git::gated`]).
    pub async fn run(
        &self,
        dir: &Path,
        args: &[&str],
        opts: &RunOpts,
    ) -> Result<GitOutput, AppError> {
        let (out, overflow) = self.exec(dir, args, opts, MAX_OUTPUT).await?;
        if overflow {
            return Err(AppError::git(format!(
                "git {}: output oltre {} MiB",
                subcommand(args),
                MAX_OUTPUT >> 20
            )));
        }
        Ok(out)
    }

    /// [`Git::run`] that turns a non-zero exit into `Git` with the stderr text.
    pub async fn run_ok(
        &self,
        dir: &Path,
        args: &[&str],
        opts: &RunOpts,
    ) -> Result<GitOutput, AppError> {
        let out = self.run(dir, args, opts).await?;
        if out.code == 0 {
            Ok(out)
        } else {
            Err(failure(args, &out))
        }
    }

    /// Checks a folder for `add_project` (spec §8.3): toplevel (canonical), not bare, at least
    /// one commit, main checkout (`--git-dir` = `--git-common-dir`), not inside
    /// `worktree_root` (nor containing it). Errors: `Invalid` with an Italian message for each
    /// failed rule.
    pub async fn validate_repo(
        &self,
        path: &Path,
        worktree_root: &Path,
    ) -> Result<RepoInfo, AppError> {
        let read = RunOpts::read();
        let bare = self
            .run(path, &["rev-parse", "--is-bare-repository"], &read)
            .await?;
        if bare.code != 0 {
            return Err(AppError::invalid("La cartella non è un repository git"));
        }
        if bare.text() == "true" {
            return Err(AppError::invalid(
                "Il repository è bare: serve un checkout con i file",
            ));
        }
        let top = self
            .run(path, &["rev-parse", "--show-toplevel"], &read)
            .await?;
        if top.code != 0 {
            return Err(AppError::invalid(
                "La cartella non è dentro il checkout di un repository git",
            ));
        }
        let toplevel = canonical(&top.path())?;
        let head = self
            .run(&toplevel, &["rev-parse", "--verify", "-q", "HEAD"], &read)
            .await?;
        if head.code != 0 {
            return Err(AppError::invalid(
                "Il repository non ha ancora commit: creane almeno uno",
            ));
        }
        let dirs = self
            .run_ok(
                &toplevel,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-dir",
                    "--git-common-dir",
                ],
                &read,
            )
            .await?;
        let dirs = dirs
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| canonical(Path::new(OsStr::from_bytes(line))))
            .collect::<Result<Vec<_>, _>>()?;
        if dirs.len() != 2 || dirs[0] != dirs[1] {
            return Err(AppError::invalid(
                "La cartella è un worktree collegato: aggiungi il checkout principale del repository",
            ));
        }
        let root = canonical(worktree_root).unwrap_or_else(|_| worktree_root.to_path_buf());
        if toplevel.starts_with(&root) {
            return Err(AppError::invalid(
                "Il repository si trova dentro la cartella dei worktree",
            ));
        }
        if root.starts_with(&toplevel) {
            return Err(AppError::invalid(
                "La cartella dei worktree si trova dentro il repository",
            ));
        }
        let mut found = Vec::new();
        if toplevel.join(".claude").is_dir() {
            found.push(".claude/");
        }
        if toplevel.join(".mcp.json").exists() {
            found.push(".mcp.json");
        }
        let warnings = if found.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "Il progetto contiene {}: non verranno caricati finché il progetto è Isolato",
                found.join(" e ")
            )]
        };
        Ok(RepoInfo {
            current_branch: self.current_branch(&toplevel).await?,
            toplevel,
            warnings,
        })
    }

    /// Local branches (`for-each-ref refs/heads`) and the checked-out one.
    pub async fn list_branches(&self, repo: &Path) -> Result<BranchList, AppError> {
        let out = self
            .run_ok(
                repo,
                &["for-each-ref", "--format=%(refname)", "refs/heads"],
                &RunOpts::read(),
            )
            .await?;
        let branches = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("refs/heads/"))
            .map(str::to_owned)
            .collect();
        Ok(BranchList {
            current: self.current_branch(repo).await?,
            branches,
        })
    }

    /// `rev-parse --verify --end-of-options refs/heads/<branch>^{commit}`.
    /// Errors: `NotFound` if the branch does not exist.
    pub async fn branch_tip(&self, repo: &Path, branch: &str) -> Result<String, AppError> {
        check_branch(branch)?;
        let rev = format!("refs/heads/{branch}^{{commit}}");
        let args = ["rev-parse", "--verify", "-q", "--end-of-options", &rev];
        let out = self.run(repo, &args, &RunOpts::read()).await?;
        match out.code {
            0 => Ok(out.text()),
            1 => Err(AppError::not_found(format!(
                "Il branch {branch} non esiste"
            ))),
            _ => Err(failure(&args, &out)),
        }
    }

    /// [`branch_name`] validated with `check-ref-format --branch`; on collision
    /// (`show-ref --verify`) appends `-2`, `-3`, … (spec §8.2).
    pub async fn unique_branch(
        &self,
        repo: &Path,
        attempt_id: &str,
        title: &str,
    ) -> Result<String, AppError> {
        let base = branch_name(attempt_id, title);
        let check = self
            .run(
                repo,
                &["check-ref-format", "--branch", &base],
                &RunOpts::read(),
            )
            .await?;
        if check.code != 0 {
            return Err(AppError::invalid(format!(
                "Nome di branch non valido: {base}"
            )));
        }
        for n in 1..=MAX_BRANCH_SUFFIX {
            let name = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            if !self.branch_exists(repo, &name).await? {
                return Ok(name);
            }
        }
        Err(AppError::conflict(format!(
            "Esistono già troppi branch {base}-N"
        )))
    }

    /// `worktree add --lock --reason "atm:<attempt_id>" -b <branch> <worktree> <base>` and
    /// returns the canonical path (spec §8.4). The path and the branch must not exist yet
    /// (`Conflict`). On failure removes only that worktree (`rm -rf` only for
    /// `<canonical root>/<attempt_id>`) and retries once. Timeout 600 s.
    pub async fn add_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        base: &str,
        attempt_id: &str,
    ) -> Result<PathBuf, AppError> {
        check_branch(branch)?;
        check_rev(base)?;
        let path = utf8(worktree)?;
        if worktree.symlink_metadata().is_ok() {
            return Err(AppError::conflict(format!(
                "Il percorso del worktree esiste già: {path}"
            )));
        }
        if self.branch_exists(repo, branch).await? {
            return Err(AppError::conflict(format!("Il branch {branch} esiste già")));
        }
        let reason = format!("atm:{attempt_id}");
        let opts = RunOpts {
            timeout: Some(WORKTREE_ADD_TIMEOUT),
            ..RunOpts::default()
        };
        let mut failed = None;
        for _ in 0..2 {
            let mut args = vec!["worktree", "add", "--lock", "--reason", &reason];
            // A failed first try may have created the branch already.
            if self.branch_exists(repo, branch).await? {
                args.extend(["--", path, branch]);
            } else {
                args.extend(["-b", branch, "--", path, base]);
            }
            match self.run_ok(repo, &args, &opts).await {
                Ok(_) => return canonical(worktree),
                Err(e) => {
                    self.clean_failed_worktree(repo, worktree, attempt_id).await;
                    failed = Some(e);
                }
            }
        }
        Err(failed.unwrap_or_else(|| AppError::internal("worktree add")))
    }

    /// Best effort after a failed `worktree add` of a path that did not exist before.
    async fn clean_failed_worktree(&self, repo: &Path, worktree: &Path, attempt_id: &str) {
        if let Some(path) = worktree.to_str() {
            let opts = RunOpts::default();
            let _ = self
                .run(repo, &["worktree", "unlock", "--", path], &opts)
                .await;
            let _ = self
                .run(repo, &["worktree", "remove", "--force", "--", path], &opts)
                .await;
        }
        let (Some(parent), Ok(real)) = (worktree.parent(), worktree.canonicalize()) else {
            return;
        };
        let Ok(root) = parent.canonicalize() else {
            return;
        };
        let under_root = real.parent() == Some(root.as_path())
            && real.file_name() == Some(OsStr::new(attempt_id))
            && !repo.starts_with(&real);
        if under_root {
            let _ = tokio::fs::remove_dir_all(&real).await;
        }
    }

    /// Removal (spec §8.4): snapshot commit if dirty, `worktree unlock`, `worktree remove
    /// --force`; if the directory is already gone, deletes only its metadata dir (matched by
    /// `gitdir`). Never a global `worktree prune`; the branch is kept. The snapshot goes only
    /// on an `atm/*` branch: on a detached HEAD it would be reachable from no ref once git
    /// drops the worktree's HEAD reflog, and other branches are not ours to commit to.
    /// Errors: `WorktreeMissing` if the directory is no longer a git checkout,
    /// `BranchMismatch` if HEAD is not on an `atm/*` branch and holds work no ref keeps
    /// (uncommitted changes, or commits only HEAD reaches).
    pub async fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), AppError> {
        let path = utf8(worktree)?;
        if !worktree.is_dir() {
            return self.remove_worktree_metadata(repo, worktree).await;
        }
        self.check_worktree(worktree).await?;
        let on_atm = self
            .head_ref(worktree)
            .await?
            .is_some_and(|r| r.starts_with("refs/heads/atm/"));
        if on_atm {
            self.commit_all(worktree, SNAPSHOT_MESSAGE).await?;
        } else if !self.status(worktree).await?.is_empty() || !self.head_in_a_ref(worktree).await? {
            return Err(AppError::new(
                ErrorCode::BranchMismatch,
                format!(
                    "Il worktree {path} non è su un branch atm/… e ha lavoro che nessun branch \
                     conserva: torna sul branch dell'attempt o crea un branch, poi riprova"
                ),
            ));
        }
        let opts = RunOpts::default();
        // Fails harmlessly when not locked; `remove` reports the real problems.
        self.run(repo, &["worktree", "unlock", "--", path], &opts)
            .await?;
        self.run_ok(repo, &["worktree", "remove", "--force", "--", path], &opts)
            .await?;
        Ok(())
    }

    async fn remove_worktree_metadata(&self, repo: &Path, worktree: &Path) -> Result<(), AppError> {
        let common = self.common_dir(repo).await?;
        let wanted = [
            worktree.join(".git"),
            canonical_parent(worktree).join(".git"),
        ];
        let Ok(admin_dirs) = std::fs::read_dir(common.join("worktrees")) else {
            return Ok(());
        };
        for admin in admin_dirs.flatten() {
            let Ok(gitdir) = std::fs::read(admin.path().join("gitdir")) else {
                continue;
            };
            let gitdir = gitdir.strip_suffix(b"\n").unwrap_or(&gitdir);
            if wanted.iter().any(|w| w.as_os_str().as_bytes() == gitdir) {
                tokio::fs::remove_dir_all(admin.path()).await?;
            }
        }
        Ok(())
    }

    pub async fn list_worktrees(&self, repo: &Path) -> Result<Vec<WorktreeEntry>, AppError> {
        let out = self
            .run_ok(
                repo,
                &["worktree", "list", "--porcelain", "-z"],
                &RunOpts::read(),
            )
            .await?;
        Ok(parse_worktree_list(&out.stdout))
    }

    /// State of each of `worktrees` (same order): `Present` if listed by git and the
    /// directory exists, else `Missing`. Deletes nothing (spec §8.4).
    pub async fn reconcile_worktrees(
        &self,
        repo: &Path,
        worktrees: &[PathBuf],
    ) -> Result<Vec<WorktreeState>, AppError> {
        let listed: Vec<PathBuf> = self
            .list_worktrees(repo)
            .await?
            .into_iter()
            .map(|entry| entry.path.canonicalize().unwrap_or(entry.path))
            .collect();
        Ok(worktrees
            .iter()
            .map(|path| match path.canonicalize() {
                Ok(real) if real.is_dir() && listed.contains(&real) => WorktreeState::Present,
                _ => WorktreeState::Missing,
            })
            .collect())
    }

    /// Auto-commit (spec §8.5): if `status --porcelain=v1 -z --untracked-files=all` is not
    /// empty, `add -A` then `commit --no-verify --no-gpg-sign -q -m <message>`, with the
    /// fallback identity when `user.name`/`user.email` are missing and HEAD is an `atm/*`
    /// branch. `None` if the worktree was clean (or nothing could be staged).
    /// Errors: `WorktreeMissing` if the directory is no longer a git checkout.
    pub async fn autocommit(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<AutoCommit>, AppError> {
        self.check_worktree(worktree).await?;
        self.commit_all(worktree, message).await
    }

    /// [`Git::autocommit`] of a worktree already checked by [`Git::check_worktree`].
    async fn commit_all(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<AutoCommit>, AppError> {
        let read = RunOpts::read();
        let files = parse::status_entries(&self.status(worktree).await?).len() as u32;
        if files == 0 {
            return Ok(None);
        }
        let write = RunOpts::default();
        self.run_ok(worktree, &["add", "-A"], &write).await?;
        // A dirty submodule shows in `status` but stages nothing.
        let staged = [
            "diff",
            "--cached",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
        ];
        let staged_out = self.run(worktree, &staged, &read).await?;
        match staged_out.code {
            0 => return Ok(None),
            1 => {}
            _ => return Err(failure(&staged, &staged_out)),
        }
        let fallback = !self.has_identity(worktree).await?
            && self
                .head_ref(worktree)
                .await?
                .is_some_and(|r| r.starts_with("refs/heads/atm/"));
        let name = format!("user.name={FALLBACK_USER_NAME}");
        let email = format!("user.email={FALLBACK_USER_EMAIL}");
        let mut args = Vec::new();
        if fallback {
            args.extend(["-c", &name, "-c", &email]);
        }
        args.extend([
            "commit",
            "--no-verify",
            "--no-gpg-sign",
            "-q",
            "-m",
            message,
        ]);
        self.run_ok(worktree, &args, &write).await?;
        Ok(Some(AutoCommit {
            commit: self.head(worktree).await?,
            files,
        }))
    }

    /// `rev-parse HEAD`.
    pub async fn head(&self, worktree: &Path) -> Result<String, AppError> {
        let out = self
            .run_ok(
                worktree,
                &["rev-parse", "--verify", "HEAD"],
                &RunOpts::read(),
            )
            .await?;
        Ok(out.text())
    }

    /// `symbolic-ref -q HEAD` (full ref), `None` if detached.
    pub async fn head_ref(&self, worktree: &Path) -> Result<Option<String>, AppError> {
        let args = ["symbolic-ref", "-q", "HEAD"];
        let out = self.run(worktree, &args, &RunOpts::read()).await?;
        match out.code {
            0 => Ok(Some(out.text())),
            1 => Ok(None),
            _ => Err(failure(&args, &out)),
        }
    }

    /// `log --oneline <base>..HEAD`, for the `fresh_session` prompt (spec §7.4).
    pub async fn log_oneline(&self, worktree: &Path, base: &str) -> Result<String, AppError> {
        check_rev(base)?;
        let range = format!("{base}..HEAD");
        let args = [
            "log",
            "--oneline",
            "--no-color",
            "--no-decorate",
            "--no-show-signature",
            "--end-of-options",
            &range,
        ];
        Ok(self.run_ok(worktree, &args, &RunOpts::read()).await?.text())
    }

    /// `branch -D <branch>`; `Invalid` unless it starts with `atm/`.
    pub async fn delete_branch(&self, repo: &Path, branch: &str) -> Result<(), AppError> {
        if !branch.starts_with("atm/") || check_branch(branch).is_err() {
            return Err(AppError::invalid(format!(
                "Si possono cancellare solo i branch atm/…, non {branch}"
            )));
        }
        self.run_ok(repo, &["branch", "-D", branch], &RunOpts::default())
            .await?;
        Ok(())
    }

    async fn current_branch(&self, repo: &Path) -> Result<Option<String>, AppError> {
        Ok(self
            .head_ref(repo)
            .await?
            .and_then(|r| r.strip_prefix("refs/heads/").map(str::to_owned)))
    }

    /// `worktree` is the top level of its own checkout. Without its `.git` git would search the
    /// parent directories and act on an enclosing repository (e.g. a `$HOME` under git).
    /// Errors: `WorktreeMissing`.
    async fn check_worktree(&self, worktree: &Path) -> Result<(), AppError> {
        let out = self
            .run(
                worktree,
                &["rev-parse", "--show-toplevel"],
                &RunOpts::read(),
            )
            .await?;
        // Compared by inode: the paths may differ in symlinks or Unicode normalization.
        let same = out.code == 0
            && match (out.path().metadata(), worktree.metadata()) {
                (Ok(top), Ok(wt)) => (top.dev(), top.ino()) == (wt.dev(), wt.ino()),
                _ => false,
            };
        if same {
            return Ok(());
        }
        Err(AppError::new(
            ErrorCode::WorktreeMissing,
            format!(
                "Il worktree {} non esiste o non è più un checkout git (manca il suo .git)",
                worktree.display()
            ),
        ))
    }

    /// `status --porcelain=v1 -z --untracked-files=all`.
    async fn status(&self, dir: &Path) -> Result<Vec<u8>, AppError> {
        let args = ["status", "--porcelain=v1", "-z", "--untracked-files=all"];
        Ok(self.run_ok(dir, &args, &RunOpts::read()).await?.stdout)
    }

    /// Nothing to commit in `dir` (`status --porcelain`, untracked files included, ignored
    /// ones not).
    pub async fn is_clean(&self, dir: &Path) -> Result<bool, AppError> {
        Ok(self.status(dir).await?.is_empty())
    }

    /// Some ref reaches the HEAD of `worktree`.
    async fn head_in_a_ref(&self, worktree: &Path) -> Result<bool, AppError> {
        let args = [
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "--contains",
            "HEAD",
        ];
        let out = self.run_ok(worktree, &args, &RunOpts::read()).await?;
        Ok(!out.stdout.is_empty())
    }

    /// Absolute `--git-common-dir` of `repo`.
    async fn common_dir(&self, repo: &Path) -> Result<PathBuf, AppError> {
        let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
        Ok(self.run_ok(repo, &args, &RunOpts::read()).await?.path())
    }

    async fn branch_exists(&self, repo: &Path, branch: &str) -> Result<bool, AppError> {
        let full = format!("refs/heads/{branch}");
        let args = ["show-ref", "--verify", "-q", &full];
        let out = self.run(repo, &args, &RunOpts::read()).await?;
        match out.code {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(failure(&args, &out)),
        }
    }

    /// `user.name` and `user.email` are both configured (non-empty) for `dir`.
    async fn has_identity(&self, dir: &Path) -> Result<bool, AppError> {
        let args = ["config", "--null", "--get-regexp", r"^user\.(name|email)$"];
        let out = self.run(dir, &args, &RunOpts::read()).await?;
        if out.code > 1 {
            return Err(failure(&args, &out));
        }
        let keys: BTreeSet<&[u8]> = out
            .stdout
            .split(|b| *b == 0)
            .filter_map(|record| {
                let i = record.iter().position(|b| *b == b'\n')?;
                (!record[i + 1..].trim_ascii().is_empty()).then_some(&record[..i])
            })
            .collect();
        Ok(keys.contains(&b"user.name"[..]) && keys.contains(&b"user.email"[..]))
    }

    /// [`Git::run`] with its own stdout cap: `(output, true)` when it was exceeded (the
    /// process is killed).
    async fn exec(
        &self,
        dir: &Path,
        args: &[&str],
        opts: &RunOpts,
        limit: usize,
    ) -> Result<(GitOutput, bool), AppError> {
        self.check_version()?;
        // Hooks, filters and signing run only on writes, merge drivers only inside
        // `merge-tree`; `status` runs the clean filter of a file whose stat data changed.
        let reads_config = matches!(subcommand(args), "merge-tree" | "status");
        let overrides = if opts.read_only && opts.index_file.is_none() && !reads_config {
            Vec::new()
        } else {
            self.config_overrides(dir, opts.timeout).await?
        };
        self.spawn(Some(dir), args, opts, &overrides, limit).await
    }

    /// Errors: `Git` when [`Git::gated`] found git too old.
    fn check_version(&self) -> Result<(), AppError> {
        match &self.refused {
            Some(why) => Err(AppError::git(why.clone())),
            None => Ok(()),
        }
    }

    /// [`config_overrides_from`] the configuration seen from `dir`, within the call's `timeout`.
    async fn config_overrides(
        &self,
        dir: &Path,
        timeout: Option<Duration>,
    ) -> Result<Vec<(OsString, OsString)>, AppError> {
        let args = [
            "config",
            "--show-scope",
            "--null",
            "--get-regexp",
            r"^(hook|merge|filter|gpg)\.",
        ];
        let opts = RunOpts {
            timeout,
            ..RunOpts::read()
        };
        let (out, overflow) = self.spawn(Some(dir), &args, &opts, &[], MAX_OUTPUT).await?;
        if overflow {
            return Err(AppError::git(format!(
                "git config: output oltre {} MiB",
                MAX_OUTPUT >> 20
            )));
        }
        match out.code {
            0 => Ok(config_overrides_from(&out.stdout)),
            1 => Ok(Vec::new()),
            _ => Err(failure(&args, &out)),
        }
    }

    /// The inherited environment plus `extra_env`, scrubbed, then the forced variables and
    /// the config `overrides` (through `GIT_CONFIG_COUNT`, which, unlike `-c`, accepts any
    /// subsection name).
    fn env(
        &self,
        opts: &RunOpts,
        overrides: &[(OsString, OsString)],
    ) -> BTreeMap<OsString, OsString> {
        let mut env: BTreeMap<OsString, OsString> = std::env::vars_os()
            .chain(self.extra_env.iter().cloned())
            .collect();
        // Nor the API keys or a parent Claude Code session's variables, and `NODE_OPTIONS`
        // as the user had it before cmux (spec §7.2): git runs filters and helpers from the
        // repository's configuration.
        crate::claude::scrub_host_env(&mut env);
        env.retain(|key, _| {
            !key.to_str()
                .is_some_and(|k| is_scrubbed_git_var(k) || crate::claude::API_KEY_VARS.contains(&k))
        });
        let mut forced = vec![
            ("PATH".to_owned(), self.path_var.clone()),
            ("LC_ALL".to_owned(), "C".into()),
            ("LANGUAGE".to_owned(), "C".into()),
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".into()),
            ("GIT_EDITOR".to_owned(), "true".into()),
            ("GIT_PAGER".to_owned(), "cat".into()),
            // The app never fetches: a partial clone's missing object is an error, not a
            // fetch from the promisor remote that the repository's configuration describes.
            ("GIT_NO_LAZY_FETCH".to_owned(), "1".into()),
        ];
        if opts.read_only {
            forced.push(("GIT_OPTIONAL_LOCKS".to_owned(), "0".into()));
        }
        if let Some(index) = &opts.index_file {
            forced.push(("GIT_INDEX_FILE".to_owned(), index.into()));
        }
        if !overrides.is_empty() {
            forced.push((
                "GIT_CONFIG_COUNT".to_owned(),
                overrides.len().to_string().into(),
            ));
        }
        for (i, (key, value)) in overrides.iter().enumerate() {
            forced.push((format!("GIT_CONFIG_KEY_{i}"), key.clone()));
            forced.push((format!("GIT_CONFIG_VALUE_{i}"), value.clone()));
        }
        env.extend(forced.into_iter().map(|(k, v)| (k.into(), v)));
        env
    }

    async fn spawn(
        &self,
        dir: Option<&Path>,
        args: &[&str],
        opts: &RunOpts,
        overrides: &[(OsString, OsString)],
        limit: usize,
    ) -> Result<(GitOutput, bool), AppError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(HARDENING);
        if let Some(dir) = dir {
            cmd.arg("-C").arg(dir);
        }
        cmd.args(args)
            .env_clear()
            .envs(self.env(opts, overrides))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let sub = subcommand(args);
        let mut child = cmd.spawn().map_err(|e| {
            AppError::git(format!("impossibile avviare {}: {e}", self.bin.display()))
        })?;
        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take())
        else {
            return Err(AppError::internal("git: pipe non disponibili"));
        };
        let timeout = opts.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let io = async {
            tokio::try_join!(
                read_capped(&mut stdout, limit),
                read_stderr(&mut stderr),
                child.wait()
            )
        };
        // On timeout or overflow `child` is dropped on return: `kill_on_drop` stops it.
        match tokio::time::timeout(timeout, io).await {
            Ok(Ok((stdout, stderr, status))) => Ok((
                GitOutput {
                    code: status.code().unwrap_or(-1),
                    stdout,
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                },
                false,
            )),
            Ok(Err(e)) if e.kind() == io::ErrorKind::FileTooLarge => Ok((
                GitOutput {
                    code: -1,
                    stdout: Vec::new(),
                    stderr: String::new(),
                },
                true,
            )),
            Ok(Err(e)) => Err(AppError::git(format!("git {sub}: {e}"))),
            Err(_) => Err(AppError::git(format!(
                "git {sub}: nessuna risposta entro {timeout:?}"
            ))),
        }
    }
}

/// Longest header line of `cat-file --batch` read (`<id> <type> <size>`, or the request and
/// `missing`).
const MAX_CAT_HEADER: u64 = 4096;
/// [`Git::commit_config_snapshot`] as a whole: past the walk's own deadline and the reader's.
pub const COMMIT_CONFIG_TIMEOUT: Duration = Duration::from_secs(70);

/// `git cat-file --batch` in a repository (spec §8.9, the approval of a commit): objects read
/// one at a time through stdin and stdout, all within one [`DEFAULT_TIMEOUT`]; killed when
/// dropped.
pub struct CatFile {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    deadline: tokio::time::Instant,
    /// An object was left unread (too large) or a read failed: the stream is out of step.
    broken: bool,
}

/// One answer of [`CatFile::get`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatObject {
    /// No such object (or an ambiguous name).
    Missing,
    /// Larger than the limit asked for: not read, and the reader cannot be used any more.
    TooLarge(u64),
    Found {
        /// Hex id.
        oid: String,
        /// `blob`, `tree`, `commit`, `tag`.
        kind: String,
        data: Vec<u8>,
    },
}

impl Git {
    /// Starts `git cat-file --batch` in `repo` with the runner's flags and environment (spec
    /// §8.1), read only, and without the lazy fetch of a partial clone (`GIT_NO_LAZY_FETCH`,
    /// as every call). Errors: `Git` on spawn failure or a git too old ([`Git::gated`]).
    pub async fn cat_file(&self, repo: &Path) -> Result<CatFile, AppError> {
        self.check_version()?;
        let env = self.env(&RunOpts::read(), &[]);
        let mut cmd = Command::new(&self.bin);
        cmd.args(HARDENING)
            .arg("-C")
            .arg(repo)
            .args(["cat-file", "--batch"])
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| {
            AppError::git(format!("impossibile avviare {}: {e}", self.bin.display()))
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(AppError::internal("git cat-file: pipe non disponibili"));
        };
        Ok(CatFile {
            _child: child,
            stdin,
            stdout: BufReader::new(stdout),
            deadline: tokio::time::Instant::now() + DEFAULT_TIMEOUT,
            broken: false,
        })
    }

    /// M6 (spec §8.9): the Claude configuration of `commit` in `repo`, as a checkout of it
    /// has it on disk (same records, same fingerprint as [`config_snapshot`] of a clean
    /// worktree of it), read from the object database: what the Trusted approval approves.
    /// Errors: `Invalid` (a bad revision, a limit, a link out of the tree, a name another
    /// filesystem could fold into a configuration file's), `NotFound` (no such commit), `Io`
    /// (the walk passed [`MAX_WALK_TIME`]), `Git` (the reader failed or did not answer within
    /// [`COMMIT_CONFIG_TIMEOUT`]).
    pub async fn commit_config_snapshot(
        &self,
        repo: &Path,
        commit: &str,
    ) -> Result<ConfigSnapshot, AppError> {
        check_rev(commit)?;
        let mut cat = self.cat_file(repo).await?;
        let rt = tokio::runtime::Handle::current();
        let commit = commit.to_owned();
        // The walk is blocking code (shared with the checkout's); its reads run on `rt`. The
        // reader comes back to be dropped (killed) here, in the runtime. The walk ends on its
        // own within its deadline and the reader's; this bounds the wait even so.
        let walk = tokio::task::spawn_blocking(move || {
            let snapshot = commit_snapshot_blocking(&rt, &mut cat, &commit);
            (snapshot, cat)
        });
        let (snapshot, _cat) = tokio::time::timeout(COMMIT_CONFIG_TIMEOUT, walk)
            .await
            .map_err(|_| {
                AppError::git(format!(
                    "configurazione del commit: nessuna risposta entro {COMMIT_CONFIG_TIMEOUT:?}"
                ))
            })?
            .map_err(|e| AppError::internal(format!("configurazione del commit: {e}")))?;
        snapshot
    }
}

impl CatFile {
    /// The object `spec` names (an id, `<commit>^{tree}`, …), read if at most `max` bytes.
    /// Errors: `Invalid` (a name with a line break), `Git` (the reader failed, was out of step
    /// or passed the deadline).
    pub async fn get(&mut self, spec: &str, max: u64) -> Result<CatObject, AppError> {
        if spec.is_empty() || spec.contains(['\n', '\r', '\0']) {
            return Err(AppError::invalid(format!(
                "Nome di oggetto non valido: {spec:?}"
            )));
        }
        if self.broken {
            return Err(AppError::git("git cat-file: lettore non più utilizzabile"));
        }
        self.broken = true;
        let read = tokio::time::timeout_at(self.deadline, self.read_object(spec, max)).await;
        let object = match read {
            Ok(Ok(object)) => object,
            Ok(Err(e)) => return Err(AppError::git(format!("git cat-file: {e}"))),
            Err(_) => {
                return Err(AppError::git(format!(
                    "git cat-file: nessuna risposta entro {DEFAULT_TIMEOUT:?}"
                )));
            }
        };
        self.broken = matches!(object, CatObject::TooLarge(_));
        Ok(object)
    }

    async fn read_object(&mut self, spec: &str, max: u64) -> io::Result<CatObject> {
        self.stdin.write_all(format!("{spec}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        let mut header = Vec::new();
        (&mut self.stdout)
            .take(MAX_CAT_HEADER)
            .read_until(b'\n', &mut header)
            .await?;
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "risposta non valida");
        let header =
            std::str::from_utf8(header.strip_suffix(b"\n").ok_or_else(bad)?).map_err(|_| bad())?;
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            return Ok(CatObject::Missing);
        }
        let mut fields = header.split(' ');
        let (Some(oid), Some(kind), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(bad());
        };
        let size: u64 = size.parse().map_err(|_| bad())?;
        if size > max {
            return Ok(CatObject::TooLarge(size));
        }
        let mut data = vec![0; usize::try_from(size).map_err(|_| bad())?];
        self.stdout.read_exact(&mut data).await?;
        let mut end = [0u8];
        self.stdout.read_exact(&mut end).await?;
        if end != *b"\n" {
            return Err(bad());
        }
        Ok(CatObject::Found {
            oid: oid.to_owned(),
            kind: kind.to_owned(),
            data,
        })
    }
}

/// `FileTooLarge` past `limit` bytes.
async fn read_capped(r: &mut (impl AsyncRead + Unpin), limit: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    r.take(limit as u64 + 1).read_to_end(&mut buf).await?;
    if buf.len() > limit {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    Ok(buf)
}

/// Keeps the first [`MAX_STDERR`] bytes and drains the rest so git never blocks on the pipe.
async fn read_stderr(r: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    (&mut *r)
        .take(MAX_STDERR as u64)
        .read_to_end(&mut buf)
        .await?;
    tokio::io::copy(r, &mut tokio::io::sink()).await?;
    Ok(buf)
}

/// The git command in `args`, skipping global options (`-c <value>`, `--literal-pathspecs`).
/// Filter commands that `git lfs install --local` writes: kept, like any filter of the user's
/// global or system configuration.
const LFS_FILTERS: &[&str] = &[
    "git-lfs clean -- %f",
    "git-lfs smudge -- %f",
    "git-lfs smudge --skip -- %f",
    "git-lfs filter-process",
    "git-lfs filter-process --skip",
];

/// `-c` overrides (as `GIT_CONFIG_*`) for the output of `git config --show-scope --null
/// --get-regexp '^(hook|merge|filter|gpg)\.'` (`scope\0key\nvalue\0` records), sorted by key:
/// - `hook.<name>.enabled=false` for every configured hook;
/// - [`MERGE_DRIVER_OFF`] for every merge driver;
/// - a filter driver with a `clean`, `smudge` or `process` command in the repository's own
///   configuration (`local`, `worktree` scope: what an agent can write, e.g. with `git config`
///   from its worktree), other than [`LFS_FILTERS`], is neutralized: `clean`/`smudge` `cat`,
///   `process` empty, `required=false`. Otherwise every `git add`, autocommit and `merge
///   --ff-only` would run it outside any turn. The user's global filters (Git LFS) are kept;
/// - a `gpg.*` program in that same configuration turns signing off (`commit.gpgSign=false`,
///   `tag.gpgSign=false`): with the user's `commit.gpgSign` it would run on every commit.
pub fn config_overrides_from(stdout: &[u8]) -> Vec<(OsString, OsString)> {
    let mut overrides = BTreeMap::new();
    let mut set = |key: &[u8], value: &str| {
        overrides.insert(OsStr::from_bytes(key).to_owned(), OsString::from(value));
    };
    let mut fields = stdout.split(|b| *b == 0);
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let (key, value) = match entry.iter().position(|b| *b == b'\n') {
            Some(i) => (&entry[..i], Some(&entry[i + 1..])),
            None => (entry, None),
        };
        let repo_scope = matches!(scope, b"local" | b"worktree");
        // `<section>.<subsection>.<variable>`; the subsection may contain dots.
        let Some(dot) = key.iter().rposition(|b| *b == b'.') else {
            continue;
        };
        let (name, var) = (&key[..dot], &key[dot + 1..]);
        let lower = var.to_ascii_lowercase();
        if name.starts_with(b"hook.") {
            set(&[name, b".enabled"].concat(), "false");
        } else if name.starts_with(b"merge.") && lower == b"driver" {
            set(key, MERGE_DRIVER_OFF);
        } else if name.starts_with(b"filter.")
            && matches!(&lower[..], b"clean" | b"smudge" | b"process")
            && repo_scope
            && !value.is_some_and(|v| LFS_FILTERS.iter().any(|l| l.as_bytes() == v))
        {
            for (var, value) in [
                ("clean", "cat"),
                ("smudge", "cat"),
                ("process", ""),
                ("required", "false"),
            ] {
                set(&[name, b".", var.as_bytes()].concat(), value);
            }
        } else if name.starts_with(b"gpg") && repo_scope {
            set(b"commit.gpgSign", "false");
            set(b"tag.gpgSign", "false");
        }
    }
    overrides.into_iter().collect()
}

fn too_old(version: &str) -> String {
    format!("git {version} è troppo vecchio: serve almeno la versione {MIN_GIT_VERSION}")
}

fn subcommand<'a>(args: &[&'a str]) -> &'a str {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if *arg == "-c" {
            it.next();
        } else if !arg.starts_with('-') {
            return arg;
        }
    }
    args.first().copied().unwrap_or_default()
}

fn failure(args: &[&str], out: &GitOutput) -> AppError {
    let detail = match out.stderr.trim() {
        "" => out.text(),
        stderr => stderr.to_owned(),
    };
    AppError::git(format!(
        "git {} non riuscito ({}): {detail}",
        subcommand(args),
        out.code
    ))
}

/// Git's ref-name rules (`check-ref-format --branch`), without spawning git: target names
/// come from the UI and go into `refs/heads/<name>` revisions.
fn check_branch(name: &str) -> Result<(), AppError> {
    let bad = name.is_empty()
        || name == "@"
        || name.starts_with(['-', '/'])
        || name.ends_with(['/', '.'])
        || ["..", "@{", "//"].iter().any(|s| name.contains(s))
        || name
            .chars()
            .any(|c| c.is_ascii_control() || " ~^:?*[\\".contains(c))
        || name
            .split('/')
            .any(|part| part.starts_with('.') || part.ends_with(".lock"));
    if bad {
        return Err(AppError::invalid(format!(
            "Nome di branch non valido: {name:?}"
        )));
    }
    Ok(())
}

/// A revision passed as a positional argument (a commit id from the DB).
fn check_rev(rev: &str) -> Result<(), AppError> {
    if rev.is_empty()
        || rev.starts_with('-')
        || rev.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(AppError::invalid(format!("Revisione non valida: {rev:?}")));
    }
    Ok(())
}

fn utf8(path: &Path) -> Result<&str, AppError> {
    path.to_str()
        .ok_or_else(|| AppError::invalid(format!("Percorso non UTF-8: {}", path.display())))
}

fn canonical(path: &Path) -> Result<PathBuf, AppError> {
    path.canonicalize()
        .map_err(|e| AppError::io(format!("{}: {e}", path.display())))
}

/// `path` with its parent canonicalized, for a path that may no longer exist.
fn canonical_parent(path: &Path) -> PathBuf {
    match (path.parent().map(Path::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// A file in the temp dir, removed with its `.lock` on drop.
struct TempFile(PathBuf);

impl TempFile {
    fn new(prefix: &str) -> TempFile {
        TempFile(std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let mut lock = self.0.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
    }
}

/// Lowercase title, `[^a-z0-9]+` → `-`, trimmed, at most 24 chars, `task` if empty.
pub fn slugify(title: &str) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(24).collect();
    match slug.trim_end_matches('-') {
        "" => "task".to_owned(),
        slug => slug.to_owned(),
    }
}

/// `atm/<first 8 hex of attempt_id>-<slugify(title)>` (collisions: [`Git::unique_branch`]).
pub fn branch_name(attempt_id: &str, title: &str) -> String {
    let short: String = attempt_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(8)
        .collect();
    format!("atm/{}-{}", short.to_ascii_lowercase(), slugify(title))
}

/// `<root>/<attempt_id>`.
pub fn worktree_path(root: &Path, attempt_id: &str) -> PathBuf {
    root.join(attempt_id)
}

/// Expands `~` against `home`, creates the directory 0700, canonicalizes it.
/// Errors: `Invalid` if the path is relative, not UTF-8 or contains spaces, `Io`.
pub fn resolve_worktree_root(setting: &str, home: &Path) -> Result<PathBuf, AppError> {
    let path = match setting.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(setting),
    };
    let check = |path: &Path| match path.to_str() {
        Some(s) if !s.chars().any(char::is_whitespace) => Ok(()),
        _ => Err(AppError::invalid(format!(
            "La cartella dei worktree non può contenere spazi: {}",
            path.display()
        ))),
    };
    if !path.is_absolute() {
        return Err(AppError::invalid(format!(
            "La cartella dei worktree deve essere un percorso assoluto: {setting}"
        )));
    }
    check(&path)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .map_err(|e| AppError::io(format!("{}: {e}", path.display())))?;
    let real = canonical(&path)?;
    check(&real)?;
    Ok(real)
}

/// `atm: turn <seq>: <first line of the prompt, at most 60 chars>` (spec §8.5).
pub fn turn_commit_message(seq: u32, prompt: &str) -> String {
    let line: String = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(60)
        .collect();
    match line.trim_end() {
        "" => format!("atm: turn {seq}"),
        line => format!("atm: turn {seq}: {line}"),
    }
}

/// M6 (spec §8.9): SHA-256 hex over the sorted records of `.claude/**`, `.mcp.json` and the
/// files inside `root` their commands run (`path\0kind\0len\0content`, ≤ 2000 records,
/// symlinks recorded by target and followed only inside `root`; details in
/// [`config_snapshot_blocking`]). Walks and hashes on the blocking pool (`spawn_blocking`): it
/// runs before every turn, from async code. The same configuration gives the same value in
/// the main checkout and in a worktree. Errors: `Invalid` (limits, a link out of `root`, the
/// tree changed meanwhile), `Io`.
pub async fn config_fingerprint(root: &Path) -> Result<String, AppError> {
    config_snapshot(root).await.map(|s| s.fingerprint)
}

/// [`config_fingerprint`] with the digest of every record and what the settings allow
/// ([`ConfigSnapshot`]).
pub async fn config_snapshot(root: &Path) -> Result<ConfigSnapshot, AppError> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || config_snapshot_blocking(&root))
        .await
        .map_err(|e| AppError::internal(format!("fingerprint della configurazione: {e}")))?
}
