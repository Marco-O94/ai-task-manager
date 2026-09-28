//! Branch status and squash merge (spec §8.7).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use atm_types::{AppError, BranchStatus, ErrorCode, MergeOutcome, MergeStrategy};

use super::{Git, RunOpts, SNAPSHOT_MESSAGE, TempFile, check_branch, failure, parse};

/// Files in the git dir of a checkout that name the branch being rebased or bisected there.
const REWRITING: &[&str] = &[
    "rebase-merge/head-name",
    "rebase-apply/head-name",
    "BISECT_START",
];

/// stderr of `merge --ff-only` refused because of the checkout's local files.
const LOCAL_FILES_REFUSAL: &[&str] = &[
    "would be overwritten by merge",
    "would be removed by merge",
    "would lose untracked files",
];

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

/// Where the target branch is checked out, and why that blocks the merge.
struct TargetCheckout {
    path: Option<PathBuf>,
    blocked: Option<String>,
}

impl Git {
    /// Spec §8.7: ahead/behind, dirty, `head_ok` (HEAD = `refs/heads/<branch>`), conflict
    /// preview, where the target is checked out (also mid-rebase or bisect), and why merge is
    /// blocked (running turns are the caller's). Errors: `NotFound` for a missing target,
    /// `WorktreeMissing`.
    pub async fn branch_status(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        target_branch: &str,
    ) -> Result<BranchStatus, AppError> {
        check_branch(branch)?;
        self.check_worktree(worktree).await?;
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
        let dirty = !self.status(worktree).await?.is_empty();
        let head_ok = self.head_ref(worktree).await?.as_deref()
            == Some(format!("refs/heads/{branch}").as_str());
        let conflicts = match self.merge_tree(worktree, &target_tip, "HEAD").await? {
            MergeTree::Clean(_) => Vec::new(),
            MergeTree::Conflicts(files) => files,
        };
        let checkout = self.target_checkout(repo, target_branch).await?;
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
            checkout.blocked
        };
        Ok(BranchStatus {
            target_branch: target_branch.to_owned(),
            ahead,
            behind,
            dirty,
            head_ok,
            conflicts,
            target_checked_out_at: checkout.path.map(|p| p.to_string_lossy().into_owned()),
            merge_blocked,
        })
    }

    /// Squash merge steps 1–6 of spec §8.7: auto-commit, `merge-tree --write-tree`,
    /// `commit-tree -p T0`, then `update-ref` CAS (one retry) or `merge --ff-only` in the
    /// worktree that has the target checked out. `cleanup_warning` is left `None` (the caller
    /// removes the worktree). Errors: `WorktreeMissing`, `BranchMismatch`,
    /// `GitIdentityMissing`, `TargetCheckoutDirty` (local changes on the merged files, an
    /// operation in progress in the target checkout, the target being rebased or bisected, or
    /// checked out in a directory that is gone), `Git`.
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
        self.check_worktree(worktree).await?;
        if self.head_ref(worktree).await?.as_deref()
            != Some(format!("refs/heads/{branch}").as_str())
        {
            return Err(AppError::new(
                ErrorCode::BranchMismatch,
                format!("Il worktree non è sul branch {branch}"),
            ));
        }
        self.commit_all(worktree, SNAPSHOT_MESSAGE).await?;
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
            let checkout = self.target_checkout(repo, target_branch).await?;
            if let Some(reason) = checkout.blocked {
                return Err(AppError::new(ErrorCode::TargetCheckoutDirty, reason));
            }
            let Some(dir) = checkout.path else {
                let args = [
                    "update-ref",
                    "-m",
                    &reflog,
                    "--end-of-options",
                    &target_ref,
                    &commit,
                    &t0,
                ];
                let out = self.run(repo, &args, &write).await?;
                if out.code == 0 {
                    return Ok(merged(commit, MergeStrategy::UpdateRef));
                }
                if retry {
                    // Not a lost race (e.g. a stale `<target>.lock`): git's reason.
                    if self.branch_tip(repo, target_branch).await? == t0 {
                        return Err(failure(&args, &out));
                    }
                    return Err(moved(target_branch));
                }
                continue;
            };
            let dir = dir.as_path();
            if self.head(dir).await? != t0 {
                if retry {
                    return Err(moved(target_branch));
                }
                continue;
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
            // Git's own refusal writes ORIG_HEAD: the common case is refused here, and for
            // the rest (a file where the merge needs a directory, a case-folding collision,
            // an ignored file) ORIG_HEAD is put back.
            if self.overlaps_local_changes(dir, &t0, &commit).await? {
                return Err(dirty());
            }
            let orig_head = self.orig_head(dir).await?;
            let args = [
                "merge",
                "--ff-only",
                "-q",
                "--no-autostash",
                "--no-verify-signatures",
                "--no-overwrite-ignore",
                "--end-of-options",
                &commit,
            ];
            let out = self.run(dir, &args, &write).await?;
            if out.code == 0 {
                return Ok(merged(commit, MergeStrategy::FfCheckedOut));
            }
            self.restore_orig_head(dir, orig_head.as_deref(), &t0)
                .await?;
            if LOCAL_FILES_REFUSAL.iter().any(|m| out.stderr.contains(m)) {
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
        let status = self.status(checkout).await?;
        let local: HashSet<&[u8]> = parse::status_entries(&status)
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
                &RunOpts::read(),
            )
            .await?;
        Ok(changed
            .stdout
            .split(|b| *b == 0)
            .any(|path| local.contains(path)))
    }

    /// Where `target` is checked out (see [`TargetCheckout`]). A rebase or bisect of the target
    /// detaches its checkout, which `worktree list` then does not show: it is found as git's
    /// own `is_worktree_being_rebased` / `_bisected` do, and blocks the merge as it blocks git.
    async fn target_checkout(&self, repo: &Path, target: &str) -> Result<TargetCheckout, AppError> {
        let listed = self
            .list_worktrees(repo)
            .await?
            .into_iter()
            .find(|w| w.branch.as_deref() == Some(target));
        if let Some(w) = listed {
            let blocked = if !w.path.is_dir() {
                Some(format!(
                    "{target} risulta in checkout in {}, che non esiste più: ripristina la \
                     cartella o esegui git worktree prune, poi riprova",
                    w.path.display()
                ))
            } else if self.operation_in_progress(&w.path).await? {
                Some(format!(
                    "In {} è in corso un'operazione git (merge, rebase, cherry-pick…): \
                     completala o annullala, poi riprova",
                    w.path.display()
                ))
            } else {
                None
            };
            return Ok(TargetCheckout {
                path: Some(w.path),
                blocked,
            });
        }
        let path = self.target_rewritten_at(repo, target).await?;
        let blocked = path.as_ref().map(|p| {
            format!(
                "In {} è in corso un rebase o un bisect di {target}: completalo o annullalo, \
                 poi riprova",
                p.display()
            )
        });
        Ok(TargetCheckout { path, blocked })
    }

    /// The checkout whose git dir names `target` in [`REWRITING`].
    async fn target_rewritten_at(
        &self,
        repo: &Path,
        target: &str,
    ) -> Result<Option<PathBuf>, AppError> {
        let common = self.common_dir(repo).await?;
        let linked = std::fs::read_dir(common.join("worktrees"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path());
        for git_dir in std::iter::once(common.clone()).chain(linked) {
            let rewriting = REWRITING.iter().any(|file| {
                std::fs::read(git_dir.join(file)).is_ok_and(|name| {
                    let name = name.trim_ascii();
                    name.strip_prefix(b"refs/heads/").unwrap_or(name) == target.as_bytes()
                })
            });
            if rewriting {
                return Ok(Some(if git_dir == common {
                    repo.to_path_buf()
                } else {
                    linked_worktree_path(&git_dir)
                }));
            }
        }
        Ok(None)
    }

    async fn orig_head(&self, checkout: &Path) -> Result<Option<String>, AppError> {
        let args = [
            "rev-parse",
            "-q",
            "--verify",
            "--end-of-options",
            "ORIG_HEAD",
        ];
        let out = self.run(checkout, &args, &RunOpts::read()).await?;
        Ok((out.code == 0).then(|| out.text()))
    }

    /// Sets ORIG_HEAD of `checkout` back to `old` (deletes it if `None`), only if it is still
    /// `written`; a no-op when git did not write it.
    async fn restore_orig_head(
        &self,
        checkout: &Path,
        old: Option<&str>,
        written: &str,
    ) -> Result<(), AppError> {
        if old == Some(written) {
            return Ok(());
        }
        let mut args = vec!["update-ref", "--no-deref"];
        match old {
            Some(old) => args.extend(["--end-of-options", "ORIG_HEAD", old, written]),
            None => args.extend(["-d", "--end-of-options", "ORIG_HEAD", written]),
        }
        self.run(checkout, &args, &RunOpts::default()).await?;
        Ok(())
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

/// The checkout of a linked worktree's admin dir (`<common>/worktrees/<id>`), from its
/// `gitdir` file (`<checkout>/.git`, possibly relative to the admin dir).
fn linked_worktree_path(admin: &Path) -> PathBuf {
    std::fs::read(admin.join("gitdir"))
        .ok()
        .and_then(|gitdir| {
            admin
                .join(OsStr::from_bytes(gitdir.trim_ascii_end()))
                .parent()
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| admin.to_path_buf())
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
