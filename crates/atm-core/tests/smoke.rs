//! Compiles `tests/common` in every `cargo test` run (M1 has no other test binary) and pins
//! the fake CLI's version to the one the app is tested with.

mod common;

use std::process::Command;

#[test]
fn fake_claude_reports_the_tested_version() {
    let out = Command::new(common::fake_claude())
        .arg("--version")
        .output()
        .expect("run fake-claude");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        format!("{} (Claude Code)", atm_types::CLAUDE_TESTED_VERSION)
    );
}
