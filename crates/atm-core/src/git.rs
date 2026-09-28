//! Hardened git CLI runner and the worktree / commit / diff / merge operations (spec §8).
//! Owner: M2-GIT (the fingerprint, §8.9, is M6).
//!
//! Locking is the caller's (spec §8.1): Core holds the per-attempt mutex and, for
//! `worktree add/remove`, merge and branch deletion, the per-repo mutex (order attempt → repo).
//! Paths and branch names are passed as UTF-8 (`Invalid` otherwise); every operation that
//! takes paths uses `-z` and `--`, every revision goes after `--end-of-options`.
//!
//! Never run here (spec §8.8): `reset --hard`, `push`, `fetch`, `stash`, `checkout`, a global
//! `worktree prune`, `branch -D` outside `atm/*`, `rm -rf` outside the worktree root.

mod diff;
mod merge;
mod parse;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use atm_types::{AppError, BranchList, WorktreeState};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

pub use parse::{parse_hunks, parse_worktree_list};

/// `merge-tree --write-tree` (spec §2.1).
pub const MIN_GIT_VERSION: &str = "2.38";
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
}

impl Git {
    /// `path_var` becomes `PATH` of every call.
    pub fn new(bin: PathBuf, path_var: OsString) -> Git {
        Git {
            bin,
            path_var,
            extra_env: Vec::new(),
        }
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
        let (out, _) = self
            .spawn(None, &["--version"], &RunOpts::read(), &[], MAX_OUTPUT)
            .await?;
        let text = out.text();
        let (version, found) = parse::parse_version(&text)
            .filter(|_| out.code == 0)
            .ok_or_else(|| {
                AppError::git(format!(
                    "{} non risponde come git: {text}",
                    self.bin.display()
                ))
            })?;
        if Some(found) < parse::major_minor(MIN_GIT_VERSION) {
            return Err(AppError::git(format!(
                "git {version} è troppo vecchio: serve almeno la versione {MIN_GIT_VERSION}"
            )));
        }
        Ok(version)
    }

