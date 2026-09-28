//! Shared helpers of the atm-core integration tests (`mod common;`). Owner: M2-GIT, which
//! adds the hostile global gitconfig; the helpers below are usable by every test file.
#![allow(dead_code)] // each test binary uses a subset

use std::path::{Path, PathBuf};
use std::process::Command;

/// The CLI double: tests never run the real `claude` (spec §11.1).
pub fn fake_claude() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-claude"))
}

pub fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Runs the system git in `dir` for test setup and returns trimmed stdout; panics on failure.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_owned()
}

/// A repo on branch `main` with one commit (`README.md`) and a local identity.
pub fn init_repo(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("create repo dir");
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    std::fs::write(dir.join("README.md"), "hello\n").expect("write README");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "init"]);
    dir.canonicalize().expect("canonicalize repo")
}
