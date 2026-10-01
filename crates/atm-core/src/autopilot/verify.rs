//! The verification of an autopilot attempt (round of 2026-10-01): the project's
//! `verify_command`, read from the DB only (never from the repository), through `/bin/sh -c`
//! in the worktree, under the attempt's lock (no merge or follow-up runs meanwhile). It runs
//! the repository's code like the agent's Bash does (spec §10): scrubbed environment
//! ([`ChildEnv`]), a process group of its own, killed with its tree at the timeout; the
//! output goes to `logs/<attempt>/verify/` (0600, capped) and its tail to `verify_summary`.

use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use atm_types::{AppError, AttemptState, Id, VerifyState};

use crate::db::AttemptCtx;
use crate::{Inner, attempt_key, claude, guard, now_ms, runner};

/// Bytes of the output's tail kept in `attempts.verify_summary` (and sent to the agent).
pub(crate) const MAX_SUMMARY: usize = 8 << 10;
/// Cap of a verification's log file.
const MAX_VERIFY_LOG: u64 = 8 << 20;
/// Once the command ended, how long its output may take to reach EOF.
const DRAIN: Duration = Duration::from_secs(2);

/// The outcome of one verification.
pub(crate) struct Verdict {
    pub(crate) state: VerifyState,
    /// The commit verified.
    pub(crate) head: String,
    /// How the command ended, for the agent ("exited with code 1", …), or why it could not
    /// run (`Error`).
    pub(crate) how: String,
    /// The output's last [`MAX_SUMMARY`] bytes.
    pub(crate) tail: String,
}

/// Out of [`Autopilot::verifying`](super::Autopilot) on every path.
struct Verifying<'a>(&'a Inner, Id);

impl Drop for Verifying<'_> {
    fn drop(&mut self) {
        guard(&self.0.autopilot.verifying).remove(&self.1);
    }
}

impl Inner {
    /// Verifies the attempt's HEAD: `None` when there is nothing to verify (closed, a turn
    /// running, gone, the app closing). Without a command it counts as passed. The state goes
    /// to the DB (`running`, then the outcome) and `TaskCard::verifying` is set meanwhile. One
    /// cut by the app closing stays `running`: the next startup marks it `error` and says so.
    pub(super) async fn verify(self: &Arc<Self>, attempt_id: &str) -> Option<Verdict> {
        let _attempt = self.lock(attempt_key(attempt_id)).await;
        if self.closing.load(Ordering::SeqCst) {
            return None;
        }
        let ctx = self.db.attempt_ctx(attempt_id).ok()?;
        if ctx.attempt.state != AttemptState::Active || self.turn(attempt_id).is_some() {
            return None;
        }
        let error = |how: String| Verdict {
            state: VerifyState::Error,
            head: String::new(),
            how,
            tail: String::new(),
        };
        let worktree = match self.present_worktree(&ctx) {
            Ok(worktree) => worktree,
            Err(e) => return Some(error(e.message)),
        };
        let head = match self.git().await.head(worktree).await {
            Ok(head) => head,
            Err(e) => return Some(error(e.message)),
        };
        let (project, task) = (ctx.project.id.as_str(), ctx.task.id.as_str());
        let Some(command) = ctx.project.verify_command.clone() else {
            if let Err(e) =
                self.db
                    .finish_verify(attempt_id, VerifyState::Passed, None, &head, now_ms())
            {
                eprintln!("verify {attempt_id}: {e}");
            }
            self.emit_changed(Some(project), Some(task));
            return Some(Verdict {
                state: VerifyState::Passed,
                head,
                how: String::new(),
                tail: String::new(),
            });
        };
        guard(&self.autopilot.verifying).insert(attempt_id.to_owned(), None);
        let verifying = Verifying(self, attempt_id.to_owned());
        if let Err(e) = self.db.begin_verify(attempt_id, &head, now_ms()) {
            eprintln!("verify {attempt_id}: {e}");
        }
        self.emit_changed(Some(project), Some(task));
        let (state, how, tail) = match self.run_verify(&ctx, &command, worktree).await {
            Ok((true, how, tail)) => (VerifyState::Passed, how, tail),
            Ok((false, how, tail)) => (VerifyState::Failed, how, tail),
            Err(e) => (VerifyState::Error, e.message, String::new()),
        };
        if self.closing.load(Ordering::SeqCst) {
            return None;
        }
        let summary = Some(tail.as_str()).filter(|t| !t.is_empty());
        if let Err(e) = self
            .db
            .finish_verify(attempt_id, state, summary, &head, now_ms())
        {
            eprintln!("verify {attempt_id}: {e}");
        }
        drop(verifying);
        self.emit_changed(Some(project), Some(task));
        Some(Verdict {
            state,
            head,
            how,
            tail,
        })
    }

