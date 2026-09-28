//! Diff, branch status and merge types (spec §6.2, §8.6, §8.7).

use serde::{Deserialize, Serialize};

/// From `diff --name-status` (`A M D R C T`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

/// `Hunk` = the `@@ … @@` header; `Meta` = `\ No newline at end of file` and similar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineKind {
    Hunk,
    Context,
    Add,
    Del,
    Meta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// The line without its `+`/`-`/` ` prefix.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// Source path of a rename or copy.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    /// Patch over 256 KiB: stats only.
    pub too_large: bool,
    /// Over the 4 MiB / 300 files budget: stats only.
    pub omitted: bool,
    pub lines: Vec<DiffLine>,
}

/// Diff of `merge-base(target, HEAD)` against a snapshot tree of the worktree, including
/// uncommitted work (spec §8.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffResult {
    pub base: String,
    pub snapshot_tree: String,
    pub files: Vec<FileDiff>,
    pub additions: u32,
    pub deletions: u32,
    /// Some files were `omitted`.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchStatus {
    pub target_branch: String,
    /// Commits of the attempt branch not in the target.
    pub ahead: u32,
    /// Commits of the target not in the attempt branch.
    pub behind: u32,
    pub dirty: bool,
    /// The worktree HEAD is `refs/heads/<attempt branch>`.
    pub head_ok: bool,
    /// Conflict preview from `merge-tree --write-tree`.
    pub conflicts: Vec<String>,
    /// Worktree path where the target branch is checked out, if any.
    pub target_checked_out_at: Option<String>,
    /// Why "Merge" is disabled (Italian, user-facing), `None` if it is allowed.
    pub merge_blocked: Option<String>,
}

/// `UpdateRef`: CAS on `refs/heads/<target>` (target not checked out);
/// `FfCheckedOut`: `merge --ff-only` in the worktree that has the target checked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MergeStrategy {
    UpdateRef,
    FfCheckedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MergeOutcome {
    /// `cleanup_warning`: the merge happened but removing the worktree failed.
    Merged {
        commit: String,
        strategy: MergeStrategy,
        cleanup_warning: Option<String>,
    },
    NothingToMerge,
    /// No ref was touched.
    Conflicts {
        files: Vec<String>,
    },
}

/// Characters of the task description kept by [`merge_message`].
pub const MERGE_MESSAGE_MAX_DESCRIPTION: usize = 2000;

/// Default squash message, editable in the merge dialog (spec §8.7): title, blank line,
/// description (at most [`MERGE_MESSAGE_MAX_DESCRIPTION`] characters; skipped when empty),
/// blank line, `ATM-Attempt: <id>`.
pub fn merge_message(title: &str, description: &str, attempt_id: &str) -> String {
    let description: String = description
        .trim()
        .chars()
        .take(MERGE_MESSAGE_MAX_DESCRIPTION)
        .collect();
    let mut message = format!("{}\n\n", title.trim());
    if !description.is_empty() {
        message.push_str(description.trim_end());
        message.push_str("\n\n");
    }
    message.push_str("ATM-Attempt: ");
    message.push_str(attempt_id);
    message
}
