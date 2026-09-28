//! Hardened git CLI runner and the worktree / commit / diff / merge operations (spec §8).
//! Owner: M2-GIT (the fingerprint, §8.9, is M6).
//!
//! Locking is the caller's (spec §8.1): Core holds the per-attempt mutex and, for
//! `worktree add/remove`, merge and branch deletion, the per-repo mutex (order attempt → repo).
//! Paths and branch names are passed as UTF-8 (`Invalid` otherwise); every operation that
//! takes paths uses `-z` and `--`, every revision goes after `--end-of-options`.
// M1 contract stubs: remove these allows when implementing.
#![allow(unused_variables, dead_code)]

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

use atm_types::{
    AppError, BranchList, BranchStatus, DiffLine, DiffResult, MergeOutcome, WorktreeState,
};

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

/// Shared with `claude::ChildEnv`.
pub fn is_scrubbed_git_var(name: &str) -> bool {
    SCRUBBED_GIT_VARS.contains(&name)
        || name.starts_with("GIT_CONFIG_KEY_")
        || name.starts_with("GIT_CONFIG_VALUE_")
}

/// `command -v git` over the login-shell `PATH` (spec §8.1), else [`FALLBACK_GIT`].
pub fn find_git(path_var: &OsStr) -> PathBuf {
    todo!("M2-GIT: find_git")
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
        Err(AppError::not_implemented("Git::version"))
    }

    /// Runs `git -c core.hooksPath=/dev/null -c core.fsmonitor=false -c core.quotePath=false
    /// -c color.ui=never -c gc.auto=0 -C <dir> <args>` with stdin null, `kill_on_drop`, the
    /// scrubbed environment plus `LC_ALL=C LANGUAGE=C GIT_TERMINAL_PROMPT=0 GIT_EDITOR=true
    /// GIT_PAGER=cat` (spec §8.1). A non-zero exit is returned in `code`. Errors: `Git` on
    /// spawn failure, timeout (the process is killed) or stdout over [`MAX_OUTPUT`].
    pub async fn run(
        &self,
        dir: &Path,
        args: &[&str],
        opts: &RunOpts,
    ) -> Result<GitOutput, AppError> {
        Err(AppError::not_implemented("Git::run"))
    }

    /// [`Git::run`] that turns a non-zero exit into `Git` with the stderr text.
    pub async fn run_ok(
        &self,
        dir: &Path,
        args: &[&str],
        opts: &RunOpts,
    ) -> Result<GitOutput, AppError> {
        Err(AppError::not_implemented("Git::run_ok"))
    }

    /// Checks a folder for `add_project` (spec §8.3): toplevel (canonical), not bare, at least
    /// one commit, main checkout (`--git-dir` = `--git-common-dir`), not inside
    /// `worktree_root`. Errors: `Invalid` with an Italian message for each failed rule.
    pub async fn validate_repo(
        &self,
        path: &Path,
        worktree_root: &Path,
    ) -> Result<RepoInfo, AppError> {
        Err(AppError::not_implemented("Git::validate_repo"))
    }

    /// Local branches (`for-each-ref refs/heads`) and the checked-out one.
    pub async fn list_branches(&self, repo: &Path) -> Result<BranchList, AppError> {
        Err(AppError::not_implemented("Git::list_branches"))
    }

    /// `rev-parse --verify --end-of-options refs/heads/<branch>^{commit}`.
    /// Errors: `NotFound` if the branch does not exist.
    pub async fn branch_tip(&self, repo: &Path, branch: &str) -> Result<String, AppError> {
        Err(AppError::not_implemented("Git::branch_tip"))
    }

    /// [`branch_name`] validated with `check-ref-format --branch`; on collision
    /// (`show-ref --verify`) appends `-2`, `-3`, … (spec §8.2).
    pub async fn unique_branch(
        &self,
        repo: &Path,
        attempt_id: &str,
        title: &str,
    ) -> Result<String, AppError> {
        Err(AppError::not_implemented("Git::unique_branch"))
    }

    /// `worktree add --lock --reason "atm:<attempt_id>" -b <branch> <worktree> <base>` and
    /// returns the canonical path (spec §8.4). On failure removes only that worktree
    /// (`rm -rf` only under the canonical root) and retries once. Timeout 600 s.
    pub async fn add_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        base: &str,
        attempt_id: &str,
    ) -> Result<PathBuf, AppError> {
        Err(AppError::not_implemented("Git::add_worktree"))
    }

    /// Removal (spec §8.4): snapshot commit if dirty, `worktree unlock`, `worktree remove
    /// --force`; if the directory is already gone, deletes only its metadata dir (matched by
    /// `gitdir`). Never a global `worktree prune`; the branch is kept.
    pub async fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<(), AppError> {
        Err(AppError::not_implemented("Git::remove_worktree"))
    }

    pub async fn list_worktrees(&self, repo: &Path) -> Result<Vec<WorktreeEntry>, AppError> {
        Err(AppError::not_implemented("Git::list_worktrees"))
    }

    /// State of each of `worktrees` (same order): `Present` if listed by git and the
    /// directory exists, else `Missing`. Deletes nothing (spec §8.4).
    pub async fn reconcile_worktrees(
        &self,
        repo: &Path,
        worktrees: &[PathBuf],
    ) -> Result<Vec<WorktreeState>, AppError> {
        Err(AppError::not_implemented("Git::reconcile_worktrees"))
    }

    /// Auto-commit (spec §8.5): if `status --porcelain=v1 -z --untracked-files=all` is not
    /// empty, `add -A` then `commit --no-verify -q -m <message>`, with the fallback identity
    /// when `user.name`/`user.email` are missing. `None` if the worktree was clean.
    pub async fn autocommit(
        &self,
        worktree: &Path,
        message: &str,
    ) -> Result<Option<AutoCommit>, AppError> {
        Err(AppError::not_implemented("Git::autocommit"))
    }

    /// `rev-parse HEAD`.
    pub async fn head(&self, worktree: &Path) -> Result<String, AppError> {
        Err(AppError::not_implemented("Git::head"))
    }

    /// `symbolic-ref -q HEAD` (full ref), `None` if detached.
    pub async fn head_ref(&self, worktree: &Path) -> Result<Option<String>, AppError> {
        Err(AppError::not_implemented("Git::head_ref"))
    }

    /// `log --oneline <base>..HEAD`, for the `fresh_session` prompt (spec §7.4).
    pub async fn log_oneline(&self, worktree: &Path, base: &str) -> Result<String, AppError> {
        Err(AppError::not_implemented("Git::log_oneline"))
    }

    /// Diff of `merge-base refs/heads/<target> HEAD` against a snapshot tree written through
    /// a copy of the worktree index (the agent's index is untouched), with the §8.6 budget.
    pub async fn snapshot_diff(
        &self,
        worktree: &Path,
        target_branch: &str,
    ) -> Result<DiffResult, AppError> {
        Err(AppError::not_implemented("Git::snapshot_diff"))
    }

    /// Spec §8.7: ahead/behind, dirty, `head_ok` (HEAD = `refs/heads/<branch>`), conflict
    /// preview, where the target is checked out, and why merge is blocked.
    pub async fn branch_status(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        target_branch: &str,
    ) -> Result<BranchStatus, AppError> {
        Err(AppError::not_implemented("Git::branch_status"))
    }

    /// Squash merge steps 1–6 of spec §8.7: auto-commit, `merge-tree --write-tree`,
    /// `commit-tree -p T0`, then `update-ref` CAS (one retry) or `merge --ff-only` in the
    /// worktree that has the target checked out. `cleanup_warning` is left `None` (the caller
    /// removes the worktree). Errors: `GitIdentityMissing`, `TargetCheckoutDirty`, `Git`.
    pub async fn squash_merge(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        target_branch: &str,
        message: &str,
        attempt_id: &str,
    ) -> Result<MergeOutcome, AppError> {
        Err(AppError::not_implemented("Git::squash_merge"))
    }

    /// `branch -D <branch>`; `Invalid` unless it starts with `atm/`.
    pub async fn delete_branch(&self, repo: &Path, branch: &str) -> Result<(), AppError> {
        Err(AppError::not_implemented("Git::delete_branch"))
    }
}

