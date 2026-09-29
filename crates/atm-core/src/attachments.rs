//! Task attachments (spec F5): files picked in the native dialog and copied into the app's data
//! dir, never into a worktree (`git add -A` never sees them), read-only for the agents. Owner:
//! CORE-RUNNER (staging, validation, copy, cleanup). The on-disk layout below is the contract:
//! `Attachment::path`, the prompt's list and `--add-dir` all come from it.
//!
//! The picker is the authority: the core checks the paths it returns ([`check_pick`]) and
//! stages them under one-use tokens ([`Staging`]); the webview only ever sees the tokens.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atm_types::{AppError, Attachment, Id, MAX_ATTACHMENT_BYTES, Millis, PickedFile};

use crate::db::AttachmentRow;
use crate::{is_hidden_char, new_id};

/// Folder of every attachment, under `CoreConfig::data_dir`.
pub const ATTACHMENTS_DIR: &str = "attachments";
/// How long a staged pick can be redeemed.
pub const PICK_TTL: Duration = Duration::from_secs(600);
/// Staged picks kept at most; past it the oldest are dropped.
pub const MAX_STAGED: usize = 100;
/// Longest attachment name, in characters ([`attachment_name`]).
pub const MAX_NAME_CHARS: usize = 120;
/// Longest extension [`attachment_name`] keeps when it cuts a name.
const MAX_EXTENSION_CHARS: usize = 16;
/// What `~/.claude*` and `$CLAUDE_CONFIG_DIR` are called in a refusal.
const CLAUDE_CONFIG: &str = "la configurazione di Claude Code";

/// `<data_dir>/attachments/<project_id>`: the attachments of a project's tasks.
pub fn project_dir(data_dir: &Path, project_id: &str) -> PathBuf {
    data_dir.join(ATTACHMENTS_DIR).join(project_id)
}

/// `<data_dir>/attachments/<project_id>/<task_id>`: a task's attachments, the folder its agent
/// is given with `--add-dir`.
pub fn task_dir(data_dir: &Path, project_id: &str, task_id: &str) -> PathBuf {
    project_dir(data_dir, project_id).join(task_id)
}

/// `<task_dir>/<attachment_id>/<name>`: one folder per attachment, so two files with the same
/// name never collide and the copy keeps the name the agent is told.
pub fn attachment_path(
    data_dir: &Path,
    project_id: &str,
    task_id: &str,
    attachment_id: &str,
    name: &str,
) -> PathBuf {
    task_dir(data_dir, project_id, task_id)
        .join(attachment_id)
        .join(name)
}

/// IPC view of `row`, an attachment of a task of `project_id`.
pub fn view(data_dir: &Path, project_id: &str, row: &AttachmentRow) -> Attachment {
    row.view(&attachment_path(
        data_dir,
        project_id,
        &row.task_id,
        &row.id,
        &row.name,
    ))
}

// ---- what the agent is given --------------------------------------------------------------

/// A task's attachments as its agent gets them: the task's folder, canonical (`--add-dir`;
/// the CLI checks real paths), and the copies' paths under it, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForAgent {
    pub dir: PathBuf,
    pub paths: Vec<PathBuf>,
}

/// [`ForAgent`] of the task's `rows`; `None` without any.
pub fn for_agent(
    data_dir: &Path,
    project_id: &str,
    task_id: &str,
    rows: &[AttachmentRow],
) -> Option<ForAgent> {
    if rows.is_empty() {
        return None;
    }
    let dir = task_dir(data_dir, project_id, task_id);
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let paths = rows.iter().map(|r| dir.join(&r.id).join(&r.name)).collect();
    Some(ForAgent { dir, paths })
}

/// The section that ends the task's prompt (first turn, fresh session) when it has
/// attachments: their absolute paths as code spans (a name never holds a backtick,
/// [`attachment_name`]), read-only copies. Empty without any.
pub fn prompt_section(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut section = String::from(
        "\n\n## Attachments\n\nThe user attached these files to the task. They are read-only \
         copies kept outside the repository: read them from these paths, do not modify, move \
         or delete them.\n",
    );
    for path in paths {
        section.push_str(&format!("\n- `{}`", path.display()));
    }
    section
}

// ---- staging ------------------------------------------------------------------------------