    /// Runs `git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c core.quotePath=false
    /// -c color.ui=never -c gc.auto=0 -c maintenance.auto=false -C <dir> <args>` with stdin
    /// null, `kill_on_drop`, the scrubbed environment plus `LC_ALL=C LANGUAGE=C
    /// GIT_TERMINAL_PROMPT=0 GIT_EDITOR=true GIT_PAGER=cat` (spec §8.1). Unless the call is
    /// `read_only` without an `index_file` (it cannot write refs or an index), the hooks
    /// defined in the configuration (`hook.<name>.command`, which `core.hooksPath` does not
    /// cover) are listed first and disabled. A non-zero exit is returned in `code`. Errors:
    /// `Git` on spawn failure, timeout (the process is killed) or stdout over [`MAX_OUTPUT`].
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
    /// `gitdir`). Never a global `worktree prune`; the branch is kept.
    pub async fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), AppError> {
        let path = utf8(worktree)?;
        if !worktree.is_dir() {
            return self.remove_worktree_metadata(repo, worktree).await;
        }
        self.autocommit(worktree, SNAPSHOT_MESSAGE).await?;
        let opts = RunOpts::default();
        // Fails harmlessly when not locked; `remove` reports the real problems.
        self.run(repo, &["worktree", "unlock", "--", path], &opts)
            .await?;
        self.run_ok(repo, &["worktree", "remove", "--force", "--", path], &opts)
            .await?;
        Ok(())
    }

    async fn remove_worktree_metadata(&self, repo: &Path, worktree: &Path) -> Result<(), AppError> {
        let common = self
            .run_ok(
                repo,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                &RunOpts::read(),
            )
            .await?
            .path();
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
    pub async fn autocommit(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<AutoCommit>, AppError> {
        let read = RunOpts::read();
        let status = self
            .run_ok(
                worktree,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
                &read,
            )
            .await?;
        let files = parse::status_entries(&status.stdout).len() as u32;
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
        let hooks = if opts.read_only && opts.index_file.is_none() {
            Vec::new()
        } else {
            self.config_hooks(dir).await?
        };
        self.spawn(Some(dir), args, opts, &hooks, limit).await
    }

    /// Names of the hooks defined in the configuration seen from `dir` (`hook.<name>.*`).
    async fn config_hooks(&self, dir: &Path) -> Result<Vec<OsString>, AppError> {
        let args = [
            "config",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^hook\.",
        ];
        let (out, _) = self
            .spawn(Some(dir), &args, &RunOpts::read(), &[], MAX_OUTPUT)
            .await?;
        match out.code {
            0 => {}
            1 => return Ok(Vec::new()),
            _ => return Err(failure(&args, &out)),
        }
        let names: BTreeSet<&[u8]> = out
            .stdout
            .split(|b| *b == 0)
            .filter_map(|key| {
                let rest = key.strip_prefix(b"hook.")?;
                let dot = rest.iter().rposition(|b| *b == b'.')?;
                Some(&rest[..dot])
            })
            .collect();
        Ok(names
            .into_iter()
            .map(|name| OsStr::from_bytes(name).to_owned())
            .collect())
    }

    /// The inherited environment plus `extra_env`, scrubbed, then the forced variables and
    /// `hook.<name>.enabled=false` for each of `hooks` (through `GIT_CONFIG_COUNT`, which,
    /// unlike `-c`, accepts any subsection name).
    fn env(&self, opts: &RunOpts, hooks: &[OsString]) -> BTreeMap<OsString, OsString> {
        let mut env: BTreeMap<OsString, OsString> = std::env::vars_os()
            .chain(self.extra_env.iter().cloned())
            .collect();
        env.retain(|key, _| !key.to_str().is_some_and(is_scrubbed_git_var));
        let mut forced = vec![
            ("PATH".to_owned(), self.path_var.clone()),
            ("LC_ALL".to_owned(), "C".into()),
            ("LANGUAGE".to_owned(), "C".into()),
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".into()),
            ("GIT_EDITOR".to_owned(), "true".into()),
            ("GIT_PAGER".to_owned(), "cat".into()),
        ];
        if opts.read_only {
            forced.push(("GIT_OPTIONAL_LOCKS".to_owned(), "0".into()));
        }
        if let Some(index) = &opts.index_file {
            forced.push(("GIT_INDEX_FILE".to_owned(), index.into()));
        }
        if !hooks.is_empty() {
            forced.push((
                "GIT_CONFIG_COUNT".to_owned(),
                hooks.len().to_string().into(),
            ));
        }
        for (i, name) in hooks.iter().enumerate() {
            let mut key = OsString::from("hook.");
            key.push(name);
            key.push(".enabled");
            forced.push((format!("GIT_CONFIG_KEY_{i}"), key));
            forced.push((format!("GIT_CONFIG_VALUE_{i}"), "false".into()));
        }
        env.extend(forced.into_iter().map(|(k, v)| (k.into(), v)));
        env
    }

    async fn spawn(
        &self,
        dir: Option<&Path>,
        args: &[&str],
        opts: &RunOpts,
        hooks: &[OsString],
        limit: usize,
    ) -> Result<(GitOutput, bool), AppError> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(HARDENING);
        if let Some(dir) = dir {
            cmd.arg("-C").arg(dir);
        }
        cmd.args(args)
            .env_clear()
            .envs(self.env(opts, hooks))
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

/// M6 (spec §8.9): SHA-256 hex over `path\0len\0content` of the sorted files under
/// `.claude/**` plus `.mcp.json` (≤ 2000 files, symlinks hashed by target, paths confined
/// to `root`). Walks and hashes on the blocking pool (`spawn_blocking`): it runs before
/// every turn, from async code.
#[allow(unused_variables)] // M6 stub
pub async fn config_fingerprint(root: &Path) -> Result<String, AppError> {
    Err(AppError::not_implemented("git::config_fingerprint"))
}
