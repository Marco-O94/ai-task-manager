//! M5 (spec §11.2, §12.3, §13.3): the Core against the REAL Claude Code CLI, billed only to the
//! user's Claude subscription. Opt-in, never run by `scripts/check.sh`:
//!
//! ```sh
//! ATM_REAL_CLAUDE=1 cargo test -p atm-core --test real_cli -- --ignored --test-threads=1
//! ```
//!
//! Subscription guard. (1) Before every `claude -p` spawn: none of [`FORBIDDEN_VARS`] is set,
//! and `claude auth status --json` says `loggedIn`, `authMethod == "claude.ai"` and
//! `apiProvider == "firstParty"`. (2) Before every spawn that carries a user message, a free
//! preflight with the same argv and environment: `initialize` only, stdin closed on its answer
//! (no user message, so no API request), whose `account` must be first-party with a
//! subscription type ([`subscription_account`]); this also sees the user's settings, which the
//! environment check cannot. (3) During the turn: the first `system/init` must carry an
//! `apiKeySource` listed in `normalize::NO_API_KEY_SOURCES`, and no model output may come
//! before it; otherwise the turn is killed. Condition (3) only reacts: the CLI sends
//! `system/init` together with its first request, so it cannot prevent that one. Any trip is
//! sticky: every later test of the binary refuses to start, and so does every later run until
//! the marker `real_cli.TRIPPED` in `CARGO_TARGET_TMPDIR` is deleted by hand. The tests
//! serialize on a lock whatever the libtest flags. Every approval outside the step's allowlist
//! is denied with `interrupt` and fails the run. Quota: model `sonnet`, effort `low`, tiny
//! prompts, 8 real turns in a full run, no retries.
//!
//! Every project lives in a temp dir removed at the end: a local clone of the toy repo
//! (`ATM_REAL_CLAUDE_REPO`, default `~/Desktop/Repositories/test-rust`, only read; the clone's
//! `origin` becomes a throwaway bare repo) or a repo made on the spot. Sanitized raw logs (email,
//! org, home, user, host and temp paths replaced; hook outputs redacted) and the observations go
//! to `ATM_REAL_CLAUDE_CAPTURE` (default `target/tmp/real_cli`); `tests/fixtures/real/` holds the
//! M5 capture, replayed without the CLI by `tests/real_fixtures.rs`.

mod common;

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use atm_core::claude::{self, ChildEnv, TurnArgs};
use atm_core::db::{Db, ProcessRow};
use atm_core::{
    AppEvent, Core, CoreConfig, Notify, TranscriptSink, normalize, now_ms, runner, wire,
};
use atm_types::*;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::watch;
use tokio::time::Instant;

/// Opt-in switch: without `ATM_REAL_CLAUDE=1` every test returns at once.
const ENABLE: &str = "ATM_REAL_CLAUDE";
/// Any of these set (even empty) could bill an API key or a third-party provider: no run.
const FORBIDDEN_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "ANTHROPIC_BASE_URL",
];
/// The parent's Claude Code variables are dropped (an app started from the Finder has none),
/// except this one, which selects the login.
const KEEP_CLAUDE_VARS: &[&str] = &["CLAUDE_CONFIG_DIR"];
const MODEL: &str = "sonnet";
/// Bound of one real turn (the model, the user's hooks, the network).
const TURN: Duration = Duration::from_secs(240);
const CODEWORD: &str = "PAPAYA-42";
const KEYWORD: &str = "TANGERINE-7031";
/// Stand-in for the temp root in captures (the worktree becomes `/tmp/atm-real/wt/<id>`).
const TMP_ROOT: &str = "/tmp/atm-real";
/// Bound of a preflight spawn (the user's `SessionStart` hooks run before the answer).
const PREFLIGHT: Duration = Duration::from_secs(90);
/// The sticky trip marker, in `CARGO_TARGET_TMPDIR`.
const TRIP_FILE: &str = "real_cli.TRIPPED";

// ---- guard --------------------------------------------------------------------------------

/// Serializes the real-CLI tests whatever `--test-threads` says: `guard` mutates the process
/// environment, and turns must never run in parallel.
static SERIAL: Mutex<()> = Mutex::new(());
/// Set by any trip of the guard (see [`trip`]); also persisted as [`TRIP_FILE`].
static TRIPPED: AtomicBool = AtomicBool::new(false);

fn trip_marker() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(TRIP_FILE)
}

/// Records a trip of the guard for this process and for every later run.
fn trip(why: &str) {
    TRIPPED.store(true, Ordering::SeqCst);
    let text = format!(
        "The M5 subscription guard tripped: {why}\n\nFix the configuration (see the module docs \
         of tests/real_cli.rs), then delete this file to allow another run.\n"
    );
    if let Err(e) = std::fs::write(trip_marker(), text) {
        eprintln!("[M5] trip marker not written: {e}");
    }
}

/// Panics once the guard has tripped, in this process or in an earlier run.
fn assert_not_tripped() {
    let marker = trip_marker();
    assert!(
        !TRIPPED.load(Ordering::SeqCst) && !marker.exists(),
        "M5 guard tripped earlier: nothing runs until the configuration is fixed and {} is \
         deleted",
        marker.display()
    );
}

/// The [`SERIAL`] lock: every real-CLI test binds it first and holds it to its end.
type Serial = MutexGuard<'static, ()>;

/// Checked once per test before any runtime exists, then before every spawn.
struct Guard {
    claude: PathBuf,
    version: String,
    path: OsString,
    /// Added to every child (hermetic git for the app and for the agent alike).
    extra_env: Vec<(OsString, OsString)>,
    /// Literal replacements for captures, longest first; never printed.
    secrets: Vec<(String, String)>,
    capture: PathBuf,
    turns: AtomicU32,
    cost: Mutex<f64>,
}

/// `None` (test skipped) without `ATM_REAL_CLAUDE=1`. Panics when the environment or the login
/// could bill anything but the subscription. The caller holds the returned [`Serial`] to its end.
fn guard() -> Option<(Serial, Arc<Guard>)> {
    // Before any read of the environment: another test may be changing it.
    let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    if std::env::var(ENABLE).as_deref() != Ok("1") {
        eprintln!("real_cli: skipped (set {ENABLE}=1 to run it against the real claude)");
        return None;
    }
    assert_not_tripped();
    let set: Vec<&str> = FORBIDDEN_VARS
        .iter()
        .copied()
        .filter(|v| std::env::var_os(v).is_some())
        .collect();
    assert!(
        set.is_empty(),
        "M5 guard: {set:?} set in the environment: refusing to run (subscription only)"
    );
    let nested: Vec<OsString> = std::env::vars_os()
        .map(|(k, _)| k)
        .filter(|k| {
            k.to_str().is_some_and(|k| {
                (k.starts_with("CLAUDE") || k.starts_with("ANTHROPIC"))
                    && !KEEP_CLAUDE_VARS.contains(&k)
            })
        })
        .collect();
    for k in nested {
        // SAFETY: runs under `SERIAL`, before this test builds any runtime or thread: no other
        // test of this binary reads the environment meanwhile (only these tests touch it).
        unsafe { std::env::remove_var(k) };
    }
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let claude = std::env::var_os("ATM_REAL_CLAUDE_PATH")
        .map_or_else(|| home.join(".local/bin/claude"), PathBuf::from);
    assert!(claude.is_file(), "no claude at {}", claude.display());
    let out = std::process::Command::new(&claude)
        .arg("--version")
        .output()
        .expect("claude --version");
    let version = claude::parse_version(&String::from_utf8_lossy(&out.stdout))
        .expect("claude --version: unexpected output");
    let out = std::process::Command::new(&claude)
        .args(["auth", "status", "--json"])
        .output()
        .expect("claude auth status");
    let auth = subscription_only(out.status.code(), &out.stdout);
    let capture = std::env::var_os("ATM_REAL_CLAUDE_CAPTURE").map_or_else(
        || PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("real_cli"),
        PathBuf::from,
    );
    std::fs::create_dir_all(&capture).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let extra_env = [
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)))
    .to_vec();
    let guard = Arc::new(Guard {
        secrets: secrets(&home, &auth),
        claude,
        version,
        path,
        extra_env,
        capture,
        turns: AtomicU32::new(0),
        cost: Mutex::new(0.0),
    });
    Some((serial, guard))
}

/// What `auth status` says about the account: kept in memory for the sanitizer only.
struct Auth {
    email: Option<String>,
    org_id: Option<String>,
    org_name: Option<String>,
}

/// Panics unless `auth status` exited 0 with a claude.ai (subscription) first-party login.
fn subscription_only(code: Option<i32>, stdout: &[u8]) -> Auth {
    let v: Value = serde_json::from_slice(stdout).unwrap_or(Value::Null);
    assert!(
        code == Some(0) && v["loggedIn"] == true,
        "M5 guard: claude is not logged in (auth status exit {code:?})"
    );
    assert!(
        v["authMethod"] == "claude.ai" && v["apiProvider"] == "firstParty",
        "M5 guard: not a Claude subscription login (authMethod {}, apiProvider {}): refusing",
        v["authMethod"],
        v["apiProvider"]
    );
    let field = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    Auth {
        email: field("email"),
        org_id: field("orgId"),
        org_name: field("orgName"),
    }
}

impl Guard {
    /// The environment of every child, exactly as the Core builds it (spec §7.2).
    fn env(&self) -> ChildEnv {
        let base: Vec<(OsString, OsString)> = std::env::vars_os()
            .chain(self.extra_env.iter().cloned())
            .collect();
        ChildEnv::new(base, &self.path, false)
    }

    /// Condition (1) of the guard, before every spawn.
    async fn check_auth(&self) {
        assert_not_tripped();
        let set: Vec<&str> = FORBIDDEN_VARS
            .iter()
            .copied()
            .filter(|v| std::env::var_os(v).is_some())
            .collect();
        assert!(
            set.is_empty(),
            "M5 guard: {set:?} appeared in the environment"
        );
        let mut cmd = tokio::process::Command::new(&self.claude);
        cmd.args(["auth", "status", "--json"])
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        self.env().apply(&mut cmd);
        let out = tokio::time::timeout(Duration::from_secs(20), cmd.output())
            .await
            .expect("claude auth status: timeout")
            .expect("claude auth status");
        subscription_only(out.status.code(), &out.stdout);
    }

    /// Conditions (1) and (2), before a spawn that carries a user message: `argv` and `env` are
    /// the turn's (the session flag is replaced by a fresh `--session-id`, so the preflight
    /// never touches the turn's session).
    async fn before_turn(&self, argv: &[String], cwd: &Path, env: &ChildEnv) {
        self.check_auth().await;
        let session = format!("--session-id={}", uuid::Uuid::new_v4());
        let argv: Vec<String> = argv
            .iter()
            .map(|a| {
                if a.starts_with("--resume=") || a.starts_with("--session-id=") {
                    session.clone()
                } else {
                    a.clone()
                }
            })
            .collect();
        let run = spawn_raw(
            &argv,
            cwd,
            env,
            &[initialize()],
            PREFLIGHT,
            false,
            close_on_answer,
        )
        .await;
        let answer = run
            .json()
            .into_iter()
            .find(|v| v["type"] == "control_response")
            .map(|v| v["response"].clone())
            .unwrap_or_default();
        if let Err(why) = subscription_account(&answer) {
            trip(&format!("preflight: {why}"));
            panic!("M5 guard (preflight): {why}: refusing to send any user message");
        }
    }

    fn count_turn(&self) {
        let n = self.turns.fetch_add(1, Ordering::SeqCst) + 1;
        eprintln!("[M5] real turn #{n}");
    }

    fn add_cost(&self, usd: Option<f64>) {
        *self.cost.lock().unwrap() += usd.unwrap_or(0.0);
    }