/// A file the picker returned, checked ([`check_pick`]): what a token stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    /// Canonical path of the original.
    pub path: PathBuf,
    /// The copy's name ([`attachment_name`]).
    pub name: String,
    /// Size when checked (the copy checks it again).
    pub size: u64,
    /// The file checked ([`FileId`]): the copy must open that same file.
    pub id: FileId,
}

/// A file's identity, device and inode (`fstat`): what a rename over the path, a hard link or
/// a folder of the path swapped for a link changes.
pub type FileId = (u64, u64);

/// The staged picks by token: each one redeemable once within [`PICK_TTL`], at most
/// [`MAX_STAGED`] (the oldest are dropped first). The clock is the caller's `now`.
#[derive(Debug, Default)]
pub struct Staging {
    /// Oldest first.
    picks: VecDeque<(Id, Instant, Picked)>,
}

impl Staging {
    /// A new token for each file, in order.
    pub fn stage(&mut self, files: Vec<Picked>, now: Instant) -> Vec<PickedFile> {
        self.expire(now);
        let mut staged = Vec::with_capacity(files.len());
        for file in files {
            let token = new_id();
            staged.push(PickedFile {
                token: token.clone(),
                name: file.name.clone(),
                size: file.size,
            });
            self.picks.push_back((token, now, file));
        }
        while self.picks.len() > MAX_STAGED {
            self.picks.pop_front();
        }
        staged
    }

    /// The file of `token`, which is used up; `None` if unknown, used or expired.
    pub fn redeem(&mut self, token: &str, now: Instant) -> Option<Picked> {
        self.expire(now);
        let i = self.picks.iter().position(|(t, _, _)| t == token)?;
        self.picks.remove(i).map(|(_, _, file)| file)
    }

    fn expire(&mut self, now: Instant) {
        self.picks
            .retain(|(_, at, _)| now.saturating_duration_since(*at) < PICK_TTL);
    }
}

// ---- checks -------------------------------------------------------------------------------

/// Folders no attachment may come from (defence in depth behind the picker): the user's
/// Claude Code configuration (`~/.claude*`, `$CLAUDE_CONFIG_DIR`), `~/.ssh`, `~/.aws`, the
/// Keychain, and the app's data and cache (every project's transcripts and logs). Each as
/// given and canonical, compared with canonical paths ASCII case-insensitively (APFS is by
/// default); a `~/.claude*` link counts with its target.
#[derive(Debug, Clone)]
pub struct DenyList {
    /// The home directory, for `~/.claude*`.
    homes: Vec<PathBuf>,
    /// Refused folders with what the refusal calls them.
    dirs: Vec<(PathBuf, &'static str)>,
}

impl DenyList {
    /// Reads the file system (canonical forms, the `.claude*` entries of `home`).
    pub fn new(
        home: &Path,
        claude_config_dir: Option<&Path>,
        data_dir: &Path,
        cache_dir: &Path,
    ) -> DenyList {
        let mut deny = DenyList {
            homes: forms(home),
            dirs: Vec::new(),
        };
        deny.add(&home.join(".ssh"), "~/.ssh");
        deny.add(&home.join(".aws"), "~/.aws");
        deny.add(
            &home.join("Library/Keychains"),
            "il Portachiavi (~/Library/Keychains)",
        );
        if let Some(dir) = claude_config_dir {
            deny.add(dir, CLAUDE_CONFIG);
        }
        if let Ok(entries) = std::fs::read_dir(home) {
            for entry in entries.flatten() {
                if is_claude_name(&entry.file_name()) {
                    deny.add(&entry.path(), CLAUDE_CONFIG);
                }
            }
        }
        deny.add(data_dir, "la cartella dei dati dell'app");
        deny.add(cache_dir, "la cartella della cache dell'app");
        deny
    }

    fn add(&mut self, dir: &Path, what: &'static str) {
        self.dirs.extend(forms(dir).into_iter().map(|d| (d, what)));
    }

