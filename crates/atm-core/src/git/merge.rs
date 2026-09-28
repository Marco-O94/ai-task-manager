//! Branch status and squash merge (spec §8.7).

use std::collections::HashSet;
use std::path::Path;

use atm_types::{AppError, BranchStatus, ErrorCode, MergeOutcome, MergeStrategy};

use super::{Git, RunOpts, SNAPSHOT_MESSAGE, TempFile, check_branch, failure, parse};

/// Files in the git dir of a checkout while an operation is in progress.
const IN_PROGRESS: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

enum MergeTree {
    Clean(String),
    Conflicts(Vec<String>),
}

impl Git {
    /// Spec §8.7: ahead/behind, dirty, `head_ok` (HEAD = `refs/heads/<branch>`), conflict
    /// preview, where the target is checked out, and why merge is blocked (running turns are
    /// the caller's).
    pub async fn branch_status(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        target_branch: &str,
    ) -> Result<BranchStatus, AppError> {
        check_branch(branch)?;
        let read = RunOpts::read();
        let target_tip = self.branch_tip(repo, target_branch).await?;
        let range = format!("refs/heads/{target_branch}...HEAD");
        let counts = self
            .run_ok(
                worktree,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    "--end-of-options",
                    &range,
                ],
                &read,
            )
            .await?
            .text();
        let mut counts = counts.split_whitespace().map(|n| n.parse().unwrap_or(0));
        let (behind, ahead) = (counts.next().unwrap_or(0), counts.next().unwrap_or(0));
        let status = self
            .run_ok(
                worktree,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
                &read,
            )
            .await?;
        let dirty = !status.stdout.is_empty();
        let head_ok = self.head_ref(worktree).await?.as_deref()
            == Some(format!("refs/heads/{branch}").as_str());
        let conflicts = match self.merge_tree(worktree, &target_tip, "HEAD").await? {
            MergeTree::Clean(_) => Vec::new(),
            MergeTree::Conflicts(files) => files,
        };
        let target_checked_out_at = self
            .list_worktrees(repo)
            .await?
            .into_iter()
            .find(|w| w.branch.as_deref() == Some(target_branch))
            .map(|w| w.path.to_string_lossy().into_owned());
        let merge_blocked = if !head_ok {
            Some(format!("Il worktree non è sul branch {branch}"))
        } else if !conflicts.is_empty() {
            Some(format!(
                "Conflitti con {target_branch} in {} file",
                conflicts.len()
            ))
        } else if ahead == 0 && !dirty {
            Some(format!("Nessuna modifica da unire in {target_branch}"))
        } else {
            None
        };
        Ok(BranchStatus {
            target_branch: target_branch.to_owned(),
            ahead,
            behind,
            dirty,
            head_ok,
            conflicts,
            target_checked_out_at,
            merge_blocked,
        })
    }

    /// Squash merge steps 1–6 of spec §8.7: auto-commit, `merge-tree --write-tree`,
    /// `commit-tree -p T0`, then `update-ref` CAS (one retry) or `merge --ff-only` in the
    /// worktree that has the target checked out. `cleanup_warning` is left `None` (the caller
    /// removes the worktree). Errors: `BranchMismatch`, `GitIdentityMissing`,
    /// `TargetCheckoutDirty` (also for an operation in progress there), `Git`.
    pub async fn squash_merge(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        target_branch: &str,
        message: &str,
        attempt_id: &str,
    ) -> Result<MergeOutcome, AppError> {
        check_branch(branch)?;
        check_branch(target_branch)?;
        if message.trim().is_empty() {
            return Err(AppError::invalid("Il messaggio del merge è vuoto"));
        }
        if self.head_ref(worktree).await?.as_deref()
            != Some(format!("refs/heads/{branch}").as_str())
        {
            return Err(AppError::new(
                ErrorCode::BranchMismatch,
                format!("Il worktree non è sul branch {branch}"),
            ));
        }
        self.autocommit(worktree, SNAPSHOT_MESSAGE).await?;
        let source = self.branch_tip(repo, branch).await?;
        let msg = TempFile::new("atm-msg");
        let mut text = message.to_owned();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        tokio::fs::write(&msg.0, text).await?;
        let msg_path = super::utf8(&msg.0)?;
        let target_ref = format!("refs/heads/{target_branch}");
        let reflog = format!("atm: squash {attempt_id}");
        let write = RunOpts::default();

        for retry in [false, true] {
            let t0 = self.branch_tip(repo, target_branch).await?;
            let tree = match self.merge_tree(repo, &t0, &source).await? {
                MergeTree::Conflicts(files) => return Ok(MergeOutcome::Conflicts { files }),
                MergeTree::Clean(tree) => tree,
            };
            let t0_tree = format!("{t0}^{{tree}}");
            let t0_tree = self
                .run_ok(
                    repo,
                    &["rev-parse", "--verify", "--end-of-options", &t0_tree],
                    &RunOpts::read(),
                )
                .await?
                .text();
            if tree == t0_tree {
                return Ok(MergeOutcome::NothingToMerge);
            }
            if !self.has_identity(repo).await? {
                return Err(AppError::new(
                    ErrorCode::GitIdentityMissing,
                    "Configura user.name e user.email di git per creare il commit di merge",
                ));
            }
            // Writes only an object: no ref, no index, no hook.
            let commit = self
                .run_ok(
                    repo,
                    &["commit-tree", &tree, "-p", &t0, "-F", msg_path],
                    &RunOpts::read(),
                )
                .await?
                .text();
            let checkout = self
                .list_worktrees(repo)
                .await?
                .into_iter()
                .find(|w| w.branch.as_deref() == Some(target_branch));
            let Some(checkout) = checkout else {
                let args = [
                    "update-ref",
                    "-m",
                    &reflog,
                    "--end-of-options",
                    &target_ref,
                    &commit,
                    &t0,
                ];
                if self.run(repo, &args, &write).await?.code == 0 {
                    return Ok(merged(commit, MergeStrategy::UpdateRef));
                }
                if retry {
                    return Err(moved(target_branch));
                }
                continue;
            };
            let dir = checkout.path.as_path();
            if self.head(dir).await? != t0 {
                if retry {
                    return Err(moved(target_branch));
                }
                continue;
            }
            if self.operation_in_progress(dir).await? {
                return Err(AppError::new(
                    ErrorCode::TargetCheckoutDirty,
                    format!(
                        "In {} è in corso un'operazione git (merge, rebase, cherry-pick…): \
                         completala o annullala, poi riprova",
                        dir.display()
                    ),
                ));
            }
            let dirty = || {
                AppError::new(
                    ErrorCode::TargetCheckoutDirty,
                    format!(
                        "Il checkout di {target_branch} in {} ha modifiche locali sui file \
                         del merge: fai commit o annullale, poi riprova",
                        dir.display()
                    ),
                )
            };
            // Checked before git's own refusal, which still writes ORIG_HEAD.
            if self.overlaps_local_changes(dir, &t0, &commit).await? {
                return Err(dirty());
            }
            let args = [
                "merge",
                "--ff-only",
                "-q",
                "--no-autostash",
                "--no-verify-signatures",
                "--end-of-options",
                &commit,
            ];
            let out = self.run(dir, &args, &write).await?;
            if out.code == 0 {
                return Ok(merged(commit, MergeStrategy::FfCheckedOut));
            }
            if out.stderr.contains("would be overwritten by merge") {
                return Err(dirty());
            }
            return Err(failure(&args, &out));
        }
        Err(moved(target_branch))
    }

    /// `merge-tree --write-tree --name-only --no-messages -z <ours> <theirs>` in `dir`.
    async fn merge_tree(
        &self,
        dir: &Path,
        ours: &str,
        theirs: &str,
    ) -> Result<MergeTree, AppError> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            "--end-of-options",
            ours,
            theirs,
        ];
        let out = self.run(dir, &args, &RunOpts::read()).await?;
        let (tree, files) = parse::parse_merge_tree(&out.stdout);
        match out.code {
            0 => Ok(MergeTree::Clean(tree)),
            1 => Ok(MergeTree::Conflicts(files)),
            _ => Err(failure(&args, &out)),
        }
    }

    /// A path changed between `from` and `to` is modified, staged or untracked in `checkout`.
    async fn overlaps_local_changes(
        &self,
        checkout: &Path,
        from: &str,
        to: &str,
    ) -> Result<bool, AppError> {
        let read = RunOpts::read();
        let status = self
            .run_ok(
                checkout,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
                &read,
            )
            .await?;
        let local: HashSet<&[u8]> = parse::status_entries(&status.stdout)
            .into_iter()
            .flat_map(|(path, source)| [Some(path), source])
            .flatten()
            .collect();
        if local.is_empty() {
            return Ok(false);
        }
        let changed = self
            .run_ok(
                checkout,
                &[
                    "diff",
                    "--name-only",
                    "--no-renames",
                    "-z",
                    "--end-of-options",
                    from,
                    to,
                ],
                &read,
            )
            .await?;
        Ok(changed
            .stdout
            .split(|b| *b == 0)
            .any(|path| local.contains(path)))
    }

    async fn operation_in_progress(&self, checkout: &Path) -> Result<bool, AppError> {
        let git_dir = self
            .run_ok(
                checkout,
                &["rev-parse", "--absolute-git-dir"],
                &RunOpts::read(),
            )
            .await?
            .path();
        Ok(IN_PROGRESS.iter().any(|name| git_dir.join(name).exists()))
    }
}

fn merged(commit: String, strategy: MergeStrategy) -> MergeOutcome {
    MergeOutcome::Merged {
        commit,
        strategy,
        cleanup_warning: None,
    }
}

fn moved(target_branch: &str) -> AppError {
    AppError::git(format!(
        "{target_branch} è cambiato durante il merge: riprova"
    ))
}
