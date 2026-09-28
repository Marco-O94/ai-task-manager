//! Snapshot-tree diff (spec §8.6): the agent's index is never touched.

use std::io;
use std::path::Path;

use atm_types::{AppError, DiffResult, FileDiff};

use super::{
    DIFF_TIMEOUT, Git, MAX_DIFF_FILES, MAX_FILE_PATCH, MAX_TOTAL_PATCH, RunOpts, TempFile,
    check_branch, failure, parse,
};

const DIFF_FLAGS: [&str; 4] = ["--no-color", "--no-ext-diff", "--no-textconv", "-M"];

impl Git {
    /// Diff of `merge-base refs/heads/<target> HEAD` against a snapshot tree written through
    /// a copy of the worktree index (the agent's index is untouched), with the §8.6 budget.
    pub async fn snapshot_diff(
        &self,
        worktree: &Path,
        target_branch: &str,
    ) -> Result<DiffResult, AppError> {
        check_branch(target_branch)?;
        let read = RunOpts {
            timeout: Some(DIFF_TIMEOUT),
            ..RunOpts::read()
        };
        let target = format!("refs/heads/{target_branch}");
        let base = self
            .run_ok(
                worktree,
                &["merge-base", "--end-of-options", &target, "HEAD"],
                &read,
            )
            .await?
            .text();
        let snapshot_tree = self.snapshot_tree(worktree, &read).await?;
        let tree_diff = |format| {
            let mut args = vec!["diff"];
            args.extend(DIFF_FLAGS);
            args.extend(["-z", format, "--end-of-options", &base, &snapshot_tree]);
            args
        };
        let names = self
            .run_ok(worktree, &tree_diff("--name-status"), &read)
            .await?;
        let stats = self
            .run_ok(worktree, &tree_diff("--numstat"), &read)
            .await?;
        let stats = parse::parse_numstat(&stats.stdout);

        let mut result = DiffResult {
            base,
            snapshot_tree,
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            truncated: false,
        };
        let mut budget = MAX_TOTAL_PATCH;
        for (i, entry) in parse::parse_name_status(&names.stdout)
            .into_iter()
            .enumerate()
        {
            let (additions, deletions, binary) =
                stats.get(&entry.path).copied().unwrap_or_default();
            let mut file = FileDiff {
                path: entry.path,
                old_path: entry.old_path,
                status: entry.status,
                additions,
                deletions,
                binary,
                too_large: false,
                omitted: false,
                lines: Vec::new(),
            };
            if i >= MAX_DIFF_FILES || budget == 0 {
                file.omitted = true;
            } else if !binary {
                match self.file_patch(worktree, &result, &file, &read).await? {
                    None => file.too_large = true,
                    Some(patch) if patch.len() > budget => {
                        budget = 0;
                        file.omitted = true;
                    }
                    Some(patch) => {
                        budget -= patch.len();
                        file.lines = parse::parse_hunks(&String::from_utf8_lossy(&patch));
                    }
                }
            }
            result.truncated |= file.omitted;
            result.additions += file.additions;
            result.deletions += file.deletions;
            result.files.push(file);
        }
        Ok(result)
    }

    /// `add -A` + `write-tree` through a copy of the worktree index (the copy keeps the stat
    /// cache, so only changed files are rehashed).
    async fn snapshot_tree(&self, worktree: &Path, read: &RunOpts) -> Result<String, AppError> {
        let index = self
            .run_ok(
                worktree,
                &["rev-parse", "--path-format=absolute", "--git-path", "index"],
                read,
            )
            .await?
            .path();
        let tmp = TempFile::new("atm-idx");
        match tokio::fs::copy(&index, &tmp.0).await {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let opts = RunOpts {
            index_file: Some(tmp.0.clone()),
            ..read.clone()
        };
        self.run_ok(worktree, &["add", "-A"], &opts).await?;
        Ok(self.run_ok(worktree, &["write-tree"], &opts).await?.text())
    }

    /// One file's `-U3` patch; `None` over [`MAX_FILE_PATCH`].
    async fn file_patch(
        &self,
        worktree: &Path,
        diff: &DiffResult,
        file: &FileDiff,
        read: &RunOpts,
    ) -> Result<Option<Vec<u8>>, AppError> {
        let mut args = vec!["--literal-pathspecs", "diff"];
        args.extend(DIFF_FLAGS);
        args.extend([
            "-U3",
            "--end-of-options",
            &diff.base,
            &diff.snapshot_tree,
            "--",
            &file.path,
        ]);
        args.extend(file.old_path.as_deref());
        let (out, overflow) = self.exec(worktree, &args, read, MAX_FILE_PATCH).await?;
        if overflow {
            return Ok(None);
        }
        if out.code != 0 {
            return Err(failure(&args, &out));
        }
        Ok(Some(out.stdout))
    }
}