    /// Runs `command` and returns whether it passed, how it ended and the output's tail. A
    /// timeout kills the group and the tree (failed). Errors: `Io` (no log folder, spawn).
    async fn run_verify(
        &self,
        ctx: &AttemptCtx,
        command: &str,
        worktree: &Path,
    ) -> Result<(bool, String, String), AppError> {
        let io = |e: std::io::Error| AppError::io(format!("verifica: {e}"));
        let settings = self.db.settings()?;
        let tools = self.tools(false).await;
        let env = self
            .child_env(&tools, &settings)
            .for_attempt(worktree, &ctx.attempt.id);
        let dir = runner::attempt_log_dir(&self.config.data_dir, &ctx.attempt.id).join("verify");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(io)?;
        let log = dir.join(format!("{}.log", now_ms()));
        // One pipe for stdout and stderr: the log keeps their order.
        let (reader, writer) = std::io::pipe().map_err(io)?;
        let mut child = {
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.arg("-c")
                .arg(command)
                .current_dir(worktree)
                .process_group(0)
                .kill_on_drop(true)
                .stdin(Stdio::null())
                .stdout(writer.try_clone().map_err(io)?)
                .stderr(writer);
            env.apply(&mut cmd);
            // `cmd` (with its ends of the pipe) is dropped here: EOF comes with the children.
            cmd.spawn().map_err(io)?
        };
        let pgid = child.id().and_then(|id| i32::try_from(id).ok());
        // Recorded before `closing` is read, which `shutdown` sets before it kills the groups
        // recorded: one of the two sees the other.
        if let Some(group) = guard(&self.autopilot.verifying).get_mut(&ctx.attempt.id) {
            *group = pgid;
        }
        if let Some(pgid) = pgid
            && self.closing.load(Ordering::SeqCst)
        {
            let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
        }
        let mut output = tokio::task::spawn_blocking(move || collect(reader, &log));
        let secs = ctx.project.verify_timeout_secs;
        let status = tokio::time::timeout(Duration::from_secs(secs.into()), child.wait()).await;
        // What it left running ends with it, at the timeout and after a normal exit alike; the
        // group is then forgotten (its id may be reused).
        if let Some(pgid) = pgid {
            let _ = tokio::task::spawn_blocking(move || kill_group(pgid)).await;
            if let Some(group) = guard(&self.autopilot.verifying).get_mut(&ctx.attempt.id) {
                *group = None;
            }
        }
        let (passed, how) = match status {
            Ok(Ok(status)) => match status.code() {
                Some(0) => (true, "passed".to_owned()),
                Some(code) => (false, format!("exited with code {code}")),
                None => (false, "was killed by a signal".to_owned()),
            },
            Ok(Err(e)) => return Err(io(e)),
            Err(_) => {
                let _ = child.wait().await;
                (false, format!("timed out after {secs} s and was killed"))
            }
        };
        // A process that left the group may hold the pipe: the tail is then given up.
        let tail = match tokio::time::timeout(DRAIN, &mut output).await {
            Ok(Ok(tail)) => tail,
            _ => String::new(),
        };
        Ok((passed, how, tail))
    }
}

/// Kills the group `pgid` and what its members started in groups of their own (a server a
/// test suite ran with `setsid`, …): their pipe ends close with them. Blocking (`ps`).
pub(super) fn kill_group(pgid: i32) {
    let table = claude::process_table();
    let tree: Vec<i32> = table
        .iter()
        .filter(|p| p.pgid == pgid)
        .flat_map(|p| claude::tree_of(&table, p.pid))
        .map(|p| p.pid)
        .collect();
    let _ = claude::killpg(pgid, libc::SIGKILL);
    claude::kill_all(&tree, libc::SIGKILL);
}

/// Reads the output to EOF into the log at `path` (0600, at most [`MAX_VERIFY_LOG`] bytes;
/// best effort) and returns its last [`MAX_SUMMARY`] bytes as text.
fn collect(mut reader: std::io::PipeReader, path: &Path) -> String {
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .inspect_err(|e| eprintln!("{}: {e}", path.display()))
        .ok();
    let (mut written, mut tail, mut buf) = (0u64, Vec::new(), [0u8; 8192]);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let room = MAX_VERIFY_LOG.saturating_sub(written).min(n as u64) as usize;
        if room > 0
            && let Some(file) = log.as_mut()
            && file.write_all(&buf[..room]).is_ok()
        {
            written += room as u64;
        }
        tail.extend_from_slice(&buf[..n]);
        if tail.len() > 2 * MAX_SUMMARY {
            tail.drain(..tail.len() - MAX_SUMMARY);
        }
    }
    let text = String::from_utf8_lossy(&tail);
    let mut start = text.len().saturating_sub(MAX_SUMMARY);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].trim_end().to_owned()
}
