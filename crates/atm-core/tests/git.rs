//! M2-GIT acceptance (spec §8, §11.2): temp repos, a hostile global gitconfig and
//! `LANG=it_IT.UTF-8` behind every call of the hardened runner; each test ends by checking
//! that no hostile script ran.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use atm_core::git::{self, Git, RunOpts, WorktreeEntry};
use atm_types::{ErrorCode, FileStatus, LineKind, MergeOutcome, MergeStrategy, WorktreeState};
use sha2::{Digest, Sha256};

use common::{Hostile, git as sh};

const HARDENING: [&str; 12] = [
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

/// A temp dir with a hostile environment, a repo on `main` and a worktree root.
struct Fx {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    hostile: Hostile,
    git: Git,
    repo: PathBuf,
    root: PathBuf,
}

struct Attempt {
    id: String,
    branch: String,
    wt: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        Fx::named("repo", "~/worktrees")
    }

    fn named(repo: &str, root: &str) -> Fx {
        let tmp = common::tempdir();
        let dir = tmp.path().canonicalize().unwrap();
        let hostile = Hostile::new(&dir.join("hostile"));
        let repo = common::init_repo(&dir.join(repo));
        let root = git::resolve_worktree_root(root, &hostile.home).unwrap();
        let git = hardened(&hostile, None);
        Fx {
            _tmp: tmp,
            dir,
            hostile,
            git,
            repo,
            root,
        }
    }

    /// What `start_attempt` does (spec §8.4): branch, tip of `main`, locked worktree.
    async fn attempt(&self, title: &str) -> Attempt {
        let id = atm_core::new_id();
        let branch = self
            .git
            .unique_branch(&self.repo, &id, title)
            .await
            .unwrap();
        let base = self.git.branch_tip(&self.repo, "main").await.unwrap();
        let wt = self
            .git
            .add_worktree(
                &self.repo,
                &git::worktree_path(&self.root, &id),
                &branch,
                &base,
                &id,
            )
            .await
            .unwrap();
        Attempt { id, branch, wt }
    }

    async fn merge(&self, a: &Attempt, message: &str) -> Result<MergeOutcome, atm_types::AppError> {
        self.git
            .squash_merge(&self.repo, &a.wt, &a.branch, "main", message, &a.id)
            .await
    }

    fn rev(&self, rev: &str) -> String {
        sh(&self.repo, &["rev-parse", rev])
    }

    fn done(&self) {
        self.hostile.assert_untouched();
    }
}

/// The app's runner (the git on `PATH`, or `bin`) with the hostile environment.
fn hardened(hostile: &Hostile, bin: Option<PathBuf>) -> Git {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let bin = bin.unwrap_or_else(|| git::find_git(&path));
    Git::new(bin, path).with_env(hostile.env.clone())
}

/// The runner's git behind a wrapper that appends each call's argv (fields ended by 0x1f,
/// one call per line) to the returned log.
fn logged_git(fx: &Fx) -> (Git, PathBuf) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let log = fx.dir.join("calls.log");
    let wrapper = fx.dir.join("logged/git");
    common::script(
        &wrapper,
        &format!(
            "for a in \"$@\"; do printf '%s\\037' \"$a\"; done >> '{log}'\necho >> '{log}'\n\
             exec '{real}' \"$@\"",
            log = log.display(),
            real = git::find_git(&path).display()
        ),
    );
    (hardened(&fx.hostile, Some(wrapper)), log)
}

fn write(path: &Path, contents: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Every file under `dir` (symlinks by target), except the object store and the linked
/// worktrees' metadata.
fn tree_bytes(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if rel == Path::new(".git/objects") || rel == Path::new(".git/worktrees") {
                continue;
            }
            let meta = path.symlink_metadata().unwrap();
            if meta.is_dir() {
                walk(root, &path, out);
            } else if meta.is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                out.insert(rel, target.into_os_string().into_encoded_bytes());
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn sha256(path: &Path) -> Vec<u8> {
    Sha256::digest(std::fs::read(path).unwrap()).to_vec()
}

fn has_worktree(list: &[WorktreeEntry], path: &Path) -> bool {
    list.iter().any(|w| w.path == path)
}

// ---------------------------------------------------------------- pure functions

#[test]
fn names_and_slugs() {
    assert_eq!(git::slugify("Fix the Login bug!"), "fix-the-login-bug");
    assert_eq!(git::slugify(""), "task");
    assert_eq!(git::slugify("  -- !! "), "task");
    assert_eq!(git::slugify("Città è bella"), "citt-bella");
    assert_eq!(
        git::slugify("Implement the whole new settings page"),
        "implement-the-whole-new"
    );
    assert!(git::slugify(&"ab ".repeat(40)).len() <= 24);

    let id = "1B4E28BA-2fa1-11d2-883f-0016d3cca427";
    assert_eq!(git::branch_name(id, "Fix login"), "atm/1b4e28ba-fix-login");
    assert_eq!(
        git::worktree_path(Path::new("/r"), id),
        PathBuf::from(format!("/r/{id}"))
    );
    assert_eq!(
        git::turn_commit_message(3, "\n  Add a README  \nthen more"),
        "atm: turn 3: Add a README"
    );
    assert_eq!(
        git::turn_commit_message(1, &"x".repeat(80)),
        format!("atm: turn 1: {}", "x".repeat(60))
    );
    assert_eq!(git::turn_commit_message(2, " \n"), "atm: turn 2");
}

#[test]
fn parse_hunks_golden() {
    // Headers, two hunks, a blank context line printed empty (`diff.suppressBlankEmpty`),
    // `\ No newline at end of file`, then a second file whose headers must not become lines.
    let patch = "diff --git a/f.txt b/f.txt\nindex 1111111..2222222 100644\n--- a/f.txt\n\
                 +++ b/f.txt\n@@ -1,4 +1,4 @@ fn main\n one\n-two\n+TWO\n three\n\n\
                 @@ -10,2 +10,3 @@\n ten\n-eleven\n\\ No newline at end of file\n+eleven\n\
                 +twelve\ndiff --git a/g b/g\n--- a/g\n+++ b/g\n@@ -0,0 +1 @@\n+new\n";
    let rendered: String = git::parse_hunks(patch)
        .iter()
        .map(|l| {
            let no = |n: Option<u32>| n.map_or("-".to_owned(), |n| n.to_string());
            format!(
                "{:<7} {:>2} {:>2} |{}\n",
                format!("{:?}", l.kind),
                no(l.old_no),
                no(l.new_no),
                l.text
            )
        })
        .collect();
    insta::assert_snapshot!("parse_hunks", rendered);
    assert!(git::parse_hunks("diff --git a/x b/x\nBinary files differ\n").is_empty());
}

#[test]
fn parse_worktree_list_records() {
    let out = b"worktree /r\0HEAD aaa\0branch refs/heads/main\0\0\
                worktree /w t\0HEAD bbb\0branch refs/heads/atm/x-y\0locked atm:1 2\0\0\
                worktree /gone\0HEAD ccc\0detached\0prunable gitdir file points to non-existent location\0\0";
    let list = git::parse_worktree_list(out);
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].branch.as_deref(), Some("main"));
    assert!(!list[0].locked);
    assert_eq!(list[1].path, Path::new("/w t"));
    assert_eq!(list[1].branch.as_deref(), Some("atm/x-y"));
    assert!(list[1].locked && !list[1].prunable);
    assert_eq!(list[2].head.as_deref(), Some("ccc"));
    assert_eq!(list[2].branch, None);
    assert!(list[2].prunable);
    assert!(git::parse_worktree_list(b"").is_empty());
}