    /// What `path` (canonical) lies in, if it is refused.
    pub fn refusal(&self, path: &Path) -> Option<&'static str> {
        let claude = self.homes.iter().any(|home| {
            strip_prefix_ci(path, home)
                .and_then(|rest| rest.components().next())
                .is_some_and(|first| is_claude_name(first.as_os_str()))
        });
        if claude {
            return Some(CLAUDE_CONFIG);
        }
        self.dirs
            .iter()
            .find(|(dir, _)| strip_prefix_ci(path, dir).is_some())
            .map(|&(_, what)| what)
    }
}

/// `path` as given and canonical (when it exists and differs); nothing unless absolute.
fn forms(path: &Path) -> Vec<PathBuf> {
    if !path.is_absolute() {
        return Vec::new();
    }
    let mut forms = vec![path.to_path_buf()];
    if let Ok(real) = std::fs::canonicalize(path)
        && real != path
    {
        forms.push(real);
    }
    forms
}

/// `.claude`, `.claude.json`, `.claude-work`, … in any case.
fn is_claude_name(name: &OsStr) -> bool {
    name.as_encoded_bytes()
        .get(..".claude".len())
        .is_some_and(|b| b.eq_ignore_ascii_case(b".claude"))
}

/// [`Path::strip_prefix`] comparing components ASCII case-insensitively.
fn strip_prefix_ci<'a>(path: &'a Path, prefix: &Path) -> Option<&'a Path> {
    let mut rest = path.components();
    for want in prefix.components() {
        let got = rest.next()?;
        let same = got
            .as_os_str()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(want.as_os_str().as_encoded_bytes());
        if !same {
            return None;
        }
    }
    Some(rest.as_path())
}

/// Checks a file the picker returned: canonicalized (links resolved), outside the
/// [`DenyList`], a regular file ([`symlink_metadata`](std::fs::symlink_metadata), then opened
/// without following a link nor waiting on a FIFO and checked again), at most
/// [`MAX_ATTACHMENT_BYTES`], with a usable name ([`attachment_name`]). Blocking. Errors:
/// `Invalid`, naming the file.
pub fn check_pick(path: &Path, deny: &DenyList) -> Result<Picked, AppError> {
    let shown = shown_name(path);
    let canonical = std::fs::canonicalize(path).map_err(|e| unreadable(&shown, &e))?;
    if let Some(what) = deny.refusal(&canonical) {
        return Err(AppError::invalid(format!(
            "«{shown}» non si può allegare: è dentro {what}, che l'app non legge"
        )));
    }
    let meta = std::fs::symlink_metadata(&canonical).map_err(|e| unreadable(&shown, &e))?;
    if !meta.is_file() {
        return Err(not_regular(&shown));
    }
    let (_, size, id) = open_regular(&canonical, &shown)?;
    let name = attachment_name(&canonical)?;
    Ok(Picked {
        path: canonical,
        name,
        size,
        id,
    })
}

/// The copy's name: the file's own name without hidden characters ([`is_hidden_char`]), cut
/// to [`MAX_NAME_CHARS`] keeping a short extension. Refused when that leaves nothing (or
/// blanks), `.` or `..`, or when it holds a backtick (the prompt lists the paths as code
/// spans). Errors: `Invalid`.
fn attachment_name(path: &Path) -> Result<String, AppError> {
    let name = shown_name(path);
    if name.trim().is_empty() || name == "." || name == ".." || name.contains('`') {
        return Err(AppError::invalid(format!(
            "Il nome «{name}» non va bene per un allegato (vuoto, «.», «..» o con un apice \
             inverso `): rinomina il file e riprova"
        )));
    }
    if name.chars().count() <= MAX_NAME_CHARS {
        return Ok(name);
    }
    let extension = name
        .rfind('.')
        .filter(|&i| i > 0)
        .map(|i| &name[i..])
        .filter(|e| e.chars().count() <= MAX_EXTENSION_CHARS)
        .unwrap_or("");
    let stem = &name[..name.len() - extension.len()];
    let keep = MAX_NAME_CHARS - extension.chars().count();
    Ok(stem.chars().take(keep).collect::<String>() + extension)
}

/// The last component of `path` (else the whole path) without hidden characters: how the
/// messages name a file.
fn shown_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .chars()
        .filter(|c| !is_hidden_char(*c))
        .collect()
}