    fn sanitizer(&self, roots: &[&Path]) -> Sanitizer {
        let mut pairs = self.secrets.clone();
        let tmpdir = std::env::temp_dir();
        for root in roots.iter().copied().chain([tmpdir.as_path()]) {
            let to = if root == tmpdir { "/tmp" } else { TMP_ROOT };
            let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            for form in [root.to_path_buf(), canonical] {
                let form = form.to_string_lossy().trim_end_matches('/').to_owned();
                pairs.push((dash(&form), dash(to)));
                pairs.push((form, to.to_owned()));
            }
        }
        Sanitizer::new(pairs)
    }
}

/// Condition (2) on the `initialize` answer (the `response` of the `control_response`): success,
/// an `account` with `apiProvider == "firstParty"` and a non-empty `subscriptionType` (what M5
/// saw with the claude.ai login), and no `apiKeySource` / `tokenSource` other than `none`.
fn subscription_account(answer: &Value) -> Result<(), String> {
    if answer["subtype"] != "success" {
        return Err(format!(
            "initialize not answered with success ({})",
            answer["subtype"]
        ));
    }
    let account = &answer["response"]["account"];
    if !account.is_object() {
        return Err("the initialize answer names no account".into());
    }
    if account["apiProvider"] != "firstParty" {
        return Err(format!(
            "apiProvider {}, not firstParty",
            account["apiProvider"]
        ));
    }
    if !account["subscriptionType"]
        .as_str()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err("no subscriptionType: not a subscription login".into());
    }
    for key in ["apiKeySource", "tokenSource"] {
        if let Some(source) = account.get(key)
            && source != "none"
        {
            return Err(format!("{key} {source}"));
        }
    }
    Ok(())
}

/// Sanitizer pairs for the account, home, user, hosts and git identities.
fn secrets(home: &Path, auth: &Auth) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut add = |from: Option<String>, to: &str| {
        if let Some(from) = from.filter(|f| f.trim().len() >= 3) {
            pairs.push((from, to.to_owned()));
        }
    };
    add(auth.email.clone(), "<email>");
    add(auth.org_id.clone(), "<org-id>");
    add(auth.org_name.clone(), "<org>");
    let home_str = home.to_string_lossy().into_owned();
    add(Some(dash(&home_str)), "-HOME");
    add(Some(home_str), "<HOME>");
    add(
        home.file_name().map(|u| u.to_string_lossy().into_owned()),
        "<user>",
    );
    let host = std::process::Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    add(host.split('.').next().map(str::to_owned), "<host>");
    add(Some(host), "<host>");
    for key in ["user.name", "user.email"] {
        let out = std::process::Command::new("git")
            .args(["config", "--global", key])
            .output();
        let value = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        add(
            value.ok(),
            if key == "user.name" {
                "<author>"
            } else {
                "<email>"
            },
        );
    }
    if let Some(toy) = toy_repo() {
        let out = toy_git(&toy, &["log", "-1", "--format=%an%n%ae%n%cn%n%ce"]);
        for (i, line) in out.unwrap_or_default().lines().enumerate() {
            add(
                Some(line.to_owned()),
                if i % 2 == 0 { "<author>" } else { "<email>" },
            );
        }
    }
    pairs
}