#[test]
fn forbidden_operations_absent_from_the_source() {
    // Spec §8.8; the logged run in `hostile_config_never_runs` checks the same at runtime.
    let sources = [
        include_str!("../src/git.rs"),
        include_str!("../src/git/diff.rs"),
        include_str!("../src/git/merge.rs"),
        include_str!("../src/git/parse.rs"),
    ];
    for src in sources {
        for op in [
            "reset", "push", "fetch", "pull", "stash", "checkout", "switch", "restore", "prune",
            "clean", "gc", "rebase",
        ] {
            assert!(!src.contains(&format!("\"{op}\"")), "{op} in git.rs");
        }
    }
}

// ---------------------------------------------------------------- runner

#[tokio::test]
async fn runner_forces_hardening_and_environment() {
    let fx = Fx::new();
    let rec = fx.dir.join("rec");
    let bin = rec.join("git");
    let r = rec.display();
    common::script(
        &bin,
        &format!(
            "case \"$*\" in *--get-regexp*) printf 'hook.evil.command\\0hook.b.x.event\\0merge.evil.driver\\0merge.ff\\0merge.x.name\\0'; exit 0;; esac\n\
             printf '%s\\n' \"$@\" > '{r}/argv'\n/usr/bin/env > '{r}/env'"
        ),
    );
    let extra: Vec<(OsString, OsString)> = [
        ("GIT_DIR", "/x"),
        ("GIT_WORK_TREE", "/x"),
        ("GIT_INDEX_FILE", "/x"),
        ("GIT_COMMON_DIR", "/x"),
        ("GIT_CONFIG_PARAMETERS", "'core.hookspath'='/x'"),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.hooksPath"),
        ("GIT_CONFIG_VALUE_0", "/x"),
        ("LANG", "it_IT.UTF-8"),
        ("LC_ALL", "it_IT.UTF-8"),
        ("HOME", "/h"),
        ("ATM_TEST_KEPT", "yes"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    let g = Git::new(bin, "/custom/bin".into()).with_env(extra);
    let env = || -> BTreeMap<String, String> {
        read(&rec.join("env"))
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    };

    let out = g
        .run(
            &fx.repo,
            &["status", "--porcelain"],
            &RunOpts {
                read_only: true,
                ..RunOpts::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(out.code, 0);
    let mut argv: Vec<String> = HARDENING.iter().map(|s| s.to_string()).collect();
    argv.extend(["-C".into(), fx.repo.display().to_string()]);
    argv.extend(["status".into(), "--porcelain".into()]);
    assert_eq!(read(&rec.join("argv")).lines().collect::<Vec<_>>(), argv);
    let e = env();
    for (k, v) in [
        ("PATH", "/custom/bin"),
        ("LC_ALL", "C"),
        ("LANGUAGE", "C"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_EDITOR", "true"),
        ("GIT_PAGER", "cat"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("LANG", "it_IT.UTF-8"),
        ("HOME", "/h"),
        ("ATM_TEST_KEPT", "yes"),
    ] {
        assert_eq!(e.get(k).map(String::as_str), Some(v), "{k}");
    }
    for k in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ] {
        assert!(!e.contains_key(k), "{k} leaked");
    }

    // A call that may write, and `merge-tree`: the config-defined hooks and merge drivers are
    // listed first and disabled.
    let disabled = |e: &BTreeMap<String, String>| {
        assert_eq!(e.get("GIT_CONFIG_COUNT").map(String::as_str), Some("3"));
        let overrides: Vec<(&str, &str)> = (0..3)
            .map(|i| {
                (
                    e[&format!("GIT_CONFIG_KEY_{i}")].as_str(),
                    e[&format!("GIT_CONFIG_VALUE_{i}")].as_str(),
                )
            })
            .collect();
        assert_eq!(
            overrides,
            [
                ("hook.b.x.enabled", "false"),
                ("hook.evil.enabled", "false"),
                ("merge.evil.driver", "/usr/bin/false"),
            ]
        );
    };
    let index = fx.dir.join("tmp-index");
    g.run(
        &fx.repo,
        &["add", "-A"],
        &RunOpts {
            index_file: Some(index.clone()),
            ..RunOpts::default()
        },
    )
    .await
    .unwrap();
    let e = env();
    assert_eq!(e.get("GIT_INDEX_FILE"), Some(&index.display().to_string()));
    assert!(!e.contains_key("GIT_OPTIONAL_LOCKS"));
    disabled(&e);
    g.run(
        &fx.repo,
        &["merge-tree", "--write-tree", "a", "b"],
        &RunOpts {
            read_only: true,
            ..RunOpts::default()
        },
    )
    .await
    .unwrap();
    let e = env();
    assert_eq!(e.get("GIT_OPTIONAL_LOCKS").map(String::as_str), Some("0"));
    disabled(&e);
    fx.done();
}

#[tokio::test]
async fn runner_kills_on_timeout_and_caps_output() {
    let fx = Fx::new();
    let slow = fx.dir.join("slow/git");
    common::script(&slow, "exec /bin/sleep 30");
    let started = Instant::now();
    let err = hardened(&fx.hostile, Some(slow))
        .run(
            &fx.repo,
            &["status"],
            &RunOpts {
                timeout: Some(Duration::from_millis(300)),
                read_only: true,
                index_file: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Git);
    assert!(started.elapsed() < Duration::from_secs(5));

    let loud = fx.dir.join("loud/git");
    common::script(&loud, "exec /usr/bin/head -c 70000000 /dev/zero");
    let err = hardened(&fx.hostile, Some(loud))
        .run(
            &fx.repo,
            &["status"],
            &RunOpts {
                read_only: true,
                ..RunOpts::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Git);
    assert!(err.message.contains("64 MiB"), "{err}");

    // LC_ALL=C despite LANG/LC_ALL=it_IT.UTF-8: messages are parseable English (localized
    // git prints "non è un repository Git" here).
    let plain = fx.dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let failing = fx
        .git
        .run_ok(&plain, &["rev-parse", "--git-dir"], &RunOpts::default())
        .await
        .unwrap_err();
    assert_eq!(failing.code, ErrorCode::Git);
    assert!(
        failing.message.contains("not a git repository"),
        "{failing}"
    );
    fx.done();
}

#[tokio::test]
async fn git_discovery_and_version_gate() {
    let fx = Fx::new();
    let not_exec = fx.dir.join("path1");
    write(&not_exec.join("git"), "not executable");
    let good = fx.dir.join("path2");
    common::script(&good.join("git"), "echo 'git version 2.30.1'");
    let path = std::env::join_paths([Path::new("relative"), &not_exec, &good]).unwrap();
    assert_eq!(git::find_git(&path), good.join("git"));
    assert_eq!(git::find_git("".as_ref()), PathBuf::from(git::FALLBACK_GIT));

    let version = fx.git.version().await.unwrap();
    let (major, minor) = {
        let mut p = version.split('.').map(|n| n.parse::<u32>().unwrap());
        (p.next().unwrap(), p.next().unwrap())
    };
    assert!((major, minor) >= (2, 38), "{version}");

    let old = hardened(&fx.hostile, Some(good.join("git")));
    assert_eq!(old.version().await.unwrap_err().code, ErrorCode::Git);
    let missing = hardened(&fx.hostile, Some(fx.dir.join("nope/git")));
    assert_eq!(missing.version().await.unwrap_err().code, ErrorCode::Git);
    fx.done();
}

// ---------------------------------------------------------------- repo, branches, names

#[tokio::test]
async fn validate_repo_rules() {
    let fx = Fx::new();
    let g = &fx.git;
    let invalid = |r: Result<git::RepoInfo, atm_types::AppError>| {
        assert_eq!(r.unwrap_err().code, ErrorCode::Invalid);
    };

    let plain = fx.dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    invalid(g.validate_repo(&plain, &fx.root).await);
    invalid(g.validate_repo(&fx.dir.join("missing"), &fx.root).await);

    let bare = fx.dir.join("bare.git");
    std::fs::create_dir_all(&bare).unwrap();
    sh(&bare, &["init", "-q", "--bare"]);
    invalid(g.validate_repo(&bare, &fx.root).await);

    let empty = fx.dir.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    sh(&empty, &["init", "-q"]);
    invalid(g.validate_repo(&empty, &fx.root).await);

    let linked = fx.dir.join("linked");
    sh(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "side",
            linked.to_str().unwrap(),
        ],
    );
    invalid(g.validate_repo(&linked, &fx.root).await);

    let inner = common::init_repo(&fx.root.join("inner"));
    invalid(g.validate_repo(&inner, &fx.root).await);
    invalid(g.validate_repo(&fx.repo, &fx.repo.join("worktrees")).await);

    std::fs::create_dir_all(fx.repo.join("sub/dir")).unwrap();
    let info = g
        .validate_repo(&fx.repo.join("sub/dir"), &fx.root)
        .await
        .unwrap();
    assert_eq!(info.toplevel, fx.repo);
    assert_eq!(info.current_branch.as_deref(), Some("main"));
    assert!(info.warnings.is_empty());

    std::fs::create_dir_all(fx.repo.join(".claude")).unwrap();
    write(&fx.repo.join(".mcp.json"), "{}");
    sh(&fx.repo, &["switch", "-q", "--detach"]);
    let info = g.validate_repo(&fx.repo, &fx.root).await.unwrap();
    assert_eq!(info.current_branch, None);
    assert_eq!(info.warnings.len(), 1);
    assert!(info.warnings[0].contains(".claude/") && info.warnings[0].contains(".mcp.json"));
    assert!(info.warnings[0].contains("Isolato"));
    fx.done();
}

#[tokio::test]
async fn branches_tips_and_collisions() {
    let fx = Fx::new();
    let g = &fx.git;
    sh(&fx.repo, &["branch", "feature/x"]);
    let list = g.list_branches(&fx.repo).await.unwrap();
    assert_eq!(list.branches, ["feature/x", "main"]);
    assert_eq!(list.current.as_deref(), Some("main"));

    assert_eq!(
        g.branch_tip(&fx.repo, "main").await.unwrap(),
        fx.rev("main")
    );
    assert_eq!(
        g.branch_tip(&fx.repo, "nope").await.unwrap_err().code,
        ErrorCode::NotFound
    );
    for bad in ["a..b", "-x", "a b", "x^{tree}", "HEAD@{1}", "", "x.lock"] {
        assert_eq!(
            g.branch_tip(&fx.repo, bad).await.unwrap_err().code,
            ErrorCode::Invalid,
            "{bad}"
        );
    }

    let id = "0123abcd-0000-4000-8000-000000000000";
    let first = g.unique_branch(&fx.repo, id, "Fix it").await.unwrap();
    assert_eq!(first, "atm/0123abcd-fix-it");
    sh(&fx.repo, &["branch", &first]);
    assert_eq!(
        g.unique_branch(&fx.repo, id, "Fix it").await.unwrap(),
        "atm/0123abcd-fix-it-2"
    );
    sh(&fx.repo, &["branch", "atm/0123abcd-fix-it-2"]);
    assert_eq!(
        g.unique_branch(&fx.repo, id, "Fix it").await.unwrap(),
        "atm/0123abcd-fix-it-3"
    );

    assert_eq!(
        g.delete_branch(&fx.repo, "feature/x")
            .await
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
    assert_eq!(
        g.delete_branch(&fx.repo, "main").await.unwrap_err().code,
        ErrorCode::Invalid
    );
    g.delete_branch(&fx.repo, &first).await.unwrap();
    let list = g.list_branches(&fx.repo).await.unwrap();
    assert!(!list.branches.contains(&first));
    assert!(list.branches.contains(&"feature/x".to_owned()));
    fx.done();
}

#[test]
fn worktree_root_rules() {
    let tmp = common::tempdir();
    let home = tmp.path().canonicalize().unwrap().join("home-ù");
    std::fs::create_dir_all(&home).unwrap();

    let root = git::resolve_worktree_root("~/wt-ròot/nested", &home).unwrap();
    assert_eq!(root, home.join("wt-ròot/nested"));
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(git::resolve_worktree_root("~", &home).unwrap(), home);
    let abs = home.join("abs");
    assert_eq!(
        git::resolve_worktree_root(abs.to_str().unwrap(), &home).unwrap(),
        abs
    );

    let err = git::resolve_worktree_root("~/with space", &home).unwrap_err();
    assert_eq!(err.code, ErrorCode::Invalid);
    assert!(!home.join("with space").exists());
    for bad in ["relative/wt", "~other/wt"] {
        assert_eq!(
            git::resolve_worktree_root(bad, &home).unwrap_err().code,
            ErrorCode::Invalid
        );
    }
    // A symlink that resolves to a path with spaces is rejected too.
    std::fs::create_dir_all(home.join("a b")).unwrap();
    std::os::unix::fs::symlink(home.join("a b"), home.join("link")).unwrap();
    assert_eq!(
        git::resolve_worktree_root("~/link", &home)
            .unwrap_err()
            .code,
        ErrorCode::Invalid
    );
}

// ---------------------------------------------------------------- hostile end to end

#[tokio::test]
async fn hostile_config_never_runs() {
    let fx = Fx::new();

    // Control: plain git with this config runs every kind of hostile script.
    let control = common::init_repo(&fx.dir.join("control"));
    sh(&control, &["switch", "-q", "-c", "side"]);
    write(&control.join("README.md"), "side\n");
    sh(&control, &["commit", "-q", "-am", "side"]);
    sh(&control, &["switch", "-q", "main"]);
    write(&control.join("README.md"), "main\n");
    sh(&control, &["commit", "-q", "-am", "main"]);
    write(&control.join("README.md"), "changed\n");
    fx.hostile.plain_git(&control, &["status"]);
    fx.hostile.plain_git(&control, &["diff"]);
    fx.hostile.plain_git(&control, &["diff", "--no-ext-diff"]);
    fx.hostile.plain_git(&control, &["commit", "-qam", "x"]);
    fx.hostile
        .plain_git(&control, &["merge-tree", "--write-tree", "main", "side"]);
    let ran = read(&fx.hostile.marker);
    for script in [
        "hooksPath pre-commit",
        "config-hook",
        "fsmonitor",
        "ext-diff",
        "textconv",
        "gpg",
        "merge-driver",
    ] {
        assert!(ran.contains(script), "control did not run {script}:\n{ran}");
    }
    std::fs::remove_file(&fx.hostile.marker).unwrap();

    // The whole lifecycle through the runner, every call logged by a wrapper.
    let (g, log) = logged_git(&fx);
    g.version().await.unwrap();
    g.validate_repo(&fx.repo, &fx.root).await.unwrap();
    g.list_branches(&fx.repo).await.unwrap();

    for (n, checked_out) in [(1, true), (2, false)] {
        if !checked_out {
            sh(&fx.repo, &["switch", "-q", "-c", "other"]);
        }
        let id = atm_core::new_id();
        let branch = g.unique_branch(&fx.repo, &id, "Hostile run").await.unwrap();
        let base = g.branch_tip(&fx.repo, "main").await.unwrap();
        let wt = g
            .add_worktree(
                &fx.repo,
                &git::worktree_path(&fx.root, &id),
                &branch,
                &base,
                &id,
            )
            .await
            .unwrap();
        write(&wt.join("README.md"), format!("hello\nturn {n}\n"));
        write(&wt.join(format!("new {n}.txt")), "new\n");
        let diff = g.snapshot_diff(&wt, "main").await.unwrap();
        assert_eq!(diff.files.len(), 2);
        let readme = &diff.files[0];
        assert_eq!(
            (readme.path.as_str(), readme.status),
            ("README.md", FileStatus::Modified)
        );
        assert!(readme.lines.iter().any(|l| l.kind == LineKind::Add));
        let status = g
            .branch_status(&fx.repo, &wt, &branch, "main")
            .await
            .unwrap();
        assert!(status.dirty && status.head_ok && status.conflicts.is_empty());
        let commit = g
            .autocommit(&wt, &git::turn_commit_message(1, "hostile"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(commit.files, 2);
        assert!(
            g.log_oneline(&wt, &base)
                .await
                .unwrap()
                .contains("atm: turn 1: hostile")
        );
        let outcome = g
            .squash_merge(
                &fx.repo,
                &wt,
                &branch,
                "main",
                "Hostile\n\nATM-Attempt: x",
                &id,
            )
            .await
            .unwrap();
        let expected = if checked_out {
            MergeStrategy::FfCheckedOut
        } else {
            MergeStrategy::UpdateRef
        };
        assert!(
            matches!(outcome, MergeOutcome::Merged { strategy, .. } if strategy == expected),
            "{outcome:?}"
        );
        g.remove_worktree(&fx.repo, &wt).await.unwrap();
        assert_eq!(
            g.reconcile_worktrees(&fx.repo, std::slice::from_ref(&wt))
                .await
                .unwrap(),
            [WorktreeState::Missing]
        );
        g.delete_branch(&fx.repo, &branch).await.unwrap();
    }
    fx.done();

    // Spec §8.1 and §8.8: every call hardened, nothing forbidden ever run.
    let allowed: BTreeSet<&str> = [
        "add",
        "branch",
        "check-ref-format",
        "commit",
        "commit-tree",
        "config",
        "diff",
        "for-each-ref",
        "log",
        "merge",
        "merge-base",
        "merge-tree",
        "rev-list",
        "rev-parse",
        "show-ref",
        "status",
        "symbolic-ref",
        "update-ref",
        "worktree",
        "write-tree",
    ]
    .into();
    let calls = read(&log);
    assert!(calls.lines().count() > 50);
    for line in calls.lines() {
        let args: Vec<&str> = line.split('\u{1f}').filter(|a| !a.is_empty()).collect();
        assert_eq!(args[..HARDENING.len()], HARDENING, "{args:?}");
        let mut rest = args[HARDENING.len()..].iter().copied();
        let mut command = Vec::new();
        while let Some(arg) = rest.next() {
            match arg {
                "-c" | "-C" => {
                    rest.next();
                }
                _ if arg.starts_with('-') && command.is_empty() => {}
                _ => command.push(arg),
            }
        }
        let Some(&sub) = command.first() else {
            continue; // --version
        };
        assert!(allowed.contains(sub), "unexpected git {sub}: {args:?}");
        match sub {
            "worktree" => assert!(["add", "remove", "unlock", "list"].contains(&command[1])),
            "branch" => assert!(command[1] == "-D" && command[2].starts_with("atm/")),
            "merge" => assert!(command.contains(&"--ff-only")),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- worktrees

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_concurrent_worktrees() {
    let fx = Arc::new(Fx::new());
    let mut set = tokio::task::JoinSet::new();
    for i in 0..10 {
        let fx = fx.clone();
        set.spawn(async move {
            let a = fx.attempt(&format!("Task {i}")).await;
            write(&a.wt.join(format!("file-{i}.txt")), format!("{i}\n"));
            let diff = fx.git.snapshot_diff(&a.wt, "main").await.unwrap();
            assert_eq!(diff.files.len(), 1);
            fx.git
                .autocommit(&a.wt, &git::turn_commit_message(1, "go"))
                .await
                .unwrap()
                .unwrap();
            let st = fx
                .git
                .branch_status(&fx.repo, &a.wt, &a.branch, "main")
                .await
                .unwrap();
            assert_eq!((st.ahead, st.dirty, st.head_ok), (1, false, true));
            a
        });
    }
    let attempts = set.join_all().await;
    let list = fx.git.list_worktrees(&fx.repo).await.unwrap();
    assert_eq!(list.len(), 11);
    for a in &attempts {
        let entry = list.iter().find(|w| w.path == a.wt).expect("listed");
        assert!(entry.locked);
        assert_eq!(entry.branch.as_deref(), Some(a.branch.as_str()));
    }
    let paths: Vec<PathBuf> = attempts.iter().map(|a| a.wt.clone()).collect();
    let states = fx.git.reconcile_worktrees(&fx.repo, &paths).await.unwrap();
    assert!(states.iter().all(|s| *s == WorktreeState::Present));

    let mut set = tokio::task::JoinSet::new();
    for path in paths.clone() {
        let fx = fx.clone();
        set.spawn(async move { fx.git.remove_worktree(&fx.repo, &path).await });
    }
    for result in set.join_all().await {
        result.unwrap();
    }
    assert_eq!(fx.git.list_worktrees(&fx.repo).await.unwrap().len(), 1);
    assert!(paths.iter().all(|p| !p.exists()));
    let branches = fx.git.list_branches(&fx.repo).await.unwrap().branches;
    assert_eq!(
        branches.iter().filter(|b| b.starts_with("atm/")).count(),
        10
    );
    fx.done();
}

#[tokio::test]
async fn paths_with_spaces_and_unicode() {
    let fx = Fx::named("My Repo città ü", "~/wt-ròot");
    assert!(fx.repo.ends_with("My Repo città ü"));
    let info = fx.git.validate_repo(&fx.repo, &fx.root).await.unwrap();
    assert_eq!(info.toplevel, fx.repo);

    let a = fx.attempt("Città è bella").await;
    assert!(a.branch.ends_with("-citt-bella"), "{}", a.branch);
    assert_eq!(a.wt, fx.root.join(&a.id));
    assert!(fx.root.to_str().unwrap().contains("wt-ròot"));

    write(&a.wt.join("dir with space/fïle ü.txt"), "ciao\n");
    std::fs::rename(a.wt.join("README.md"), a.wt.join("Lèggimi file.md")).unwrap();
    let diff = fx.git.snapshot_diff(&a.wt, "main").await.unwrap();
    let files: Vec<_> = diff
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.old_path.as_deref(), f.status))
        .collect();
    assert_eq!(
        files,
        [
            ("Lèggimi file.md", Some("README.md"), FileStatus::Renamed),
            ("dir with space/fïle ü.txt", None, FileStatus::Added),
        ]
    );
    let added = &diff.files[1].lines;
    assert_eq!(
        added.last().map(|l| (l.kind, l.new_no, l.text.as_str())),
        Some((LineKind::Add, Some(1), "ciao"))
    );

    let commit = fx
        .git
        .autocommit(&a.wt, "atm: turn 1: unicode")
        .await
        .unwrap();
    assert_eq!(commit.unwrap().files, 3);
    let outcome = fx.merge(&a, "Unicode").await.unwrap();
    assert!(matches!(
        outcome,
        MergeOutcome::Merged {
            strategy: MergeStrategy::FfCheckedOut,
            ..
        }
    ));
    assert_eq!(read(&fx.repo.join("dir with space/fïle ü.txt")), "ciao\n");
    assert!(fx.repo.join("Lèggimi file.md").exists());
    assert_eq!(sh(&fx.repo, &["status", "--porcelain"]), "");

    fx.git.remove_worktree(&fx.repo, &a.wt).await.unwrap();
    assert!(!a.wt.exists());
    assert!(!has_worktree(
        &fx.git.list_worktrees(&fx.repo).await.unwrap(),
        &a.wt
    ));
    fx.done();
}

#[tokio::test]
async fn user_prune_keeps_locked_worktrees() {
    let fx = Fx::new();
    let a = fx.attempt("Kept").await;
    let away = fx.dir.join("away");
    std::fs::rename(&a.wt, &away).unwrap();

    // Control: an unlocked worktree whose directory is gone is pruned.
    let plain = fx.dir.join("plain-wt");
    sh(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "plain",
            plain.to_str().unwrap(),
        ],
    );
    std::fs::remove_dir_all(&plain).unwrap();

    sh(&fx.repo, &["worktree", "prune"]);
    let list = fx.git.list_worktrees(&fx.repo).await.unwrap();
    assert!(!has_worktree(
        &list,
        &plain.canonicalize().unwrap_or(plain.clone())
    ));
    assert!(!list.iter().any(|w| w.branch.as_deref() == Some("plain")));
    let entry = list
        .iter()
        .find(|w| w.path == a.wt)
        .expect("locked worktree kept");
    assert!(entry.locked);
    assert_eq!(
        fx.git
            .reconcile_worktrees(&fx.repo, std::slice::from_ref(&a.wt))
            .await
            .unwrap(),
        [WorktreeState::Missing]
    );

    std::fs::rename(&away, &a.wt).unwrap();
    assert_eq!(
        fx.git
            .reconcile_worktrees(&fx.repo, std::slice::from_ref(&a.wt))
            .await
            .unwrap(),
        [WorktreeState::Present]
    );
    write(&a.wt.join("still.txt"), "works\n");
    assert!(
        fx.git
            .autocommit(&a.wt, "atm: turn 1: x")
            .await
            .unwrap()
            .is_some()
    );

    // Removal of a worktree whose directory is gone deletes only its own metadata.
    let b = fx.attempt("Gone").await;
    let c = fx.attempt("Also gone").await;
    std::fs::remove_dir_all(&b.wt).unwrap();
    std::fs::rename(&c.wt, fx.dir.join("c-away")).unwrap();
    fx.git.remove_worktree(&fx.repo, &b.wt).await.unwrap();
    let list = fx.git.list_worktrees(&fx.repo).await.unwrap();
    assert!(!has_worktree(&list, &b.wt));
    assert!(has_worktree(&list, &c.wt) && has_worktree(&list, &a.wt));
    assert!(
        fx.git
            .list_branches(&fx.repo)
            .await
            .unwrap()
            .branches
            .contains(&b.branch)
    );

    std::fs::rename(fx.dir.join("c-away"), &c.wt).unwrap();
    for w in [&a, &c] {
        fx.git.remove_worktree(&fx.repo, &w.wt).await.unwrap();
    }
    assert_eq!(fx.git.list_worktrees(&fx.repo).await.unwrap().len(), 1);
    fx.done();
}

#[tokio::test]
async fn add_worktree_refuses_existing_path_or_branch() {
    let fx = Fx::new();
    let base = fx.rev("main");
    let id = atm_core::new_id();
    let path = git::worktree_path(&fx.root, &id);
    write(&path.join("precious.txt"), "keep me\n");
    let err = fx
        .git
        .add_worktree(&fx.repo, &path, "atm/aaaa-x", &base, &id)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(read(&path.join("precious.txt")), "keep me\n");

    let id = atm_core::new_id();
    let err = fx
        .git
        .add_worktree(
            &fx.repo,
            &git::worktree_path(&fx.root, &id),
            "main",
            &base,
            &id,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);

    // A failing `worktree add` (unknown base) leaves nothing behind.
    let id = atm_core::new_id();
    let path = git::worktree_path(&fx.root, &id);
    let err = fx
        .git
        .add_worktree(&fx.repo, &path, "atm/bbbb-y", &"0".repeat(40), &id)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Git);
    assert!(!path.exists());
    assert_eq!(fx.git.list_worktrees(&fx.repo).await.unwrap().len(), 1);
    fx.done();
}

#[tokio::test]
async fn a_worktree_without_its_git_file_never_reaches_an_enclosing_repo() {
    let fx = Fx::new();
    // The worktree root sits under a $HOME that is itself a repository.
    let home = &fx.hostile.home;
    sh(home, &["init", "-q"]);
    write(&home.join("secret.key"), "secret\n");
    let a = fx.attempt("Lost gitfile").await;
    write(&a.wt.join("work.txt"), "work\n");
    std::fs::remove_file(a.wt.join(".git")).unwrap();

    let g = &fx.git;
    let codes = [
        g.autocommit(&a.wt, "atm: turn 1: x").await.map(drop),
        g.snapshot_diff(&a.wt, "main").await.map(drop),
        g.branch_status(&fx.repo, &a.wt, &a.branch, "main")
            .await
            .map(drop),
        fx.merge(&a, "Lost").await.map(drop),
        g.remove_worktree(&fx.repo, &a.wt).await,
    ]
    .map(|r| r.unwrap_err().code);
    assert_eq!(codes, [ErrorCode::WorktreeMissing; 5]);
    assert!(!home.join(".git/index").exists());
    assert_eq!(sh(home, &["count-objects"]), "0 objects, 0 kilobytes");
    assert_eq!(read(&a.wt.join("work.txt")), "work\n");
    fx.done();
}

#[tokio::test]
async fn removal_snapshots_on_the_atm_branch_and_never_drops_detached_work() {
    let fx = Fx::new();
    let a = fx.attempt("Snapshot").await;
    write(&a.wt.join("wip.txt"), "wip\n");
    fx.git.remove_worktree(&fx.repo, &a.wt).await.unwrap();
    assert!(!a.wt.exists());
    assert_eq!(
        sh(&fx.repo, &["log", "-1", "--format=%s", &a.branch]),
        git::SNAPSHOT_MESSAGE
    );
    assert_eq!(
        sh(&fx.repo, &["show", &format!("{}:wip.txt", a.branch)]),
        "wip"
    );

    // Changes, then commits, that only a detached HEAD holds block the removal.
    let b = fx.attempt("Detached").await;
    let tip = fx.rev(&b.branch);
    sh(&b.wt, &["switch", "-q", "--detach"]);
    write(&b.wt.join("b.txt"), "b\n");
    let refused = || async {
        let err = fx.git.remove_worktree(&fx.repo, &b.wt).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::BranchMismatch, "{err}");
        assert!(b.wt.join("b.txt").exists());
        assert_eq!(fx.rev(&b.branch), tip);
    };
    refused().await;
    sh(&b.wt, &["add", "b.txt"]);
    sh(&b.wt, &["commit", "-q", "-m", "detached work"]);
    refused().await;

    // Once a branch holds it, there is nothing to lose.
    sh(&b.wt, &["switch", "-q", "-c", "rescued"]);
    fx.git.remove_worktree(&fx.repo, &b.wt).await.unwrap();
    assert!(!b.wt.exists());
    assert_eq!(
        sh(&fx.repo, &["log", "-1", "--format=%s", "rescued"]),
        "detached work"
    );
    assert_eq!(fx.rev(&b.branch), tip);
    fx.done();
}

// ---------------------------------------------------------------- autocommit and diff

#[tokio::test]
async fn autocommit_and_fallback_identity() {
    let fx = Fx::new();
    let a = fx.attempt("Commit").await;
    assert_eq!(
        fx.git.autocommit(&a.wt, "atm: turn 1: x").await.unwrap(),
        None
    );
    write(&a.wt.join("README.md"), "changed\n");
    write(&a.wt.join("new.txt"), "new\n");
    let c = fx
        .git
        .autocommit(&a.wt, "atm: turn 1: first line")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.files, 2);
    assert_eq!(c.commit, fx.git.head(&a.wt).await.unwrap());
    assert_eq!(
        sh(&a.wt, &["log", "-1", "--format=%an <%ae>|%s"]),
        "Test <test@example.com>|atm: turn 1: first line"
    );
    assert_eq!(sh(&a.wt, &["status", "--porcelain"]), "");
    assert_eq!(
        fx.git.head_ref(&a.wt).await.unwrap(),
        Some(format!("refs/heads/{}", a.branch))
    );

    // No identity: atm/* commits use the fallback one; the squash commit refuses.
    sh(&fx.repo, &["config", "--unset", "user.name"]);
    sh(&fx.repo, &["config", "--unset", "user.email"]);
    let b = fx.attempt("No identity").await;
    write(&b.wt.join("b.txt"), "b\n");
    fx.git
        .autocommit(&b.wt, "atm: turn 1: b")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sh(&b.wt, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"]),
        "AI Task Manager <atm@localhost>|AI Task Manager <atm@localhost>"
    );
    let main = fx.rev("main");
    let err = fx.merge(&b, "Needs identity").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::GitIdentityMissing);
    assert_eq!(fx.rev("main"), main);

    sh(&b.wt, &["switch", "-q", "--detach"]);
    assert_eq!(fx.git.head_ref(&b.wt).await.unwrap(), None);
    assert_eq!(
        fx.merge(&b, "Detached").await.unwrap_err().code,
        ErrorCode::BranchMismatch
    );
    fx.done();
}

#[tokio::test]
async fn snapshot_diff_kinds_and_untouched_index() {
    let fx = Fx::new();
    let r = &fx.repo;
    write(&r.join("mod.txt"), "a\nb\n\nc\nd\ne\nf\ng\nh\ni\n");
    let renamed: String = (1..=20).map(|i| format!("rename me, line {i}\n")).collect();
    write(&r.join("ren_src.txt"), &renamed);
    write(&r.join("del.txt"), "bye\n");
    sh(r, &["add", "-A"]);
    sh(r, &["commit", "-q", "-m", "fixtures"]);
    let a = fx.attempt("Diff").await;
    let w = &a.wt;

    write(&w.join("committed.txt"), "in a commit\n");
    fx.git
        .autocommit(w, "atm: turn 1: c")
        .await
        .unwrap()
        .unwrap();
    write(&w.join("mod.txt"), "a\nb\n\nc\nD\ne\nf\ng\nh\ni\n");
    std::fs::create_dir_all(w.join("moved")).unwrap();
    std::fs::rename(w.join("ren_src.txt"), w.join("moved/renamed.txt")).unwrap();
    std::fs::remove_file(w.join("del.txt")).unwrap();
    write(&w.join("new.txt"), "untracked\n");
    write(&w.join("staged.txt"), "staged\n");
    sh(w, &["add", "staged.txt"]);
    write(&w.join("bin.dat"), [0u8, 159, 146, 150, 0, 255, 1, 2]);
    let big: String = (0..10_000).map(|i| format!("{i:>29}\n")).collect();
    write(&w.join("big.txt"), &big);

    let status_before = sh(w, &["status", "--porcelain=v1", "--untracked-files=all"]);
    let index = PathBuf::from(sh(
        w,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    ));
    let (mtime, hash) = (
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        sha256(&index),
    );

    let diff = fx.git.snapshot_diff(w, "main").await.unwrap();

    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        mtime
    );
    assert_eq!(sha256(&index), hash);
    assert_eq!(
        sh(w, &["status", "--porcelain=v1", "--untracked-files=all"]),
        status_before
    );
    assert_eq!(diff.base, fx.rev("main"));
    assert_eq!(diff.snapshot_tree.len(), 40);
    assert!(!diff.truncated);
    let summary: String = diff
        .files
        .iter()
        .map(|f| {
            format!(
                "{:?} {}{} +{} -{}{}{}{} lines={}\n",
                f.status,
                f.old_path
                    .as_deref()
                    .map(|o| format!("{o} -> "))
                    .unwrap_or_default(),
                f.path,
                f.additions,
                f.deletions,
                if f.binary { " binary" } else { "" },
                if f.too_large { " too_large" } else { "" },
                if f.omitted { " omitted" } else { "" },
                f.lines.len()
            )
        })
        .collect();
    insta::assert_snapshot!("snapshot_diff_files", summary);
    let modified = diff.files.iter().find(|f| f.path == "mod.txt").unwrap();
    let rendered: String = modified
        .lines
        .iter()
        .map(|l| format!("{:?} {:?} {:?} |{}\n", l.kind, l.old_no, l.new_no, l.text))
        .collect();
    insta::assert_snapshot!("snapshot_diff_modified_lines", rendered);
    assert_eq!(
        diff.additions,
        diff.files.iter().map(|f| f.additions).sum::<u32>()
    );
    assert_eq!(
        diff.deletions,
        diff.files.iter().map(|f| f.deletions).sum::<u32>()
    );
    assert_eq!(
        fx.git.snapshot_diff(w, "nope").await.unwrap_err().code,
        ErrorCode::NotFound
    );
    fx.done();
}

#[tokio::test]
async fn snapshot_diff_budget() {
    let fx = Fx::new();
    let a = fx.attempt("Many files").await;
    for i in 0..301 {
        write(&a.wt.join(format!("many/{i:03}.txt")), format!("{i}\n"));
    }
    let diff = fx.git.snapshot_diff(&a.wt, "main").await.unwrap();
    assert_eq!(diff.files.len(), 301);
    assert!(diff.truncated);
    assert!(
        diff.files[..300]
            .iter()
            .all(|f| !f.omitted && f.lines.len() == 2)
    );
    let last = &diff.files[300];
    assert!(last.omitted && last.lines.is_empty() && last.additions == 1);
    assert_eq!(diff.additions, 301);

    // 20 patches of ~250 KiB: each under the per-file cap, the total over 4 MiB.
    let b = fx.attempt("Large files").await;
    let line = format!("{}\n", "x".repeat(63));
    let content = line.repeat(240 * 1024 / 64);
    for i in 0..20 {
        write(&b.wt.join(format!("large/{i:02}.txt")), &content);
    }
    let diff = fx.git.snapshot_diff(&b.wt, "main").await.unwrap();
    assert!(diff.truncated);
    assert!(
        diff.files
            .iter()
            .all(|f| !f.too_large && f.additions == 3840)
    );
    let shown = diff.files.iter().take_while(|f| !f.omitted).count();
    assert_eq!(shown, 16);
    assert!(
        diff.files[shown..]
            .iter()
            .all(|f| f.omitted && f.lines.is_empty())
    );
    fx.done();
}

// ---------------------------------------------------------------- branch status and merge

#[tokio::test]
async fn merge_update_ref_when_target_not_checked_out() {
    let fx = Fx::new();
    sh(&fx.repo, &["switch", "-q", "-c", "other"]);
    let a = fx.attempt("Feature").await;
    write(&a.wt.join("feature.txt"), "feature\n");
    let t0 = fx.rev("main");
    let status = fx
        .git
        .branch_status(&fx.repo, &a.wt, &a.branch, "main")
        .await
        .unwrap();
    assert_eq!(status.target_checked_out_at, None);
    assert_eq!(status.merge_blocked, None);
    let checkout = tree_bytes(&fx.repo);

    let message = atm_types::merge_message("Add feature", "Why it matters", &a.id);
    let outcome = fx.merge(&a, &message).await.unwrap();
    let MergeOutcome::Merged {
        commit,
        strategy,
        cleanup_warning,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        (strategy, cleanup_warning),
        (MergeStrategy::UpdateRef, None)
    );
    assert_eq!(fx.rev("main"), commit);
    assert_eq!(fx.rev("main^"), t0);
    assert_eq!(sh(&fx.repo, &["log", "-1", "--format=%B", "main"]), message);
    assert_eq!(sh(&fx.repo, &["show", "main:feature.txt"]), "feature");
    assert_eq!(
        sh(&fx.repo, &["reflog", "-1", "--format=%gs", "main"]),
        format!("atm: squash {}", a.id)
    );
    // The user's checkout (on `other`) is untouched, logs aside.
    let after = tree_bytes(&fx.repo);
    let unchanged = |m: &BTreeMap<PathBuf, Vec<u8>>| {
        m.iter()
            .filter(|(p, _)| !p.starts_with(".git/logs") && !p.starts_with(".git/refs/heads"))
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(unchanged(&after), unchanged(&checkout));
    assert_eq!(
        fx.merge(&a, &message).await.unwrap(),
        MergeOutcome::NothingToMerge
    );
    fx.done();
}

/// The runner's git behind a wrapper that, at each `update-ref`, first moves `main` to the
/// next commit of `moves` (a concurrent writer racing the CAS).
fn racing_git(fx: &Fx, moves: &[&str]) -> Git {
    let dir = fx.dir.join("race");
    write(&dir.join("moves"), moves.join("\n") + "\n");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let real = git::find_git(&path);
    common::script(
        &dir.join("git"),
        &format!(
            "case \" $* \" in *\" update-ref \"*)\n\
             n=$(cat '{d}/count' 2>/dev/null || echo 0); n=$((n + 1)); echo $n > '{d}/count'\n\
             to=$(sed -n \"${{n}}p\" '{d}/moves')\n\
             if [ -n \"$to\" ]; then '{real}' -c core.hooksPath=/dev/null -C '{repo}' update-ref refs/heads/main \"$to\"; fi ;;\n\
             esac\nexec '{real}' \"$@\"",
            d = dir.display(),
            real = real.display(),
            repo = fx.repo.display()
        ),
    );
    hardened(&fx.hostile, Some(dir.join("git")))
}

#[tokio::test]
async fn merge_retries_once_when_the_cas_fails() {
    for moves_twice in [false, true] {
        let fx = Fx::new();
        let a = fx.attempt("Race").await;
        write(&a.wt.join("feature.txt"), "feature\n");
        // Two commits on top of `main` for the racer; `main` itself stays put.
        sh(&fx.repo, &["switch", "-q", "-c", "other"]);
        write(&fx.repo.join("x.txt"), "x\n");
        sh(&fx.repo, &["add", "x.txt"]);
        sh(&fx.repo, &["commit", "-q", "-m", "x"]);
        let x = fx.rev("HEAD");
        write(&fx.repo.join("y.txt"), "y\n");
        sh(&fx.repo, &["add", "y.txt"]);
        sh(&fx.repo, &["commit", "-q", "-m", "y"]);
        let y = fx.rev("HEAD");
        let moves: &[&str] = if moves_twice { &[&x, &y] } else { &[&x] };
        let g = racing_git(&fx, moves);

        let result = g
            .squash_merge(&fx.repo, &a.wt, &a.branch, "main", "Race", &a.id)
            .await;
        if moves_twice {
            let err = result.unwrap_err();
            assert_eq!(err.code, ErrorCode::Git);
            assert!(err.message.contains("è cambiato durante il merge"), "{err}");
            assert_eq!(fx.rev("main"), y, "the racer's commit is kept");
        } else {
            let MergeOutcome::Merged {
                commit,
                strategy: MergeStrategy::UpdateRef,
                ..
            } = result.unwrap()
            else {
                panic!("not merged by update-ref");
            };
            assert_eq!(fx.rev("main"), commit);
            assert_eq!(fx.rev("main^"), x, "retried on the new tip");
            let files = sh(&fx.repo, &["ls-tree", "--name-only", "main"]);
            assert_eq!(
                files.lines().collect::<Vec<_>>(),
                ["README.md", "feature.txt", "x.txt"]
            );
        }
        fx.done();
    }

    // A failure that is not a lost race (a stale lock) comes with git's reason.
    let fx = Fx::new();
    sh(&fx.repo, &["switch", "-q", "-c", "other"]);
    let a = fx.attempt("Locked").await;
    write(&a.wt.join("feature.txt"), "feature\n");
    let main = fx.rev("main");
    write(&fx.repo.join(".git/refs/heads/main.lock"), "");
    let err = fx.merge(&a, "Locked").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Git);
    assert!(err.message.contains("main.lock"), "{err}");
    assert_eq!(fx.rev("main"), main);
    fx.done();
}

#[tokio::test]
async fn merge_ff_only_when_target_checked_out_and_clean() {
    let fx = Fx::new();
    let a = fx.attempt("Checked out").await;
    write(&a.wt.join("feature.txt"), "feature\n");
    write(&fx.repo.join("untracked-elsewhere.txt"), "mine\n");
    let status = fx
        .git
        .branch_status(&fx.repo, &a.wt, &a.branch, "main")
        .await
        .unwrap();
    assert_eq!(
        status.target_checked_out_at,
        Some(fx.repo.display().to_string())
    );

    let outcome = fx.merge(&a, "Checked out").await.unwrap();
    let MergeOutcome::Merged {
        commit,
        strategy: MergeStrategy::FfCheckedOut,
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert_eq!(fx.rev("HEAD"), commit);
    assert_eq!(read(&fx.repo.join("feature.txt")), "feature\n");
    assert_eq!(read(&fx.repo.join("untracked-elsewhere.txt")), "mine\n");
    assert_eq!(
        sh(&fx.repo, &["status", "--porcelain"]),
        "?? untracked-elsewhere.txt"
    );
    fx.done();
}

#[tokio::test]
async fn merge_refuses_dirty_overlap_without_touching_the_checkout() {
    let fx = Fx::new();
    let a = fx.attempt("Overlap").await;
    write(&a.wt.join("README.md"), "hello\nfrom the agent\n");
    fx.git
        .autocommit(&a.wt, "atm: turn 1: x")
        .await
        .unwrap()
        .unwrap();
    write(&fx.repo.join("README.md"), "hello\nlocal edit\n");
    let main = fx.rev("main");
    let before = tree_bytes(&fx.repo);

    let err = fx.merge(&a, "Overlap").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::TargetCheckoutDirty, "{err}");
    assert_eq!(
        tree_bytes(&fx.repo),
        before,
        "a byte of the checkout changed"
    );
    assert_eq!(fx.rev("main"), main);

    // An operation in progress in the target checkout also blocks the merge.
    write(&fx.repo.join("README.md"), "hello\n");
    write(&fx.repo.join(".git/MERGE_HEAD"), format!("{main}\n"));
    let err = fx.merge(&a, "In progress").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::TargetCheckoutDirty);
    assert_eq!(fx.rev("main"), main);
    fx.done();
}

#[tokio::test]
async fn merge_divergent_target_without_conflicts() {
    let fx = Fx::new();
    let a = fx.attempt("Diverge").await;
    write(&a.wt.join("feature.txt"), "feature\n");
    write(&fx.repo.join("other.txt"), "other\n");
    sh(&fx.repo, &["add", "other.txt"]);
    sh(&fx.repo, &["commit", "-q", "-m", "user work"]);
    let z = fx.rev("main");
    let status = fx
        .git
        .branch_status(&fx.repo, &a.wt, &a.branch, "main")
        .await
        .unwrap();
    assert_eq!((status.behind, status.ahead, status.dirty), (1, 0, true));
    assert!(status.conflicts.is_empty() && status.merge_blocked.is_none());

    let outcome = fx.merge(&a, "Diverge").await.unwrap();
    let MergeOutcome::Merged { commit, .. } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(fx.rev("main"), commit);
    assert_eq!(fx.rev("main^"), z);
    assert_eq!(read(&fx.repo.join("feature.txt")), "feature\n");
    assert_eq!(read(&fx.repo.join("other.txt")), "other\n");
    assert_eq!(sh(&fx.repo, &["status", "--porcelain"]), "");
    fx.done();
}

#[tokio::test]
async fn merge_conflicts_touch_no_ref() {
    let fx = Fx::new();
    let a = fx.attempt("Conflict").await;
    write(&a.wt.join("README.md"), "agent\n");
    fx.git
        .autocommit(&a.wt, "atm: turn 1: x")
        .await
        .unwrap()
        .unwrap();
    write(&fx.repo.join("README.md"), "user\n");
    sh(&fx.repo, &["commit", "-q", "-am", "user"]);
    let (main, branch) = (fx.rev("main"), fx.rev(&a.branch));
    let checkout = tree_bytes(&fx.repo);

    let status = fx
        .git
        .branch_status(&fx.repo, &a.wt, &a.branch, "main")
        .await
        .unwrap();
    assert_eq!(status.conflicts, ["README.md"]);
    assert_eq!((status.ahead, status.behind), (1, 1));
    assert!(status.merge_blocked.is_some());

    let outcome = fx.merge(&a, "Conflict").await.unwrap();
    assert_eq!(
        outcome,
        MergeOutcome::Conflicts {
            files: vec!["README.md".into()]
        }
    );
    assert_eq!((fx.rev("main"), fx.rev(&a.branch)), (main, branch));
    assert_eq!(tree_bytes(&fx.repo), checkout);
    fx.done();
}

#[tokio::test]
async fn merge_nothing_to_merge() {
    let fx = Fx::new();
    let a = fx.attempt("Nothing").await;
    let status = fx
        .git
        .branch_status(&fx.repo, &a.wt, &a.branch, "main")
        .await
        .unwrap();
    assert!(!status.dirty && status.head_ok && status.ahead == 0);
    assert!(status.merge_blocked.is_some());
    let main = fx.rev("main");
    assert_eq!(
        fx.merge(&a, "Nothing").await.unwrap(),
        MergeOutcome::NothingToMerge
    );
    assert_eq!(fx.rev("main"), main);

    // The same change already on the target: still nothing to merge.
    let b = fx.attempt("Same change").await;
    write(&b.wt.join("same.txt"), "same\n");
    write(&fx.repo.join("same.txt"), "same\n");
    sh(&fx.repo, &["add", "same.txt"]);
    sh(&fx.repo, &["commit", "-q", "-m", "same"]);
    let main = fx.rev("main");
    assert_eq!(
        fx.merge(&b, "Same").await.unwrap(),
        MergeOutcome::NothingToMerge
    );
    assert_eq!(fx.rev("main"), main);
    fx.done();
}

async fn assert_target_blocked(fx: &Fx, a: &Attempt, at: &Path) {
    let status = fx
        .git
        .branch_status(&fx.repo, &a.wt, &a.branch, "main")
        .await
        .unwrap();
    assert_eq!(status.target_checked_out_at, Some(at.display().to_string()));
    assert!(status.merge_blocked.is_some(), "{status:?}");
    let main = fx.rev("main");
    let err = fx.merge(a, "Blocked").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::TargetCheckoutDirty, "{err}");
    assert_eq!(fx.rev("main"), main);
}

#[tokio::test]
async fn merge_refuses_a_target_being_rebased_bisected_or_stale() {
    let fx = Fx::new();
    let r = &fx.repo;
    for n in 1..=2 {
        write(&r.join(format!("c{n}.txt")), "c\n");
        sh(r, &["add", "-A"]);
        sh(r, &["commit", "-q", "-m", &format!("c{n}")]);
    }
    let a = fx.attempt("Busy target").await;
    write(&a.wt.join("feature.txt"), "feature\n");

    // The user's checkout stopped in the middle of a rebase of main: HEAD is detached.
    sh(
        r,
        &[
            "-c",
            "sequence.editor=sed -i.bak s/^pick/edit/",
            "rebase",
            "-q",
            "-i",
            "HEAD~1",
        ],
    );
    assert_eq!(fx.git.head_ref(r).await.unwrap(), None);
    assert_target_blocked(&fx, &a, r).await;
    sh(r, &["rebase", "--abort"]);

    // A bisect of main in a linked worktree of the user.
    sh(r, &["switch", "-q", "-c", "other"]);
    let user_wt = fx.dir.join("user wt");
    sh(
        r,
        &["worktree", "add", "-q", user_wt.to_str().unwrap(), "main"],
    );
    sh(&user_wt, &["bisect", "start", "main", "main~2"]);
    assert_eq!(fx.git.head_ref(&user_wt).await.unwrap(), None);
    assert_target_blocked(&fx, &a, &user_wt).await;
    sh(&user_wt, &["bisect", "reset"]);

    // main checked out in a worktree whose directory is gone.
    std::fs::remove_dir_all(&user_wt).unwrap();
    assert_target_blocked(&fx, &a, &user_wt).await;

    // Pruned, main is checked out nowhere.
    sh(r, &["worktree", "prune"]);
    let outcome = fx.merge(&a, "Free").await.unwrap();
    assert!(
        matches!(
            outcome,
            MergeOutcome::Merged {
                strategy: MergeStrategy::UpdateRef,
                ..
            }
        ),
        "{outcome:?}"
    );
    fx.done();
}

#[tokio::test]
async fn merge_refused_by_git_itself_changes_no_byte() {
    let fx = Fx::new();
    write(&fx.repo.join(".gitignore"), "secret.env\n");
    sh(&fx.repo, &["add", ".gitignore"]);
    sh(&fx.repo, &["commit", "-q", "-m", "ignore"]);
    write(&fx.dir.join("case-probe"), "");
    let case_insensitive = fx.dir.join("CASE-PROBE").exists();
    let (g, log) = logged_git(&fx);
    let ff_merges = || read(&log).matches("merge\u{1f}--ff-only").count();

    // What the agent adds, and what the user has at a path the merge does not name exactly:
    // the pre-check lets these through and git refuses (in English under LANG=it_IT.UTF-8).
    let mut cases = vec![
        ("file where a directory goes", "foo/bar.txt", "foo"),
        ("ignored file", "secret.env", "secret.env"),
    ];
    if case_insensitive {
        cases.push(("case-folding collision", "Notes.txt", "notes.txt"));
    }
    for (i, (case, theirs, mine)) in cases.into_iter().enumerate() {
        let a = fx.attempt("Refused").await;
        write(&a.wt.join(theirs), "agent\n");
        sh(&a.wt, &["add", "-f", theirs]);
        g.autocommit(&a.wt, "atm: turn 1: x")
            .await
            .unwrap()
            .unwrap();
        write(&fx.repo.join(mine), "user\n");
        if i == 1 {
            // An ORIG_HEAD from an earlier operation is kept as it was.
            sh(&fx.repo, &["update-ref", "ORIG_HEAD", "main~1"]);
        }
        let before = tree_bytes(&fx.repo);
        let merges = ff_merges();

        let err = g
            .squash_merge(&fx.repo, &a.wt, &a.branch, "main", "Refused", &a.id)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::TargetCheckoutDirty, "{case}: {err}");
        assert_eq!(ff_merges(), merges + 1, "{case}: git itself did not refuse");
        assert_eq!(tree_bytes(&fx.repo), before, "{case}: a byte changed");
        std::fs::remove_file(fx.repo.join(mine)).unwrap();
    }
    fx.done();
}