/// Lowercase title, `[^a-z0-9]+` → `-`, trimmed, at most 24 chars, `task` if empty.
pub fn slugify(title: &str) -> String {
    todo!("M2-GIT: slugify")
}

/// `atm/<first 8 hex of attempt_id>-<slugify(title)>` (collisions: [`Git::unique_branch`]).
pub fn branch_name(attempt_id: &str, title: &str) -> String {
    todo!("M2-GIT: branch_name")
}

/// `<root>/<attempt_id>`.
pub fn worktree_path(root: &Path, attempt_id: &str) -> PathBuf {
    todo!("M2-GIT: worktree_path")
}

/// Expands `~` against `home`, creates the directory 0700, canonicalizes it.
/// Errors: `Invalid` if the canonical path contains spaces, `Io`.
pub fn resolve_worktree_root(setting: &str, home: &Path) -> Result<PathBuf, AppError> {
    Err(AppError::not_implemented("git::resolve_worktree_root"))
}

/// `atm: turn <seq>: <first line of the prompt, at most 60 chars>` (spec §8.5).
pub fn turn_commit_message(seq: u32, prompt: &str) -> String {
    todo!("M2-GIT: turn_commit_message")
}

/// Parses `worktree list --porcelain -z`.
pub fn parse_worktree_list(out: &[u8]) -> Vec<WorktreeEntry> {
    todo!("M2-GIT: parse_worktree_list")
}

/// Parses the hunks of one file's unified diff into numbered lines (spec §8.6); file headers
/// before the first `@@` are skipped.
pub fn parse_hunks(patch: &str) -> Vec<DiffLine> {
    todo!("M2-GIT: parse_hunks")
}

/// M6 (spec §8.9): SHA-256 hex over `path\0len\0content` of the sorted files under
/// `.claude/**` plus `.mcp.json` (≤ 2000 files, symlinks hashed by target, paths confined
/// to `root`). Walks and hashes on the blocking pool (`spawn_blocking`): it runs before
/// every turn, from async code.
pub async fn config_fingerprint(root: &Path) -> Result<String, AppError> {
    Err(AppError::not_implemented("git::config_fingerprint"))
}