/// The CLI's project-directory encoding: every non-alphanumeric character becomes `-`.
fn dash(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The toy repo (only ever read and cloned), if present.
fn toy_repo() -> Option<PathBuf> {
    let path = std::env::var_os("ATM_REAL_CLAUDE_REPO").map_or_else(
        || {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join("Desktop/Repositories/test-rust")
        },
        PathBuf::from,
    );
    path.join(".git").exists().then_some(path)
}

/// A read-only git command in the toy repo: its trimmed stdout, or its stderr. With
/// `--no-optional-locks` (and `GIT_OPTIONAL_LOCKS=0`) `git status` never refreshes, hence never
/// rewrites, `.git/index`: the harness writes nothing there.
fn toy_git(toy: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(toy)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

/// HEAD, refs and status of the toy repo: equal before and after a run.
fn toy_state(toy: &Path) -> String {
    [
        "rev-parse HEAD",
        "for-each-ref",
        "status --porcelain",
        "worktree list",
    ]
    .iter()
    .map(|args| {
        toy_git(toy, &args.split(' ').collect::<Vec<_>>())
            .unwrap_or_else(|e| panic!("git {args} in the toy repo: {e}"))
    })
    .collect::<Vec<_>>()
    .join("\n")
}

// ---- sanitizer ----------------------------------------------------------------------------

struct Sanitizer {
    pairs: Vec<(String, String)>,
}

/// Catalog arrays trimmed in captures (the user's skills, agents, plugins, models).
const TRIMMED: &[&str] = &[
    "commands",
    "agents",
    "models",
    "slash_commands",
    "skills",
    "plugins",
    "available_output_styles",
];
const KEEP_ITEMS: usize = 2;

impl Sanitizer {
    fn new(mut pairs: Vec<(String, String)>) -> Sanitizer {
        pairs.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        pairs.dedup_by(|a, b| a.0 == b.0);
        Sanitizer { pairs }
    }

    fn text(&self, s: &str) -> String {
        let mut s = s.to_owned();
        for (from, to) in &self.pairs {
            s = s.replace(from.as_str(), to);
        }
        scrub_users(&scrub_emails(&s))
    }

    /// One JSONL file: hook outputs redacted, the `initialize` account redacted, catalog arrays
    /// trimmed; lines left intact otherwise (then [`Self::text`]).
    fn jsonl(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for line in text.lines() {
            let redacted = wire::redact_for_log(line.as_bytes());
            let line = String::from_utf8_lossy(&redacted).into_owned();
            let line = match serde_json::from_str::<Value>(&line) {
                Ok(mut v) => {
                    if redact_json(&mut v) {
                        v.to_string()
                    } else {
                        line
                    }
                }
                Err(_) => line,
            };
            out.push_str(&self.text(&line));
            out.push('\n');
        }
        out
    }
}

/// `true` if `v` changed.
fn redact_json(v: &mut Value) -> bool {
    let mut changed = false;
    let hook = v["type"] == "system"
        && v["subtype"]
            .as_str()
            .is_some_and(|s| s.starts_with("hook_"));
    if hook {
        for key in ["output", "stdout", "stderr"] {
            if v[key].as_str().is_some_and(|s| !s.is_empty()) {
                v[key] = "<redacted: user hook output>".into();
                changed = true;
            }
        }
    }
    let catalog = if v["type"] == "control_response" {
        v["response"].get_mut("response")
    } else if v["type"] == "system" && v["subtype"] == "init" {
        Some(&mut *v)
    } else {
        None
    };
    if let Some(Value::Object(map)) = catalog {
        for key in TRIMMED {
            if let Some(Value::Array(items)) = map.get_mut(*key)
                && items.len() > KEEP_ITEMS
            {
                items.truncate(KEEP_ITEMS);
                changed = true;
            }
        }
    }
    changed
}

/// `local@domain.tld` → `<email>`, a bare `@domain.tld` → `@<domain>` (a dot followed by two
/// letters makes a domain, as the M5 fixture grep does).
fn scrub_emails(s: &str) -> String {
    let b = s.as_bytes();
    let local = |c: u8| c.is_ascii_alphanumeric() || b"._%+-".contains(&c);
    let domain = |c: u8| c.is_ascii_alphanumeric() || b".-".contains(&c);
    let (mut out, mut last, mut i) = (String::with_capacity(s.len()), 0, 0);
    while i < b.len() {
        if b[i] != b'@' {
            i += 1;
            continue;
        }
        let mut start = i;
        while start > last && local(b[start - 1]) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < b.len() && domain(b[end]) {
            end += 1;
        }
        let d = &b[i + 1..end];
        let is_domain = (1..d.len().saturating_sub(2)).any(|k| {
            d[k] == b'.' && d[k + 1].is_ascii_alphabetic() && d[k + 2].is_ascii_alphabetic()
        });
        if is_domain {
            out.push_str(&s[last..start]);
            out.push_str(if start < i { "<email>" } else { "@<domain>" });
            last = end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[last..]);
    out
}

/// Any other `/Users/<name>` → `/Users/<user>`.
fn scrub_users(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find("/Users/") {
        out.push_str(&rest[..at + "/Users/".len()]);
        rest = &rest[at + "/Users/".len()..];
        let name = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'))
            .unwrap_or(rest.len());
        if name > 0 {
            out.push_str("<user>");
            rest = &rest[name..];
        }
    }
    out.push_str(rest);
    out
}

// ---- event sink ---------------------------------------------------------------------------

/// App events and transcript messages as they arrive; each one bumps `tick`. Condition (3) of
/// the guard sets `bad_key`: a `SessionInit` whose `apiKeySource` is not a subscription one, or
/// model output (text, thinking, tool call, turn end) of a process before its `SessionInit`.
#[derive(Clone)]
struct Seen {
    events: Arc<Mutex<Vec<AppEvent>>>,
    msgs: Arc<Mutex<Vec<TranscriptMsg>>>,
    bad_key: Arc<Mutex<Option<String>>>,
    /// Processes whose `SessionInit` arrived.
    inits: Arc<Mutex<HashSet<String>>>,
    tick: Arc<watch::Sender<u64>>,
}

impl Seen {
    fn new() -> Seen {
        Seen {
            events: Arc::default(),
            msgs: Arc::default(),
            bad_key: Arc::default(),
            inits: Arc::default(),
            tick: Arc::new(watch::channel(0).0),
        }
    }

    fn notify(&self) -> Notify {
        let seen = self.clone();
        Arc::new(move |event| {
            seen.events.lock().unwrap().push(event);
            seen.tick.send_modify(|n| *n += 1);
        })
    }

    fn sink(&self) -> TranscriptSink {
        let seen = self.clone();
        Box::new(move |msg| {
            // A snapshot is a tail of the transcript: it may hold the output of an old process
            // without its `SessionInit`, so only live upserts are held to the order.
            let (entries, live) = match &msg {
                TranscriptMsg::Snapshot { entries, .. } => (entries.as_slice(), false),
                TranscriptMsg::Upsert { entries } => (entries.as_slice(), true),
                TranscriptMsg::Typing { .. } => (&[][..], false),
            };
            for e in entries {
                let bad = match &e.body {
                    EntryBody::SessionInit { api_key_source, .. } => {
                        seen.inits.lock().unwrap().insert(e.process_id.clone());
                        let ok = api_key_source
                            .as_deref()
                            .is_some_and(|s| normalize::NO_API_KEY_SOURCES.contains(&s));
                        (!ok).then(|| {
                            format!(
                                "apiKeySource {}",
                                api_key_source.as_deref().unwrap_or("<absent>")
                            )
                        })
                    }
                    EntryBody::AssistantText { .. }
                    | EntryBody::Thinking { .. }
                    | EntryBody::ToolCall { .. }
                    | EntryBody::TurnEnd { .. }
                        if live && !seen.inits.lock().unwrap().contains(&e.process_id) =>
                    {
                        Some(format!("{} before any system/init", e.body.kind()))
                    }
                    _ => None,
                };
                if let Some(bad) = bad {
                    seen.bad_key.lock().unwrap().get_or_insert(bad);
                }
            }
            seen.msgs.lock().unwrap().push(msg);
            seen.tick.send_modify(|n| *n += 1);
            true
        })
    }

    fn msgs(&self) -> Vec<TranscriptMsg> {
        self.msgs.lock().unwrap().clone()
    }

    /// The UI store (spec §6.5).
    fn store(&self) -> BTreeMap<u32, Entry> {
        let mut store = BTreeMap::new();
        for msg in self.msgs() {
            match msg {
                TranscriptMsg::Snapshot { entries, .. } => {
                    store = entries.into_iter().map(|e| (e.idx, e)).collect();
                }
                TranscriptMsg::Upsert { entries } => {
                    for e in entries {
                        if store.get(&e.idx).is_none_or(|old: &Entry| e.rev > old.rev) {
                            store.insert(e.idx, e);
                        }
                    }
                }
                TranscriptMsg::Typing { .. } => {}
            }
        }
        store
    }

    fn typing_previews(&self) -> usize {
        self.msgs()
            .iter()
            .filter(|m| matches!(m, TranscriptMsg::Typing { text: Some(_) }))
            .count()
    }
}

// ---- the Core on the real CLI -------------------------------------------------------------

/// Where the project repo comes from.
enum Source<'a> {
    /// `git clone --no-hardlinks` of the toy repo.
    Toy(&'a Path),
    /// A new repo: `common::init_repo` plus these files, committed.
    Fresh(&'a [(&'a str, String)]),
}

/// A `can_use_tool` as the transcript shows it.
#[derive(Debug, Clone)]
struct Asked {
    approval_id: String,
    tool: String,
    summary: String,
    /// The whole Bash command (the summary keeps its first line only); `None` for another
    /// tool or an input that does not parse.
    command: Option<String>,
    can_remember: bool,
    reason: Option<String>,
}

impl Asked {
    fn json(&self) -> Value {
        json!({"tool": self.tool, "summary": self.summary, "can_remember": self.can_remember,
               "reason": self.reason})
    }
}

struct Real {
    guard: Arc<Guard>,
    test: &'static str,
    dir: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    config: CoreConfig,
    core: Core,
    /// Cores whose runtime is gone (kept alive, like the dead app's memory).
    retired: Vec<Core>,
    seen: Seen,
    project: Project,
    /// Capture label → (attempt id, seq).
    labels: Mutex<Vec<(String, String, u32)>>,
    obs: Mutex<BTreeMap<String, Value>>,
    /// Soft checks that failed: asserted by [`Real::finish`], after the captures are saved, so
    /// one run records every observation.
    failures: Mutex<Vec<String>>,
}

impl Real {
    async fn new(guard: Arc<Guard>, test: &'static str, source: Source<'_>) -> Real {
        let dir = common::tempdir();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("repo");
        match source {
            Source::Toy(toy) => {
                let out = std::process::Command::new("git")
                    .args(["clone", "-q", "--no-hardlinks"])
                    .arg(toy)
                    .arg(&repo)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .output()
                    .unwrap();
                assert!(out.status.success(), "git clone of the toy repo failed");
                common::git(&repo, &["config", "user.name", "ATM M5"]);
                common::git(&repo, &["config", "user.email", "m5@example.invalid"]);
            }
            Source::Fresh(files) => {
                common::init_repo(&repo);
                for (path, content) in files {
                    let file = repo.join(path);
                    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                    std::fs::write(file, content).unwrap();
                }
                common::git(&repo, &["add", "-A"]);
                common::git(&repo, &["commit", "-q", "-m", "project config"]);
            }
        }
        // Never push anywhere real: `origin` is a throwaway bare repo (step f proves it empty).
        let remote = root.join("remote.git");
        common::git(&root, &["init", "-q", "--bare", "remote.git"]);
        if common::git(&repo, &["remote"])
            .lines()
            .any(|r| r == "origin")
        {
            common::git(
                &repo,
                &["remote", "set-url", "origin", remote.to_str().unwrap()],
            );
        } else {
            common::git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
        }
        let config = CoreConfig {
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            claude_path: Some(guard.claude.clone()),
            path_env: Some(guard.path.clone()),
            extra_env: guard.extra_env.clone(),
            open_log: None,
        };
        let seen = Seen::new();
        let core = Core::new(config.clone(), seen.notify()).unwrap();
        core.startup().await.unwrap();
        let settings = Settings {
            worktree_root: root.join("wt").to_string_lossy().into_owned(),
            ..core.get_settings().await.unwrap()
        };
        core.update_settings(settings).await.unwrap();
        let added = core
            .add_project(AddProjectReq {
                path: repo.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let real = Real {
            guard,
            test,
            dir,
            repo,
            remote,
            config,
            core,
            retired: Vec::new(),
            seen,
            project: added.project,
            labels: Mutex::default(),
            obs: Mutex::default(),
            failures: Mutex::default(),
        };
        real.note("cli_version", json!(real.guard.version));
        real.note("add_project_warnings", json!(added.warnings));
        real
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn note(&self, key: &str, value: Value) {
        eprintln!("[M5] {}/{key} = {value}", self.test);
        self.obs.lock().unwrap().insert(key.to_owned(), value);
    }

    /// A soft check (see `failures`).
    fn check(&self, ok: bool, what: impl std::fmt::Display) {
        if !ok {
            eprintln!("[M5] {}: CHECK FAILED: {what}", self.test);
            self.failures.lock().unwrap().push(what.to_string());
        }
    }

    fn db(&self) -> Db {
        Db::open(&self.config.data_dir.join("atm.sqlite3")).unwrap()
    }

    fn processes(&self, attempt_id: &str) -> Vec<ProcessRow> {
        self.db().attempt_processes(attempt_id).unwrap()
    }

    /// A new Core on the same data dir (the app restarted), with its startup recovery.
    async fn restart(&mut self) {
        self.seen = Seen::new();
        let core = Core::new(self.config.clone(), self.seen.notify()).unwrap();
        core.startup().await.unwrap();
        let old = std::mem::replace(&mut self.core, core);
        self.retired.push(old);
    }

    async fn task(&self, title: &str, description: &str) -> Task {
        let req = CreateTaskReq {
            project_id: self.project.id.clone(),
            title: title.into(),
            description: description.into(),
            status: None,
        };
        self.core.create_task(req).await.unwrap().task
    }

    async fn start(&self, task: &Task, mode: PermissionMode, label: &str) -> AttemptView {
        // The worktree does not exist yet: the preflight runs in the repo, the same user
        // settings apply (Isolated reads no project settings).
        let argv = turn_argv(&self.guard, &self.repo, mode, &[]);
        let env = self.guard.env().for_attempt(&self.repo, "m5-preflight");
        self.guard.before_turn(&argv, &self.repo, &env).await;
        let req = StartAttemptReq {
            task_id: task.id.clone(),
            target_branch: "main".into(),
            permission_mode: mode,
            model: Some(MODEL.into()),
            effort: Some(Effort::Low),
        };
        let view = self.core.start_attempt(req).await.unwrap();
        self.guard.count_turn();
        self.label(label, &view.id, 1);
        self.subscribe(&view.id).await;
        view
    }

    async fn follow_up(
        &self,
        attempt_id: &str,
        prompt: &str,
        mode: Option<PermissionMode>,
        fresh_session: bool,
        label: &str,
    ) -> ProcessInfo {
        let attempt = self.db().attempt(attempt_id).unwrap();
        let worktree = PathBuf::from(&attempt.worktree_path);
        let mode_now = mode.unwrap_or(attempt.permission_mode);
        let argv = turn_argv(&self.guard, &worktree, mode_now, &attempt.allow_rules);
        let env = self.guard.env().for_attempt(&worktree, attempt_id);
        self.guard.before_turn(&argv, &worktree, &env).await;
        let req = SendFollowUpReq {
            attempt_id: attempt_id.into(),
            prompt: prompt.into(),
            permission_mode: mode,
            fresh_session,
        };
        let info = self.core.send_follow_up(req).await.unwrap();
        self.guard.count_turn();
        self.label(label, attempt_id, info.seq);
        info
    }

    fn label(&self, label: &str, attempt_id: &str, seq: u32) {
        let label = format!("{}-{label}", self.test);
        self.labels
            .lock()
            .unwrap()
            .push((label, attempt_id.into(), seq));
    }

    async fn subscribe(&self, attempt_id: &str) {
        let req = AttemptIdReq {
            attempt_id: attempt_id.into(),
        };
        self.core
            .subscribe_transcript(req, self.seen.sink())
            .await
            .unwrap();
    }

    async fn detail(&self, task_id: &str) -> TaskDetail {
        let req = IdReq { id: task_id.into() };
        self.core.get_task_detail(req).await.unwrap()
    }

    /// Condition (3) of the guard: trips it (sticky), stops every turn, then fails the run.
    async fn check_key(&self) {
        let bad = self.seen.bad_key.lock().unwrap().clone();
        if let Some(why) = bad {
            trip(&why);
            self.core.shutdown(Duration::from_secs(15)).await;
            panic!(
                "M5 guard: {why}: not provably a subscription login: turn stopped, run \
                 aborted"
            );
        }
    }

    /// Re-checks `cond` after every event until it holds; panics after `limit`.
    async fn until(&self, what: &str, limit: Duration, mut cond: impl AsyncFnMut() -> bool) {
        let mut tick = self.seen.tick.subscribe();
        let deadline = Instant::now() + limit;
        loop {
            tick.borrow_and_update();
            self.check_key().await;
            if cond().await {
                return;
            }
            if tokio::time::timeout_at(deadline, tick.changed())
                .await
                .is_err()
            {
                panic!("timed out after {limit:?} waiting for {what}");
            }
        }
    }

    /// Entries of one process in the subscribed transcript.
    fn entries_of(&self, process_id: &str) -> Vec<Entry> {
        self.seen
            .store()
            .into_values()
            .filter(|e| e.process_id == process_id)
            .collect()
    }

    fn pending(&self, process_id: &str) -> Vec<Asked> {
        self.entries_of(process_id)
            .into_iter()
            .filter_map(|e| match e.body {
                EntryBody::ToolCall {
                    name,
                    summary,
                    input,
                    status:
                        ToolStatus::AwaitingApproval {
                            approval_id,
                            can_remember,
                            reason,
                        },
                    ..
                } => Some(Asked {
                    approval_id,
                    command: (name == "Bash")
                        .then(|| serde_json::from_str::<Value>(&input).ok())
                        .flatten()
                        .and_then(|v| v["command"].as_str().map(str::to_owned)),
                    tool: name,
                    summary,
                    can_remember,
                    reason,
                }),
                _ => None,
            })
            .collect()
    }

    /// The next approval of turn `seq` not in `handled`, or `None` once the turn ended.
    async fn next_approval(
        &self,
        task_id: &str,
        seq: u32,
        handled: &HashSet<String>,
    ) -> Option<Asked> {
        let mut next = None;
        self.until(&format!("turn {seq} to ask or end"), TURN, async || {
            let d = self.detail(task_id).await;
            if turn_over(&d, seq) {
                return true;
            }
            let Some(p) = d.processes.get(seq as usize - 1) else {
                return false;
            };
            next = self
                .pending(&p.id)
                .into_iter()
                .find(|a| !handled.contains(&a.approval_id));
            next.is_some()
        })
        .await;
        next
    }

    async fn respond(&self, attempt_id: &str, asked: &Asked, decision: ApprovalDecision) {
        eprintln!(
            "[M5] {} → {decision:?} ({} can_remember={} reason={:?})",
            asked.summary, asked.tool, asked.can_remember, asked.reason
        );
        let req = RespondApprovalReq {
            attempt_id: attempt_id.into(),
            approval_id: asked.approval_id.clone(),
            decision,
        };
        self.core.respond_approval(req).await.unwrap();
    }

    /// The allowlist of a step: `Allow{remember}` for a Bash command `allowed` accepts (double
    /// quotes read as single ones), else "Nega e ferma" plus a failed check. The real model
    /// runs on this machine: nothing it asks outside what the step's prompt names is approved.
    fn gate(
        &self,
        step: &str,
        a: &Asked,
        allowed: impl Fn(&str) -> bool,
        remember: bool,
    ) -> ApprovalDecision {
        let command = a.command.as_deref().map(|c| c.trim().replace('"', "'"));
        if command.as_deref().is_some_and(allowed) {
            return ApprovalDecision::Allow { remember };
        }
        self.check(false, format!("{step}: unexpected approval denied: {a:?}"));
        ApprovalDecision::Deny {
            message: "Not part of this validation run: stop here.".into(),
            interrupt: true,
        }
    }

    /// Answers every approval of turn `seq` with `policy` until the turn ends; returns them.
    async fn drive(
        &self,
        attempt_id: &str,
        task_id: &str,
        seq: u32,
        mut policy: impl FnMut(&Asked) -> ApprovalDecision,
    ) -> (Vec<Asked>, TaskDetail) {
        let mut handled = HashSet::new();
        let mut asked = Vec::new();
        while let Some(a) = self.next_approval(task_id, seq, &handled).await {
            handled.insert(a.approval_id.clone());
            self.respond(attempt_id, &a, policy(&a)).await;
            asked.push(a);
        }
        (asked, self.turn_end(task_id, seq).await)
    }

    /// Waits until turn `seq` is finalized; adds its cost.
    async fn turn_end(&self, task_id: &str, seq: u32) -> TaskDetail {
        self.until(&format!("the end of turn {seq}"), TURN, async || {
            turn_over(&self.detail(task_id).await, seq)
        })
        .await;
        let d = self.detail(task_id).await;
        let p = &d.processes[seq as usize - 1];
        self.guard.add_cost(p.cost_usd_estimate);
        eprintln!(
            "[M5] turn {seq}: {:?}/{:?} result {:?} cost ≈ {:?} USD",
            p.status, p.stop_reason, p.result_subtype, p.cost_usd_estimate
        );
        d
    }

    /// Assistant texts and the `result` text of a process, joined.
    fn said(&self, process_id: &str) -> String {
        self.entries_of(process_id)
            .into_iter()
            .filter_map(|e| match e.body {
                EntryBody::AssistantText { text } => Some(text),
                EntryBody::TurnEnd { text, .. } => text,
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn session_init(&self, process_id: &str) -> Option<Value> {
        self.entries_of(process_id)
            .into_iter()
            .find_map(|e| match e.body {
                body @ EntryBody::SessionInit { .. } => serde_json::to_value(body).ok(),
                _ => None,
            })
    }

    fn turn_end_entry(&self, process_id: &str) -> Option<Value> {
        self.entries_of(process_id)
            .into_iter()
            .find_map(|e| match e.body {
                body @ EntryBody::TurnEnd { .. } => serde_json::to_value(body).ok(),
                _ => None,
            })
    }

    fn tool_calls(&self, process_id: &str) -> Vec<Value> {
        self.entries_of(process_id)
            .into_iter()
            .filter_map(|e| match e.body {
                EntryBody::ToolCall {
                    name,
                    summary,
                    status,
                    output,
                    ..
                } => Some(json!({
                    "name": name,
                    "summary": summary,
                    "status": status,
                    "output": output.map(|o| o.text),
                })),
                _ => None,
            })
            .collect()
    }

    fn notices(&self, process_id: &str) -> Vec<Value> {
        self.entries_of(process_id)
            .into_iter()
            .filter_map(|e| match e.body {
                EntryBody::Notice {
                    level,
                    text,
                    action,
                } => Some(json!({"level": level, "text": text, "action": action})),
                _ => None,
            })
            .collect()
    }

    /// Raw `stdout.jsonl` lines of a process (the Core's log).
    fn raw_log(&self, attempt_id: &str, process_id: &str) -> Vec<Value> {
        let dir = runner::log_dir(&self.config.data_dir, attempt_id, process_id);
        std::fs::read_to_string(dir.join("stdout.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// The `request` of every `can_use_tool` of a process, from its raw log.
    fn raw_can_use_tool(&self, attempt_id: &str, process_id: &str) -> Vec<Value> {
        self.raw_log(attempt_id, process_id)
            .into_iter()
            .filter(|v| v["type"] == "control_request" && v["request"]["subtype"] == "can_use_tool")
            .map(|v| v["request"].clone())
            .collect()
    }

    fn raw_types(&self, attempt_id: &str, process_id: &str) -> Vec<String> {
        let mut kinds: Vec<String> = self
            .raw_log(attempt_id, process_id)
            .iter()
            .map(|v| {
                let kind = v["type"].as_str().unwrap_or("?");
                match v["subtype"].as_str() {
                    Some(sub) => format!("{kind}/{sub}"),
                    None => kind.to_owned(),
                }
            })
            .collect();
        kinds.dedup();
        kinds
    }

    /// `ps` shows the claude path of the argv and the session id on the group leader, so the
    /// startup recovery can recognise an orphan of the real CLI (spec §7.9).
    async fn orphan_proof(&self, p: &ProcessRow) -> bool {
        let Some(pgid) = p.pid else { return false };
        let command = claude::process_command(pgid).await.unwrap_or_default();
        let claude = self.guard.claude.to_string_lossy();
        command.contains(claude.as_ref()) && command.contains(&p.session_id)
    }

    /// Sanitized raw logs of every labelled turn, the observations, and a check that nothing of
    /// this test still runs.
    async fn finish(self) {
        self.core.shutdown(Duration::from_secs(15)).await;
        let root = self.root();
        let sessions = self.save_captures();
        let mut marks: Vec<String> = sessions;
        marks.push(root.to_string_lossy().into_owned());
        marks.push(self.dir.path().to_string_lossy().into_owned());
        no_leftovers(&marks).await;
        let failures = self.failures.lock().unwrap().clone();
        eprintln!(
            "[M5] {}: {} real turns so far, ≈ {:.4} USD (CLI estimate); captures in {}",
            self.test,
            self.guard.turns.load(Ordering::SeqCst),
            *self.guard.cost.lock().unwrap(),
            self.guard.capture.display()
        );
        assert!(
            failures.is_empty(),
            "{} failed checks: {failures:#?}",
            self.test
        );
    }
}

/// What [`Real::run_until`] reached.
enum Reached {
    /// The command runs (`ps`).
    Running,
    Ended,
}

impl Real {
    /// Polls turn `seq` (a process starting has no event), answering approvals with `policy`;
    /// returns once a process whose command line contains `command` runs, or the turn ends.
    async fn run_until(
        &self,
        attempt_id: &str,
        task_id: &str,
        seq: u32,
        command: &str,
        asked: &mut Vec<Asked>,
        mut policy: impl FnMut(&Asked) -> ApprovalDecision,
    ) -> Reached {
        let deadline = Instant::now() + TURN;
        loop {
            self.check_key().await;
            let d = self.detail(task_id).await;
            if turn_over(&d, seq) {
                return Reached::Ended;
            }
            if let Some(p) = d.processes.get(seq as usize - 1) {
                for a in self.pending(&p.id) {
                    if asked.iter().any(|x| x.approval_id == a.approval_id) {
                        continue;
                    }
                    let decision = policy(&a);
                    self.respond(attempt_id, &a, decision).await;
                    asked.push(a);
                }
            }
            if !running(command).await.is_empty() {
                return Reached::Running;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for `{command}`"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Real {
    /// Sanitized raw logs of every labelled turn, a summary of each turn from the DB, and the
    /// observations; returns the session ids. Also runs when the test fails ([`Drop`]).
    fn save_captures(&self) -> Vec<String> {
        let root = self.root();
        let sanitizer = self.guard.sanitizer(&[self.dir.path(), &root]);
        let capture = &self.guard.capture;
        let labels = self.labels.lock().unwrap().clone();
        let db = self.db();
        let mut sessions = Vec::new();
        let mut turns = BTreeMap::new();
        for (label, attempt_id, seq) in &labels {
            let Some(p) = self
                .processes(attempt_id)
                .into_iter()
                .nth(*seq as usize - 1)
            else {
                continue;
            };
            sessions.push(p.session_id.clone());
            let entries = db
                .entries_tail(attempt_id, 200)
                .map(|page| page.entries)
                .unwrap_or_default();
            let bodies: Vec<Value> = entries
                .iter()
                .filter(|e| {
                    e.process_id == p.id && !matches!(e.body, EntryBody::UserMessage { .. })
                })
                .filter_map(|e| serde_json::to_value(&e.body).ok())
                .collect();
            turns.insert(
                label.clone(),
                json!({"status": p.status, "stop_reason": p.stop_reason,
                       "result_subtype": p.result_subtype, "exit_code": p.exit_code,
                       "cost_usd_estimate": p.cost_usd_estimate, "resumed": p.resumed,
                       "permission_mode": p.permission_mode, "entries": bodies}),
            );
            let dir = runner::log_dir(&self.config.data_dir, attempt_id, &p.id);
            for file in ["stdout.jsonl", "stdin.jsonl", "stderr.log"] {
                let text = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
                if text.trim().is_empty() {
                    continue;
                }
                let text = if file.ends_with(".jsonl") {
                    sanitizer.jsonl(&text)
                } else {
                    sanitizer.text(&text)
                };
                std::fs::write(capture.join(format!("{label}.{file}")), text).unwrap();
            }
        }
        let mut obs = self.obs.lock().unwrap().clone();
        obs.insert("turns".into(), json!(turns));
        obs.insert(
            "failed_checks".into(),
            json!(*self.failures.lock().unwrap()),
        );
        let obs = serde_json::to_string_pretty(&obs).unwrap();
        std::fs::write(
            capture.join(format!("{}-observations.json", self.test)),
            sanitizer.text(&obs) + "\n",
        )
        .unwrap();
        sessions
    }
}

impl Drop for Real {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.save_captures();
            eprintln!("[M5] {}: captures saved after the failure", self.test);
        }
    }
}

fn turn_over(d: &TaskDetail, seq: u32) -> bool {
    !d.attempt.as_ref().is_some_and(|a| a.running)
        && d.processes
            .get(seq as usize - 1)
            .is_some_and(|p| p.status != ProcessStatus::Running)
}

fn is_touch(a: &Asked) -> bool {
    a.summary.contains(TOUCH)
}

/// The commands the prompts of the checklist ask for (the allowlists of [`Real::gate`]).
const TOUCH: &str = "touch approved.txt";
const WAIT_41_CMD: &str = "python3 -c 'import time; time.sleep(41)'";
const WAIT_42_CMD: &str = "python3 -c 'import time; time.sleep(42)'";

/// `ls`, with flags at most (`ls -la`).
fn is_ls(command: &str) -> bool {
    let mut words = command.split_whitespace();
    words.next() == Some("ls")
        && words.all(|w| {
            w.len() > 1 && w.starts_with('-') && w[1..].chars().all(|c| c.is_ascii_alphabetic())
        })
}

/// `cat <path>/CLAUDE.md`, optionally with `2>/dev/null` and `| head -N`: the read the append
/// prompt asks for (M5 saw `cat <worktree>/CLAUDE.md 2>/dev/null | head -5`).
fn reads_claude_md(command: &str) -> bool {
    let mut rest = command.trim();
    if let Some((before, n)) = rest.rsplit_once("| head -")
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
    {
        rest = before.trim_end();
    }
    rest = rest.strip_suffix("2>/dev/null").unwrap_or(rest).trim_end();
    rest.strip_prefix("cat ").is_some_and(|path| {
        path.ends_with("CLAUDE.md")
            && path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    })
}

fn argv_of(p: &ProcessRow) -> Vec<String> {
    serde_json::from_str(&p.argv_json).unwrap()
}

/// `(pid, pgid, command)` of every process of the machine.
async fn ps() -> Vec<(i32, i32, String)> {
    let out = tokio::process::Command::new("/bin/ps")
        .args(["-Ao", "pid=,pgid=,command="])
        .output()
        .await
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let pgid = it.next()?.parse().ok()?;
            Some((pid, pgid, it.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

/// The deliberate waits of steps (d) and (e): a foreground `sleep N` is blocked by the CLI
/// 2.1.283 ("use run_in_background"), a Python sleep is not.
const WAIT_41: &str = "time.sleep(41)";
const WAIT_42: &str = "time.sleep(42)";

/// `(pid, pgid)` of the processes whose command line contains `needle`.
async fn running(needle: &str) -> Vec<(i32, i32)> {
    ps().await
        .into_iter()
        .filter(|(_, _, c)| c.contains(needle) && !c.starts_with("/bin/ps"))
        .map(|(pid, pgid, _)| (pid, pgid))
        .collect()
}

/// [`running`] outside any runtime.
fn running_blocking(needle: &str) -> Vec<(i32, i32)> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-Ao", "pid=,pgid=,command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(needle))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
        })
        .collect()
}

/// SIGKILL to what a check found left behind (the waits this test asked for).
fn kill_pids(left: &[(i32, i32)]) {
    for &(pid, _) in left {
        // SAFETY: a plain syscall on a pid this test just listed.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// No process of the machine names one of `marks` (session ids, temp roots): anything left is
/// killed, then the test fails.
async fn no_leftovers(marks: &[String]) {
    let me = std::process::id() as i32;
    let left: Vec<(i32, i32, String)> = ps()
        .await
        .into_iter()
        .filter(|(pid, _, c)| {
            *pid != me && !c.starts_with("/bin/ps") && marks.iter().any(|m| c.contains(m.as_str()))
        })
        .collect();
    for (pid, _, _) in &left {
        // SAFETY: a plain syscall on integers; only processes naming this test's temp dir or
        // sessions.
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    assert!(left.is_empty(), "processes left behind: {left:?}");
}

/// Reaps our child `pid` that a dropped runtime left behind (see `flow.rs`).
fn reap(pid: i32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let mut status = 0;
        // SAFETY: a non-blocking wait for our own child; `status` outlives the call.
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if reaped != 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the agent was not killed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

// ---- raw spawns (protocol probes) ---------------------------------------------------------

/// What a probe does after a stdout line.
enum Act {
    CloseStdin,
    Kill,
}

#[derive(Default)]
struct RawRun {
    stdout: Vec<String>,
    stdin: Vec<String>,
    stderr: String,
    exit: Option<i32>,
    timed_out: bool,
    /// Why condition (3) killed the turn.
    bad_key: Option<String>,
}

impl RawRun {
    fn json(&self) -> Vec<Value> {
        self.stdout
            .iter()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

/// One `claude` spawn in `cwd` with the Core's environment (`PWD` = `cwd`, the same string),
/// `first` frames on stdin, then `on_line` for every stdout line; killed after `limit`. The
/// guard runs first: conditions (1) and (2) when `first` carries a user message, else (1).
async fn raw(
    guard: &Guard,
    argv: &[String],
    cwd: &Path,
    first: &[Value],
    limit: Duration,
    on_line: impl FnMut(&Value) -> Vec<Act>,
) -> RawRun {
    let env = guard.env().for_attempt(cwd, "m5-probe");
    let turn = first.iter().any(|f| f["type"] == "user");
    if turn {
        guard.before_turn(argv, cwd, &env).await;
    } else {
        guard.check_auth().await;
    }
    let run = spawn_raw(argv, cwd, &env, first, limit, turn, on_line).await;
    if let Some(why) = &run.bad_key {
        panic!("M5 guard: {why}: not provably a subscription login: turn killed, run aborted");
    }
    run
}

/// [`raw`] without the checks before the spawn. With `watch_key`, condition (3) of the guard on
/// every stdout line ([`key_trip`]): a trip kills the group at once and is recorded (sticky).
async fn spawn_raw(
    argv: &[String],
    cwd: &Path,
    env: &ChildEnv,
    first: &[Value],
    limit: Duration,
    watch_key: bool,
    mut on_line: impl FnMut(&Value) -> Vec<Act>,
) -> RawRun {
    let claude::Spawned {
        mut child,
        pgid,
        stdin,
        stdout,
        stderr,
    } = claude::spawn(argv, cwd, env).unwrap();
    let mut run = RawRun::default();
    let mut stdin = Some(stdin);
    for frame in first {
        let line = frame.to_string();
        let w = stdin.as_mut().unwrap();
        w.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        w.flush().await.unwrap();
        run.stdin.push(line);
    }
    let mut out = BufReader::new(stdout).lines();
    let mut err = BufReader::new(stderr).lines();
    let (mut out_done, mut err_done) = (false, false);
    let mut init_seen = false;
    let deadline = Instant::now() + limit;
    while !(out_done && err_done) {
        tokio::select! {
            line = out.next_line(), if !out_done => match line {
                Ok(Some(line)) => {
                    let v = serde_json::from_str::<Value>(&line).ok();
                    let tripped = v
                        .as_ref()
                        .filter(|_| watch_key && run.bad_key.is_none())
                        .and_then(|v| key_trip(v, &mut init_seen));
                    let acts = match tripped {
                        Some(why) => {
                            trip(&why);
                            run.bad_key = Some(why);
                            vec![Act::Kill]
                        }
                        None => v.as_ref().map(&mut on_line).unwrap_or_default(),
                    };
                    run.stdout.push(line);
                    for act in acts {
                        match act {
                            Act::CloseStdin => stdin = None,
                            Act::Kill => {
                                stdin = None;
                                let _ = claude::killpg(pgid, libc::SIGKILL);
                            }
                        }
                    }
                }
                _ => out_done = true,
            },
            line = err.next_line(), if !err_done => match line {
                Ok(Some(line)) => {
                    run.stderr.push_str(&line);
                    run.stderr.push('\n');
                }
                _ => err_done = true,
            },
            () = tokio::time::sleep_until(deadline), if !run.timed_out => {
                run.timed_out = true;
                stdin = None;
                let _ = claude::killpg(pgid, libc::SIGKILL);
            }
        }
    }
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
    run.exit = status.ok().and_then(Result::ok).and_then(|s| s.code());
    let _ = claude::killpg(pgid, libc::SIGKILL);
    run
}

/// Condition (3) on one raw stdout line of a turn: `Some(why)` for a `system/init` whose
/// `apiKeySource` is not a subscription one, or for model output (`assistant`, `stream_event`,
/// `result`) before any `system/init`.
fn key_trip(v: &Value, init_seen: &mut bool) -> Option<String> {
    match (v["type"].as_str(), v["subtype"].as_str()) {
        (Some("system"), Some("init")) => {
            *init_seen = true;
            let ok = v["apiKeySource"]
                .as_str()
                .is_some_and(|s| normalize::NO_API_KEY_SOURCES.contains(&s));
            (!ok).then(|| format!("apiKeySource {}", v["apiKeySource"]))
        }
        (Some(kind @ ("assistant" | "stream_event" | "result")), _) if !*init_seen => {
            Some(format!("{kind} before any system/init"))
        }
        _ => None,
    }
}

/// Argv of a turn as the Core builds it (spec §7.3): policy Isolated, `sonnet`, `low`.
fn probe_argv(guard: &Guard, cwd: &Path, mode: PermissionMode, resume: bool) -> Vec<String> {
    let mut args = turn_args(guard, cwd, mode, &[]);
    args.resume = resume;
    claude::build_argv(&args)
}

/// [`probe_argv`] of a new session with `allow_rules` in `--settings`: what the preflight of
/// a Core turn runs.
fn turn_argv(
    guard: &Guard,
    cwd: &Path,
    mode: PermissionMode,
    allow_rules: &[String],
) -> Vec<String> {
    claude::build_argv(&turn_args(guard, cwd, mode, allow_rules))
}

fn turn_args(guard: &Guard, cwd: &Path, mode: PermissionMode, allow_rules: &[String]) -> TurnArgs {
    TurnArgs {
        claude: guard.claude.clone(),
        permission_mode: mode,
        allow_bypass: false,
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: false,
        isolated: true,
        allow_rules: allow_rules.to_vec(),
        model: Some(MODEL.into()),
        effort: Some(Effort::Low),
        append_prompt: claude::append_prompt(cwd, "main", "main"),
    }
}

fn initialize() -> Value {
    wire::initialize_request(&wire::request_id(1))
}

/// Closes stdin once the `initialize` answer arrives: nothing reaches the model.
fn close_on_answer(v: &Value) -> Vec<Act> {
    if v["type"] == "control_response" {
        vec![Act::CloseStdin]
    } else {
        Vec::new()
    }
}

fn line_kinds(lines: &[Value]) -> Vec<String> {
    let mut kinds: Vec<String> = lines
        .iter()
        .map(|v| {
            let kind = v["type"].as_str().unwrap_or("?");
            match v["subtype"].as_str() {
                Some(sub) => format!("{kind}/{sub}"),
                None => kind.to_owned(),
            }
        })
        .collect();
    kinds.dedup();
    kinds
}

fn save_raw(guard: &Guard, sanitizer: &Sanitizer, label: &str, run: &RawRun) {
    let dir = &guard.capture;
    let stdout: String = run.stdout.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(
        dir.join(format!("probes-{label}.stdout.jsonl")),
        sanitizer.jsonl(&stdout),
    )
    .unwrap();
    if !run.stdin.is_empty() {
        let stdin: String = run.stdin.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(
            dir.join(format!("probes-{label}.stdin.jsonl")),
            sanitizer.jsonl(&stdin),
        )
        .unwrap();
    }
    if !run.stderr.trim().is_empty() {
        std::fs::write(
            dir.join(format!("probes-{label}.stderr.log")),
            sanitizer.text(&run.stderr),
        )
        .unwrap();
    }
}

/// `.claude/settings.json` whose hooks append their event name to `marker` (and, for
/// `SessionStart`, save their stdin JSON to `input`).
fn hook_settings(marker: &Path, input: &Path) -> String {
    let m = marker.display();
    let hook = |event: &str| {
        json!([{"hooks": [{"type": "command",
        "command": format!("echo {event} >> '{m}'")}]}])
    };
    json!({"hooks": {
        "SessionStart": [{"hooks": [{"type": "command",
            "command": format!("cat > '{}'; echo SessionStart >> '{m}'", input.display())}]}],
        "UserPromptSubmit": hook("UserPromptSubmit"),
        "PreToolUse": [{"matcher": "*", "hooks": [{"type": "command",
            "command": format!("echo PreToolUse >> '{m}'")}]}],
        "PostToolUse": [{"matcher": "*", "hooks": [{"type": "command",
            "command": format!("echo PostToolUse >> '{m}'")}]}],
        "Stop": hook("Stop"),
    }})
    .to_string()
}

// ---- tests --------------------------------------------------------------------------------

/// Protocol facts of spec §13.3 that need no model turn (initialize, `--verbose`, permission
/// modes, resume failure, project hooks and the project directory name), plus ONE real turn
/// that skips `initialize`.
#[test]
#[ignore = "real Claude Code CLI: ATM_REAL_CLAUDE=1, see the module docs"]
fn real_cli_protocol_probes() {
    let Some((_serial, guard)) = guard() else {
        return;
    };
    runtime().block_on(async {
        let dir = common::tempdir();
        let root = dir.path().canonicalize().unwrap();
        let repo = common::init_repo(&root.join("repo"));
        let sanitizer = guard.sanitizer(&[dir.path(), &root]);
        let mut obs = BTreeMap::<String, Value>::new();
        let mut note = |k: &str, v: Value| {
            eprintln!("[M5] probes/{k} = {v}");
            obs.insert(k.to_owned(), v);
        };
        note("cli_version", json!(guard.version));
        let mut sessions = Vec::new();

        // --verbose is required by stream-json output under -p.
        let mut argv = probe_argv(&guard, &repo, PermissionMode::AcceptEdits, false);
        argv.retain(|a| a != "--verbose");
        let run = raw(&guard, &argv, &repo, &[], Duration::from_secs(60), |_| Vec::new())
        .await;
        note(
            "without_verbose",
            json!({"exit": run.exit, "stderr": sanitizer.text(run.stderr.trim()),
                   "stdout_lines": run.stdout.len()}),
        );
        assert_eq!(run.exit, Some(1));
        assert!(run.stderr.contains("requires --verbose"), "{}", run.stderr);

        // Permission modes: the help lists `manual`, not `default`.
        for mode in ["default", "manual", "bogus"] {
            let mut argv = probe_argv(&guard, &repo, PermissionMode::AcceptEdits, false);
            for a in &mut argv {
                if a.starts_with("--permission-mode=") {
                    *a = format!("--permission-mode={mode}");
                }
            }
            let run = raw(&guard, &argv, &repo, &[initialize()], Duration::from_secs(90), close_on_answer).await;
            let answer = run.json().into_iter().find(|v| v["type"] == "control_response");
            note(
                &format!("permission_mode_{mode}"),
                json!({"exit": run.exit,
                       "stderr": sanitizer.text(run.stderr.trim()),
                       "current_permission_mode": answer.map(|a| a["response"]["response"]["current_permission_mode"].clone())}),
            );
        }

        // initialize: when it is answered, and what the answer carries.
        let argv = probe_argv(&guard, &repo, PermissionMode::AcceptEdits, false);
        sessions.push(argv.iter().find_map(|a| a.strip_prefix("--session-id=")).unwrap().to_owned());
        let run = raw(&guard, &argv, &repo, &[initialize()], Duration::from_secs(90), close_on_answer).await;
        let lines = run.json();
        let answer_at = lines.iter().position(|v| v["type"] == "control_response");
        let answer = answer_at.map(|i| lines[i]["response"].clone()).unwrap_or_default();
        note(
            "initialize",
            json!({"exit": run.exit,
                   "answered": answer["subtype"],
                   "answer_index": answer_at,
                   "kinds_in_order": line_kinds(&lines),
                   "response_keys": answer["response"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
                   "account_keys": answer["response"]["account"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
                   "system_init_before_user_message": lines.iter().any(|v| v["type"] == "system" && v["subtype"] == "init")}),
        );
        assert_eq!(answer["subtype"], "success");
        save_raw(&guard, &sanitizer, "initialize", &run);

        // --resume of a session that does not exist (RESUME_FAILED_PATTERN).
        let argv = probe_argv(&guard, &repo, PermissionMode::AcceptEdits, true);
        let run = raw(&guard, &argv, &repo, &[initialize()], Duration::from_secs(90), |v| {
            if v["type"] == "result" { vec![Act::CloseStdin] } else { Vec::new() }
        })
        .await;
        let lines = run.json();
        let result = lines.iter().find(|v| v["type"] == "result").cloned().unwrap_or_default();
        let parsed = normalize::parse_result(&result);
        note(
            "resume_not_found",
            json!({"exit": run.exit,
                   "stderr": sanitizer.text(run.stderr.trim()),
                   "kinds_in_order": line_kinds(&lines),
                   "result_subtype": result["subtype"], "is_error": result["is_error"],
                   "errors": sanitizer.text(&result["errors"].to_string()),
                   "initialize_answered": lines.iter().any(|v| v["type"] == "control_response"),
                   "pattern_in_stderr": normalize::is_resume_failure(&run.stderr),
                   "pattern_in_result": parsed.as_ref().and_then(|r| r.text.as_deref()).is_some_and(normalize::is_resume_failure),
                   "classified_limit": parsed.as_ref().map(|r| format!("{:?}", r.limit))}),
        );
        assert!(normalize::is_resume_failure(&run.stderr), "{}", run.stderr);
        save_raw(&guard, &sanitizer, "resume-not-found", &run);

        // Project hooks and the project directory name: a SessionStart hook runs without any
        // user message, so both are free. The cwd is the non-canonical temp path.
        let hooks_repo = dir.path().join("hooks-repo");
        common::init_repo(&hooks_repo);
        let marker = root.join("hook-marker");
        let input = root.join("hook-input.json");
        std::fs::create_dir_all(hooks_repo.join(".claude")).unwrap();
        std::fs::write(hooks_repo.join(".claude/settings.json"), hook_settings(&marker, &input)).unwrap();
        let isolated = probe_argv(&guard, &hooks_repo, PermissionMode::AcceptEdits, false);
        let run = raw(&guard, &isolated, &hooks_repo, &[initialize()], Duration::from_secs(90), close_on_answer).await;
        let isolated_marker = std::fs::read_to_string(&marker).ok();
        note("hooks_isolated", json!({"exit": run.exit, "marker": isolated_marker}));
        assert_eq!(isolated_marker, None, "--setting-sources=user ran a project hook");
        let mut trusted = isolated.clone();
        trusted.retain(|a| a != "--setting-sources=user" && a != "--strict-mcp-config");
        let run = raw(&guard, &trusted, &hooks_repo, &[initialize()], Duration::from_secs(90), close_on_answer).await;
        let trusted_marker = std::fs::read_to_string(&marker).ok();
        let hook_input: Value = std::fs::read_to_string(&input)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let transcript = PathBuf::from(hook_input["transcript_path"].as_str().unwrap_or_default());
        let project_dir = transcript
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let canonical = hooks_repo.canonicalize().unwrap();
        note(
            "hooks_control_user_project",
            json!({"exit": run.exit, "marker": trusted_marker}),
        );
        note(
            "project_dir_name",
            json!({"pwd": sanitizer.text(&hooks_repo.to_string_lossy()),
                   "realpath": sanitizer.text(&canonical.to_string_lossy()),
                   "hook_cwd": sanitizer.text(hook_input["cwd"].as_str().unwrap_or_default()),
                   "dir_name": sanitizer.text(&project_dir),
                   "matches_realpath": project_dir == dash(&canonical.to_string_lossy()),
                   "matches_pwd": project_dir == dash(&hooks_repo.to_string_lossy())}),
        );
        assert!(
            trusted_marker.is_some_and(|m| m.contains("SessionStart")),
            "control: the project hook must run with --setting-sources=user,project"
        );

        // ONE real turn: the user message without `initialize` first.
        let argv = probe_argv(&guard, &repo, PermissionMode::AcceptEdits, false);
        sessions.push(argv.iter().find_map(|a| a.strip_prefix("--session-id=")).unwrap().to_owned());
        let prompt = wire::user_message("Reply with exactly the word OK.");
        guard.count_turn();
        // `raw` watches condition (3) itself: a bad `system/init` kills the turn and panics.
        let run = raw(&guard, &argv, &repo, &[prompt], Duration::from_secs(150), |v| {
            if v["type"] == "result" { vec![Act::CloseStdin] } else { Vec::new() }
        })
        .await;
        let lines = run.json();
        let result = lines.iter().find(|v| v["type"] == "result").cloned().unwrap_or_default();
        guard.add_cost(result["total_cost_usd"].as_f64());
        note(
            "no_initialize_turn",
            json!({"exit": run.exit, "timed_out": run.timed_out,
                   "kinds_in_order": line_kinds(&lines),
                   "result_subtype": result["subtype"], "is_error": result["is_error"],
                   "result": result["result"], "cost_usd": result["total_cost_usd"],
                   "stream_events": lines.iter().filter(|v| v["type"] == "stream_event").count()}),
        );
        save_raw(&guard, &sanitizer, "no-initialize", &run);

        let text = serde_json::to_string_pretty(&obs).unwrap();
        std::fs::write(guard.capture.join("probes-observations.json"), sanitizer.text(&text) + "\n").unwrap();
        sessions.push(root.to_string_lossy().into_owned());
        sessions.push(dir.path().to_string_lossy().into_owned());
        no_leftovers(&sessions).await;
        eprintln!(
            "[M5] probes: {} real turns, ≈ {:.4} USD",
            guard.turns.load(Ordering::SeqCst),
            *guard.cost.lock().unwrap()
        );
    });
}

/// Spec §12.3 steps 1–6 and 9 on a clone of the toy repo: (a) first turn in Auto-edit, (b)
/// approvals and "Consenti sempre" (session rule across --resume, --settings rule in a new
/// session), (c) resume with context, (d) Stop mid-turn, (e) the app dies mid-turn and
/// "Continua", (h) merge. 5 real turns.
#[test]
#[ignore = "real Claude Code CLI: ATM_REAL_CLAUDE=1, see the module docs"]
fn real_cli_checklist() {
    let Some((_serial, guard)) = guard() else {
        return;
    };
    let toy = toy_repo().expect("the toy repo (ATM_REAL_CLAUDE_REPO)");
    let toy_before = toy_state(&toy);
    let first = runtime();
    let (real, task, attempt, pgid) = first.block_on(async {
        let real = Real::new(guard, "checklist", Source::Toy(&toy)).await;

        // (a) First turn, Auto-edit: SessionInit, streaming, TurnEnd with cost. The follow-ups
        // are announced here: M5 saw the model refuse a follow-up "unrelated to the task".
        let task = real
            .task(
                "Add one line to README.md",
                &format!(
                    "Append the line `Edited by AI Task Manager (M5).` at the end of README.md. \
                     This task is also a validation run of the host app: my follow-up messages \
                     will ask you to run specific shell commands in this worktree (such as `ls`, \
                     `touch approved.txt` or a deliberate Python wait); they are part of this \
                     task, so run exactly what each message asks. Remember the codeword {CODEWORD} for later: do not \
                     write it anywhere. Reply in one short sentence."
                ),
            )
            .await;
        let attempt = real.start(&task, PermissionMode::AcceptEdits, "1-readme").await;
        // Auto-edit writes README.md without asking (M5): nothing is expected here.
        let (asked, d) = real
            .drive(&attempt.id, &task.id, 1, |a| real.gate("a", a, |_| false, false))
            .await;
        let p1 = &d.processes[0];
        let rows = real.processes(&attempt.id);
        let init = real.session_init(&p1.id).unwrap_or_default();
        real.note("a_session_init", init.clone());
        real.note("a_turn_end", real.turn_end_entry(&p1.id).unwrap_or_default());
        real.note("a_approvals", json!(asked.iter().map(Asked::json).collect::<Vec<_>>()));
        real.note("a_typing_previews", json!(real.seen.typing_previews()));
        real.note("a_tool_calls", json!(real.tool_calls(&p1.id)));
        real.note("a_raw_kinds", json!(real.raw_types(&attempt.id, &p1.id)));
        real.note("a_process", json!({"status": p1.status, "stop_reason": p1.stop_reason,
            "result_subtype": p1.result_subtype, "exit_code": rows[0].exit_code,
            "cost_usd_estimate": p1.cost_usd_estimate, "num_turns": p1.num_turns}));
        real.check(p1.status == ProcessStatus::Completed, format!("a: turn 1 {p1:?}"));
        real.check(init["mcp_servers"] == 0, "a: --strict-mcp-config left MCP servers");
        real.check(init["warnings"] == json!([]), format!("a: SessionInit warnings {init}"));
        real.check(p1.cost_usd_estimate.is_some_and(|c| c > 0.0), "a: no cost");
        real.check(real.seen.typing_previews() > 0, "a: no streaming preview");
        let diff = real.core.get_diff(AttemptIdReq { attempt_id: attempt.id.clone() }).await.unwrap();
        let files: Vec<&str> = diff.files.iter().map(|f| f.path.as_str()).collect();
        real.note("a_diff_files", json!(files));
        real.check(files.contains(&"README.md"), format!("a: diff {files:?}"));

        // (b) Supervised: `ls` → Allow; `touch approved.txt` → Allow{remember}. (c) The resumed
        // session remembers turn 1.
        let info = real
            .follow_up(
                &attempt.id,
                "As part of this task, run these two shell commands now, each in its own Bash \
                 tool call: first `ls`, then `touch approved.txt`. After both have run, reply \
                 with the codeword from my first message.",
                Some(PermissionMode::Default),
                false,
                "2-approvals",
            )
            .await;
        let (asked, d) = real
            .drive(&attempt.id, &task.id, info.seq, |a| {
                real.gate("b", a, |c| is_ls(c) || c == TOUCH, is_touch(a))
            })
            .await;
        let p2 = &d.processes[1];
        let row2 = real.processes(&attempt.id)[1].clone();
        let rules = real.db().attempt(&attempt.id).unwrap().allow_rules;
        let said = real.said(&p2.id);
        let resumed = argv_of(&row2).contains(&format!("--resume={}", row2.session_id));
        real.note("b_approvals", json!(asked.iter().map(Asked::json).collect::<Vec<_>>()));
        real.note("b_can_use_tool_requests", json!(real.raw_can_use_tool(&attempt.id, &p2.id)));
        real.note("b_allow_rules", json!(rules));
        real.note("b_tool_calls", json!(real.tool_calls(&p2.id)));
        real.note("b_ls_asked", json!(asked.iter().any(|a| a.summary.starts_with("$ ls"))));
        real.note("c_resume", json!({"argv_resume": resumed, "session_init": real.session_init(&p2.id),
            "codeword_remembered": said.contains(CODEWORD), "said": said,
            "notices": real.notices(&p2.id), "status": p2.status}));
        match asked.iter().find(|a| is_touch(a)) {
            Some(touch) => real.check(
                touch.can_remember,
                format!("b: real suggestions not rememberable {touch:?}"),
            ),
            None => real.check(false, "b: `touch approved.txt` did not ask"),
        }
        real.check(rules == ["Bash(touch approved.txt)"], format!("b: allow_rules {rules:?}"));
        real.check(resumed && p2.status == ProcessStatus::Completed, format!("c: {p2:?}"));
        real.check(said.contains(CODEWORD), format!("c: turn 1 forgotten: {said}"));

        // (b) Resume WITHOUT our --settings allow rules: does the session rule granted by
        // `updatedPermissions` survive --resume? (d) Then Stop during a 41-second wait.
        let conn = rusqlite::Connection::open(real.config.data_dir.join("atm.sqlite3")).unwrap();
        conn.execute("UPDATE attempts SET allow_rules = '[]' WHERE id = ?1", [&attempt.id]).unwrap();
        drop(conn);
        let info = real
            .follow_up(
                &attempt.id,
                "As part of this task, run two shell commands, each in its own Bash tool call: \
                 first `touch approved.txt`, then `python3 -c 'import time; time.sleep(41)'` (a \
                 deliberate 41-second wait). Then reply DONE.",
                Some(PermissionMode::Default),
                false,
                "3-stop",
            )
            .await;
        let mut asked = Vec::new();
        let reached = real
            .run_until(&attempt.id, &task.id, info.seq, WAIT_41, &mut asked, |a| {
                real.gate("d", a, |c| c == TOUCH || c == WAIT_41_CMD, false)
            })
            .await;
        real.note("b_session_rule_after_resume", json!({
            "touch_asked": asked.iter().any(is_touch),
            "approvals": asked.iter().map(Asked::json).collect::<Vec<_>>()}));
        real.check(matches!(reached, Reached::Running), "d: the wait never ran");
        let row3 = real.processes(&attempt.id)[2].clone();
        let waits = running(WAIT_41).await;
        let orphan_proof = real.orphan_proof(&row3).await;
        let t0 = Instant::now();
        real.core.stop_attempt(AttemptIdReq { attempt_id: attempt.id.clone() }).await.unwrap();
        let d = real.turn_end(&task.id, info.seq).await;
        let elapsed = t0.elapsed();
        let p3 = &d.processes[2];
        let row3 = real.processes(&attempt.id)[2].clone();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !running(WAIT_41).await.is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let left = running(WAIT_41).await;
        real.note("d_stop", json!({
            "status": p3.status, "stop_reason": p3.stop_reason,
            "result_subtype": p3.result_subtype, "is_error": p3.is_error,
            "exit_code": row3.exit_code, "elapsed_ms": elapsed.as_millis() as u64,
            "turn_end": real.turn_end_entry(&p3.id),
            "wait_processes_pgid_is_claude_pgid": waits.iter().map(|(_, g)| Some(*g) == row3.pid).collect::<Vec<_>>(),
            "wait_left_10s_after_stop": !left.is_empty(),
            "ps_shows_claude_path_and_session": orphan_proof,
            "raw_kinds": real.raw_types(&attempt.id, &p3.id),
            "tool_calls": real.tool_calls(&p3.id), "notices": real.notices(&p3.id)}));
        real.check(
            (p3.status, p3.stop_reason) == (ProcessStatus::Killed, Some(StopReason::UserStop)),
            format!("d: stop {p3:?}"),
        );
        real.check(orphan_proof, "d: ps does not show the claude path and the session id");
        real.check(left.is_empty(), format!("d: the wait outlived the stop: {left:?}"));
        kill_pids(&left);
        real.db().add_allow_rules(&attempt.id, &rules, now_ms()).unwrap();

        // (b) A NEW session (fresh_session): only our --settings allow rule can spare the ask.
        // (e) Then the app dies while a 42-second wait runs: the runtime drops mid-turn.
        let info = real
            .follow_up(
                &attempt.id,
                "As part of this task, run two shell commands, each in its own Bash tool call: \
                 first `touch approved.txt`, then `python3 -c 'import time; time.sleep(42)'` (a \
                 deliberate 42-second wait). Then reply DONE.",
                Some(PermissionMode::Default),
                true,
                "4-interrupted",
            )
            .await;
        let mut asked = Vec::new();
        let reached = real
            .run_until(&attempt.id, &task.id, info.seq, WAIT_42, &mut asked, |a| {
                real.gate("e", a, |c| c == TOUCH || c == WAIT_42_CMD, false)
            })
            .await;
        let row4 = real.processes(&attempt.id)[3].clone();
        real.note("b_settings_allow_in_new_session", json!({
            "argv_session_id": argv_of(&row4).contains(&format!("--session-id={}", row4.session_id)),
            "touch_asked": asked.iter().any(is_touch),
            "approvals": asked.iter().map(Asked::json).collect::<Vec<_>>()}));
        real.check(!asked.iter().any(is_touch), "b: the --settings allow rule did not spare the ask");
        real.check(matches!(reached, Reached::Running), "e: the wait never ran before the drop");
        let waits = running(WAIT_42).await;
        real.note("e_before_drop", json!({
            "wait_processes_pgid_is_claude_pgid": waits.iter().map(|(_, g)| Some(*g) == row4.pid).collect::<Vec<_>>()}));
        real.check(real.orphan_proof(&row4).await, "e: ps does not show the orphan proof");
        (real, task, attempt, row4.pid.unwrap())
    });
    drop(first);
    reap(pgid);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let gone = || !claude::group_alive(pgid) && running_blocking(WAIT_42).is_empty();
    while !gone() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let left = running_blocking(WAIT_42);
    real.note(
        "e_after_drop",
        json!({"group_alive": claude::group_alive(pgid), "wait_left": left}),
    );
    real.check(
        !claude::group_alive(pgid),
        "e: the dropped runtime left the agent's group alive",
    );
    real.check(
        left.is_empty(),
        format!("e: the wait outlived the dropped runtime: {left:?}"),
    );
    kill_pids(&left);

    runtime().block_on(async {
        let mut real = real;
        real.restart().await;
        let d = real.detail(&task.id).await;
        let p5 = &d.processes[3];
        let card = real
            .core
            .get_board(ProjectIdReq { project_id: real.project.id.clone() })
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.task.id == task.id)
            .unwrap();
        real.subscribe(&attempt.id).await;
        real.until("the snapshot", TURN, async || !real.seen.store().is_empty()).await;
        real.note("e_after_restart", json!({
            "status": p5.status, "stop_reason": p5.stop_reason,
            "task_status": d.task.status, "card_last_stop_reason": card.last_stop_reason,
            "notices": real.notices(&p5.id)}));
        real.check(
            (p5.status, p5.stop_reason) == (ProcessStatus::Failed, Some(StopReason::AppRestart)),
            format!("e: after restart {p5:?}"),
        );
        real.check(card.last_stop_reason == Some(StopReason::AppRestart), "e: card");

        // "Continua": CONTINUE_PROMPT with --resume, in the attempt's own mode.
        let info = real.follow_up(&attempt.id, CONTINUE_PROMPT, None, false, "5-continue").await;
        let (asked, d) = real
            .drive(&attempt.id, &task.id, info.seq, |_| ApprovalDecision::Deny {
                message: "Skip it: do not run commands now, just reply DONE.".into(),
                interrupt: false,
            })
            .await;
        let p6 = &d.processes[4];
        let row6 = real.processes(&attempt.id)[4].clone();
        let resumed = argv_of(&row6).contains(&format!("--resume={}", row6.session_id));
        real.note("e_continue", json!({
            "argv_resume": resumed, "status": p6.status, "result_subtype": p6.result_subtype,
            "session_init": real.session_init(&p6.id), "said": real.said(&p6.id),
            "approvals": asked.iter().map(Asked::json).collect::<Vec<_>>(),
            "notices": real.notices(&p6.id)}));
        real.check(resumed && p6.status == ProcessStatus::Completed, format!("e: continue {p6:?}"));
        real.check(real.session_init(&p6.id).is_some(), "e: continue without system/init");

        // (h) Merge: squash commit on the clone's main.
        let diff = real.core.get_diff(AttemptIdReq { attempt_id: attempt.id.clone() }).await.unwrap();
        let outcome = real
            .core
            .merge_attempt(MergeAttemptReq {
                attempt_id: attempt.id.clone(),
                message: "M5: add one line to README.md".into(),
            })
            .await
            .unwrap();
        let MergeOutcome::Merged { commit, .. } = &outcome else {
            panic!("merge: {outcome:?}");
        };
        let head = common::git(&real.repo, &["rev-parse", "main"]);
        let parents = common::git(&real.repo, &["rev-list", "--parents", "-n", "1", "main"]);
        let files = common::git(&real.repo, &["show", "--name-only", "--format=", "main"]);
        let readme = std::fs::read_to_string(real.repo.join("README.md")).unwrap();
        real.note("h_merge", json!({
            "outcome": outcome, "diff_files": diff.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
            "main_is_commit": &head == commit, "parents": parents.split(' ').count() - 1,
            "files": files.lines().collect::<Vec<_>>(),
            "readme_has_line": readme.contains("Edited by AI Task Manager (M5).")}));
        real.check(&head == commit, "h: main is not the squash commit");
        real.check(parents.split(' ').count() == 2, "h: a squash commit has one parent");
        real.check(readme.contains("Edited by AI Task Manager (M5)."), "h: README");
        real.check(common::git(&real.remote, &["for-each-ref"]).is_empty(), "h: pushed");
        real.finish().await;
    });
    assert_eq!(
        toy_state(&toy),
        toy_before,
        "the toy repo must never change"
    );
}

/// Spec §12.3 step 7: `git push` is denied by the `--settings` deny rule, without asking (the
/// rule, not the model). 1 real turn.
#[test]
#[ignore = "real Claude Code CLI: ATM_REAL_CLAUDE=1, see the module docs"]
fn real_cli_git_push_is_denied_by_the_settings_rule() {
    let Some((_serial, guard)) = guard() else {
        return;
    };
    let toy = toy_repo().expect("the toy repo (ATM_REAL_CLAUDE_REPO)");
    let toy_before = toy_state(&toy);
    runtime().block_on(async {
        let real = Real::new(guard, "push", Source::Toy(&toy)).await;
        let task = real
            .task(
                "Permission rule check",
                "This task checks the host app's permission configuration. The only git remote \
                 is a throwaway local bare repository created for this check. Use the Bash tool \
                 exactly once to run `git push`. A permission deny rule is expected to block it \
                 before it runs: do not try any workaround and do not retry. Then reply with the \
                 exact error message the tool returned.",
            )
            .await;
        let attempt = real.start(&task, PermissionMode::Default, "1-git-push").await;
        let (asked, d) = real
            .drive(&attempt.id, &task.id, 1, |_| ApprovalDecision::Deny {
                message: "Not approved.".into(),
                interrupt: false,
            })
            .await;
        let p = &d.processes[0];
        let calls = real.tool_calls(&p.id);
        let pushes: Vec<&Value> = calls
            .iter()
            .filter(|c| c["summary"].as_str().is_some_and(|s| s.starts_with("$ git push")))
            .collect();
        let result = real
            .raw_log(&attempt.id, &p.id)
            .into_iter()
            .find(|v| v["type"] == "result")
            .unwrap_or_default();
        let refs = common::git(&real.remote, &["for-each-ref"]);
        real.note("f_push", json!({
            "tool_calls": calls, "approvals": asked.iter().map(Asked::json).collect::<Vec<_>>(),
            "turn_end": real.turn_end_entry(&p.id), "permission_denials": result["permission_denials"],
            "remote_refs": refs, "said": real.said(&p.id), "status": p.status}));
        assert!(!pushes.is_empty(), "the model never ran git push: the deny rule is unproven");
        assert!(
            !asked.iter().any(|a| a.summary.starts_with("$ git push")),
            "git push asked for approval: the --settings deny rule did not apply"
        );
        assert!(pushes.iter().all(|c| c["status"]["state"] == "Failed"), "{pushes:?}");
        assert!(
            result["permission_denials"].as_array().is_some_and(|d| d.iter().any(|x| x["tool_name"] == "Bash")),
            "no permission_denials in the result"
        );
        assert_eq!(refs, "", "something reached the remote");
        real.finish().await;
    });
    assert_eq!(
        toy_state(&toy),
        toy_before,
        "the toy repo must never change"
    );
}

/// Spec §12.3 step 8 (E11): a repo whose `.claude/settings.json` hooks and `.mcp.json` server
/// write a marker and whose CLAUDE.md holds a keyword. Isolated: no marker, 0 MCP servers;
/// records whether the agent knows the keyword. 1 real turn (plus a free control spawn).
#[test]
#[ignore = "real Claude Code CLI: ATM_REAL_CLAUDE=1, see the module docs"]
fn real_cli_isolated_project_config() {
    let Some((_serial, guard)) = guard() else {
        return;
    };
    let marker_dir = common::tempdir();
    let marker_root = marker_dir.path().canonicalize().unwrap();
    let marker = marker_root.join("marker");
    let input = marker_root.join("hook-input.json");
    let mcp = json!({"mcpServers": {"m5-marker": {"command": "/bin/sh",
        "args": ["-c", format!("echo mcp >> '{}'", marker.display())]}}});
    let files = [
        (".claude/settings.json", hook_settings(&marker, &input)),
        (".mcp.json", mcp.to_string()),
        (
            "CLAUDE.md",
            format!("# Project notes\n\nThe project keyword is {KEYWORD}.\n"),
        ),
    ];
    runtime().block_on(async {
        let real = Real::new(guard, "isolated", Source::Fresh(&files)).await;
        // Control (free, no user message): the same config does run with the project source.
        let mut argv = probe_argv(&real.guard, &real.repo, PermissionMode::AcceptEdits, false);
        argv.retain(|a| a != "--setting-sources=user" && a != "--strict-mcp-config");
        let run = raw(
            &real.guard,
            &argv,
            &real.repo,
            &[initialize()],
            Duration::from_secs(90),
            close_on_answer,
        )
        .await;
        let control = std::fs::read_to_string(&marker).ok();
        real.note(
            "g_control_user_project",
            json!({"exit": run.exit, "marker": control}),
        );
        assert!(
            control.is_some_and(|m| m.contains("SessionStart")),
            "the hook is not armed"
        );
        std::fs::remove_file(&marker).unwrap();

        let task = real
            .task(
                "Project keyword",
                "Answer in two lines. Line 1, before using any tool: if the content of a \
                 CLAUDE.md file of this repository is already in your context, write its \
                 project keyword, otherwise write NOT_IN_CONTEXT. Line 2: follow your \
                 instructions about CLAUDE.md, then write the project keyword.",
            )
            .await;
        let attempt = real
            .start(&task, PermissionMode::AcceptEdits, "1-keyword")
            .await;
        let (asked, d) = real
            .drive(&attempt.id, &task.id, 1, |a| {
                real.gate("g", a, reads_claude_md, false)
            })
            .await;
        let p = &d.processes[0];
        let said = real.said(&p.id);
        let init = real.session_init(&p.id).unwrap_or_default();
        let calls = real.tool_calls(&p.id);
        let marker_after = std::fs::read_to_string(&marker).ok();
        real.note(
            "g_isolated",
            json!({
            "marker": marker_after, "session_init": init, "said": said,
            "keyword_known": said.contains(KEYWORD),
            "read_claude_md": calls.iter().any(|c| c["summary"] == "Read CLAUDE.md"),
            "first_line": said.lines().next(),
            "tool_calls": calls, "approvals": asked.iter().map(Asked::json).collect::<Vec<_>>(),
            "status": p.status}),
        );
        assert_eq!(
            marker_after, None,
            "a project hook or MCP server ran in Isolated"
        );
        assert_eq!(init["mcp_servers"], 0);
        assert_eq!(p.status, ProcessStatus::Completed);
        real.finish().await;
    });
    drop(marker_dir);
}

/// The sanitizer itself (no CLI): runs with the normal test suite.
#[test]
fn sanitizer_scrubs_account_home_and_hook_output() {
    let s = Sanitizer::new(vec![
        ("/Users/alice".into(), "<HOME>".into()),
        ("Alice's Org".into(), "<org>".into()),
    ]);
    assert_eq!(
        s.text("mail a.b+c@example.co.uk, org Alice's Org, dir /Users/alice/x /Users/bob/y"),
        "mail <email>, org <org>, dir <HOME>/x /Users/<user>/y"
    );
    assert_eq!(
        s.text("icon@2x.png and @scope/pkg"),
        "<email> and @scope/pkg"
    );
    assert_eq!(s.text("see @example.com"), "see @<domain>");
    let hook = json!({"type":"system","subtype":"hook_response","output":"secret","stdout":"",
        "session_id":"s"});
    let init = json!({"type":"control_response","response":{"subtype":"success",
        "request_id":"atm_1_0","response":{"commands":[1,2,3,4],
        "account":{"email":"alice@example.com","organization":"Alice's Org",
                   "subscriptionType":"Claude Max","apiProvider":"firstParty"}}}});
    let out = s.jsonl(&format!("{hook}\n{init}\nnot json\n"));
    let lines: Vec<&str> = out.lines().collect();
    let hook: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(hook["output"], "<redacted: user hook output>");
    let init: Value = serde_json::from_str(lines[1]).unwrap();
    let response = &init["response"]["response"];
    assert_eq!(response["commands"], json!([1, 2]));
    assert_eq!(response["account"]["email"], wire::REDACTED);
    assert_eq!(response["account"]["organization"], wire::REDACTED);
    assert_eq!(response["account"]["subscriptionType"], "Claude Max");
    assert_eq!(lines[2], "not json");
    assert!(!out.contains("alice") && !out.contains("Alice"));
}

// ---- the guard itself (no CLI): run with the normal test suite -----------------------------

/// Condition (2) accepts the `initialize` answer M5 captured (claude.ai login, Max) and refuses
/// every answer that does not prove a first-party subscription.
#[test]
fn preflight_accepts_only_a_subscription_account() {
    let captured = include_str!("fixtures/real/probes-initialize.stdout.jsonl")
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["type"] == "control_response")
        .unwrap();
    let answer = captured["response"].clone();
    assert_eq!(subscription_account(&answer), Ok(()));
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut a = answer.clone();
        edit(&mut a);
        subscription_account(&a)
    };
    for bad in [
        with(&|a| a["subtype"] = "error".into()),
        with(&|a| a["response"]["account"] = Value::Null),
        with(&|a| a["response"]["account"]["apiProvider"] = "bedrock".into()),
        with(&|a| {
            a["response"]["account"]
                .as_object_mut()
                .unwrap()
                .remove("apiProvider");
        }),
        with(&|a| a["response"]["account"]["subscriptionType"] = " ".into()),
        with(&|a| {
            a["response"]["account"]
                .as_object_mut()
                .unwrap()
                .remove("subscriptionType");
        }),
        with(&|a| a["response"]["account"]["apiKeySource"] = "ANTHROPIC_API_KEY".into()),
        with(&|a| a["response"]["account"]["tokenSource"] = "apiKeyHelper".into()),
    ] {
        assert!(bad.is_err(), "{bad:?}");
    }
    assert_eq!(
        with(&|a| a["response"]["account"]["apiKeySource"] = "none".into()),
        Ok(())
    );
}

/// Condition (3) on raw lines: silent on every capture of M5, trips on an API-key source, a
/// missing one, or model output before `system/init`.
#[test]
fn key_trip_on_raw_lines() {
    for capture in [
        include_str!("fixtures/real/probes-no-initialize.stdout.jsonl"),
        include_str!("fixtures/real/checklist-1-readme.stdout.jsonl"),
    ] {
        let mut init_seen = false;
        for line in capture.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(key_trip(&v, &mut init_seen), None, "{line}");
        }
        assert!(init_seen);
    }
    let init = |source: Value| json!({"type":"system","subtype":"init","apiKeySource":source});
    for bad in [init(json!("ANTHROPIC_API_KEY")), init(Value::Null)] {
        assert!(key_trip(&bad, &mut false).is_some(), "{bad}");
    }
    for early in ["assistant", "stream_event", "result"] {
        let line = json!({"type": early});
        assert!(key_trip(&line, &mut false).is_some(), "{line}");
        assert_eq!(key_trip(&line, &mut true), None, "{line}");
    }
}

/// Condition (3) in the Core's transcript: a live upsert of model output before the process's
/// `SessionInit` trips, a snapshot (a tail, which may start after an old `SessionInit`) never.
#[test]
fn transcript_output_before_session_init_trips() {
    let entry = |idx: u32, process: &str, body: EntryBody| Entry {
        idx,
        rev: 0,
        process_id: process.into(),
        ts: 0,
        parent_tool_use_id: None,
        body,
    };
    let text = |idx, process| entry(idx, process, EntryBody::AssistantText { text: "hi".into() });
    let init = |idx, process, source: Option<&str>| {
        entry(
            idx,
            process,
            EntryBody::SessionInit {
                model: None,
                permission_mode: None,
                api_key_source: source.map(str::to_owned),
                mcp_servers: 0,
                warnings: Vec::new(),
            },
        )
    };
    let seen = Seen::new();
    let sink = seen.sink();
    sink(TranscriptMsg::Snapshot {
        entries: vec![text(1, "old")],
        has_more: true,
        typing: None,
    });
    sink(TranscriptMsg::Upsert {
        entries: vec![init(2, "p1", Some("none")), text(3, "p1")],
    });
    assert_eq!(*seen.bad_key.lock().unwrap(), None);
    sink(TranscriptMsg::Upsert {
        entries: vec![text(4, "p2")],
    });
    assert!(seen.bad_key.lock().unwrap().is_some());

    let seen = Seen::new();
    let sink = seen.sink();
    sink(TranscriptMsg::Upsert {
        entries: vec![init(1, "p1", None)],
    });
    assert_eq!(
        seen.bad_key.lock().unwrap().as_deref(),
        Some("apiKeySource <absent>")
    );
}

/// The allowlists of the checklist: the exact commands the prompts name, nothing chained.
#[test]
fn approval_allowlists() {
    for ok in ["ls", "ls -la", "ls -l -a"] {
        assert!(is_ls(ok), "{ok}");
    }
    for bad in [
        "ls; rm -rf x",
        "ls && curl x",
        "ls /",
        "lsof",
        "ls -",
        "ls -1",
    ] {
        assert!(!is_ls(bad), "{bad}");
    }
    for ok in [
        "cat CLAUDE.md",
        "cat /tmp/atm-real/wt/7982576f-ce7d/CLAUDE.md 2>/dev/null | head -5",
        "cat ./CLAUDE.md | head -20",
    ] {
        assert!(reads_claude_md(ok), "{ok}");
    }
    for bad in [
        "cat CLAUDE.md; rm -rf ~",
        "cat ~/.ssh/id_rsa CLAUDE.md",
        "cat CLAUDE.md | sh",
        "cat $(echo x)/CLAUDE.md",
        "cat README.md",
    ] {
        assert!(!reads_claude_md(bad), "{bad}");
    }
}
