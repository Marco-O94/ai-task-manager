//! Shared helpers of the atm-core integration tests (`mod common;`). Owner: M2-GIT, which
//! adds the hostile global gitconfig; the helpers below are usable by every test file.
#![allow(dead_code)] // each test binary uses a subset

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
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
/// Neither the user's nor the system gitconfig applies.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

/// Writes an executable `/bin/sh` script.
pub fn script(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("script dir")).expect("create script dir");
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod script");
}

/// Hooks git runs from `core.hooksPath` and from `hook.<name>.event` (githooks(5)).
pub const HOOK_EVENTS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "post-index-change",
];

/// The environment of a user whose global gitconfig runs code at every chance: every hook
/// (`core.hooksPath` and a config-defined `hook.evil`), `core.fsmonitor`, `diff.external`, a
/// textconv driver for every file, commit signing, plus options that change what git prints
/// (colors, quoting, prefixes, context, untracked files, renames). Each script appends its
/// name to `marker`, which must never appear when git runs through the app's runner.
///
/// `env` is meant for `Git::with_env` / `CoreConfig::extra_env`: it also carries
/// `LANG=it_IT.UTF-8` (git is localized on this machine) and variables the runner scrubs.
pub struct Hostile {
    pub home: PathBuf,
    pub gitconfig: PathBuf,
    pub marker: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

impl Hostile {
    pub fn new(dir: &Path) -> Hostile {
        let home = dir.join("home");
        let bin = dir.join("bin");
        let hooks = dir.join("hooks");
        let marker = dir.join("marker");
        let m = marker.display();
        for event in HOOK_EVENTS {
            script(
                &hooks.join(event),
                &format!("echo 'hooksPath {event}' >> '{m}'"),
            );
        }
        script(
            &bin.join("config-hook"),
            &format!("echo config-hook >> '{m}'"),
        );
        script(
            &bin.join("fsmonitor"),
            &format!("echo fsmonitor >> '{m}'\nexit 1"),
        );
        script(&bin.join("ext-diff"), &format!("echo ext-diff >> '{m}'"));
        script(
            &bin.join("textconv"),
            &format!("echo textconv >> '{m}'\ncat \"$1\""),
        );
        script(&bin.join("gpg"), &format!("echo gpg >> '{m}'\nexit 1"));
        std::fs::create_dir_all(&home).expect("create hostile home");
        let attributes = home.join("attributes");
        std::fs::write(&attributes, "* diff=evil\n").expect("write attributes");

        let b = bin.display();
        let events: String = HOOK_EVENTS
            .iter()
            .map(|e| format!("\tevent = {e}\n"))
            .collect();
        let gitconfig = home.join(".gitconfig");
        std::fs::write(
            &gitconfig,
            format!(
                "[core]\n\thooksPath = {hooks}\n\tfsmonitor = {b}/fsmonitor\n\
                 \tquotePath = true\n\tattributesFile = {attributes}\n\
                 [hook \"evil\"]\n\tcommand = {b}/config-hook\n{events}\
                 [diff]\n\texternal = {b}/ext-diff\n\trenames = false\n\tcontext = 10\n\
                 \tnoprefix = true\n\tsuppressBlankEmpty = true\n\
                 [diff \"evil\"]\n\ttextconv = {b}/textconv\n\
                 [color]\n\tui = always\n\tdiff = always\n\tstatus = always\n\tbranch = always\n\
                 [status]\n\tshowUntrackedFiles = no\n\
                 [commit]\n\tgpgSign = true\n[gpg]\n\tprogram = {b}/gpg\n\
                 [log]\n\tshowSignature = true\n\tdecorate = full\n\
                 [merge]\n\tautoStash = true\n\tverifySignatures = true\n\tff = false\n\
                 [maintenance]\n\tstrategy = incremental\n[gc]\n\tauto = 1\n",
                hooks = hooks.display(),
                attributes = attributes.display(),
            ),
        )
        .expect("write hostile gitconfig");

        let bogus = dir.join("not-a-repo");
        let env = [
            ("HOME", home.as_os_str()),
            ("GIT_CONFIG_GLOBAL", gitconfig.as_os_str()),
            ("LANG", "it_IT.UTF-8".as_ref()),
            ("LC_ALL", "it_IT.UTF-8".as_ref()),
            ("LANGUAGE", "it".as_ref()),
            // Scrubbed by the runner: left in place they would redirect or break every call.
            ("GIT_DIR", bogus.as_os_str()),
            ("GIT_WORK_TREE", bogus.as_os_str()),
            ("GIT_INDEX_FILE", bogus.as_os_str()),
            ("GIT_CONFIG_COUNT", "1".as_ref()),
            ("GIT_CONFIG_KEY_0", "core.hooksPath".as_ref()),
            ("GIT_CONFIG_VALUE_0", hooks.as_os_str()),
        ]
        .into_iter()
        .map(|(k, v)| (OsString::from(k), v.to_owned()))
        .collect();
        Hostile {
            home,
            gitconfig,
            marker,
            env,
        }
    }

    /// Panics with the scripts that ran, if any.
    pub fn assert_untouched(&self) {
        if let Ok(ran) = std::fs::read_to_string(&self.marker) {
            panic!("the hostile gitconfig ran code:\n{ran}");
        }
    }

    /// The system git with this global config and none of the app's hardening: the control
    /// proving the config is armed.
    pub fn plain_git(&self, dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("spawn git")
    }
}