/// Opens `path` to read it without following a final link (`O_NOFOLLOW`) nor waiting for a
/// FIFO's writer (`O_NONBLOCK`), then checks (`fstat`) that it is a regular file of at most
/// [`MAX_ATTACHMENT_BYTES`]: the file, its size and its identity.
fn open_regular(path: &Path, shown: &str) -> Result<(File, u64, FileId), AppError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => not_regular(shown),
            _ => unreadable(shown, &e),
        })?;
    let meta = file.metadata().map_err(|e| unreadable(shown, &e))?;
    if !meta.is_file() {
        return Err(not_regular(shown));
    }
    if meta.len() > MAX_ATTACHMENT_BYTES {
        return Err(too_large(shown));
    }
    Ok((file, meta.len(), (meta.dev(), meta.ino())))
}

fn unreadable(shown: &str, e: &std::io::Error) -> AppError {
    AppError::invalid(format!("«{shown}» non si può leggere: {e}"))
}

fn not_regular(shown: &str) -> AppError {
    AppError::invalid(format!(
        "«{shown}» non è un file normale: si possono allegare solo file"
    ))
}

fn changed(shown: &str) -> AppError {
    AppError::invalid(format!(
        "«{shown}» è cambiato dopo la scelta (sostituito o spostato): sceglilo di nuovo"
    ))
}

fn too_large(shown: &str) -> AppError {
    AppError::invalid(format!(
        "«{shown}» è troppo grande: il massimo è {} MB",
        MAX_ATTACHMENT_BYTES >> 20
    ))
}

// ---- copies -------------------------------------------------------------------------------

/// Copies each pick into a new folder `<task_dir>/<new attachment id>/` (0700; `task_dir` and
/// its missing parents are created 0700 too) as a new file with its name (0600,
/// `create_new`): the original is opened and checked again, it must still be the file checked
/// when staged ([`Picked::id`]), and a file that grew past [`MAX_ATTACHMENT_BYTES`] while
/// copying is refused. The rows to insert; on any error the
/// folders it created are removed. Blocking. Errors: `Invalid` (naming the file), `Io`.
pub fn copy_picks(
    picks: &[Picked],
    task_dir: &Path,
    task_id: &str,
    now: Millis,
) -> Result<Vec<AttachmentRow>, AppError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(task_dir)
        .map_err(|e| AppError::io(format!("{}: {e}", task_dir.display())))?;
    let mut rows: Vec<AttachmentRow> = Vec::with_capacity(picks.len());
    for pick in picks {
        match copy_pick(pick, task_dir) {
            Ok((id, size)) => rows.push(AttachmentRow {
                id,
                task_id: task_id.to_owned(),
                name: pick.name.clone(),
                size,
                created_at: now,
            }),
            Err(e) => {
                for row in &rows {
                    remove_dir_logged(&task_dir.join(&row.id));
                }
                return Err(e);
            }
        }
    }
    Ok(rows)
}

/// One pick into a new `<task_dir>/<id>/`, removed again if the copy fails: the id and the
/// bytes copied.
fn copy_pick(pick: &Picked, task_dir: &Path) -> Result<(Id, u64), AppError> {
    let (file, _, id) = open_regular(&pick.path, &pick.name)?;
    if id != pick.id {
        return Err(changed(&pick.name));
    }
    let id = new_id();
    let dir = task_dir.join(&id);
    DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| AppError::io(format!("{}: {e}", dir.display())))?;
    let copied = write_copy(file, &dir.join(&pick.name), &pick.name);
    if copied.is_err() {
        remove_dir_logged(&dir);
    }
    copied.map(|size| (id, size))
}

fn write_copy(file: File, dest: &Path, shown: &str) -> Result<u64, AppError> {
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dest)
        .map_err(|e| AppError::io(format!("{}: {e}", dest.display())))?;
    let copied = std::io::copy(&mut file.take(MAX_ATTACHMENT_BYTES + 1), &mut out)
        .map_err(|e| AppError::io(format!("«{shown}» non copiato: {e}")))?;
    if copied > MAX_ATTACHMENT_BYTES {
        return Err(too_large(shown));
    }
    Ok(copied)
}

/// Removes `dir` with its content, best effort: a failure is logged, a missing `dir` is fine.
/// Blocking.
pub fn remove_dir_logged(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("{}: not removed: {e}", dir.display());
    }
}
