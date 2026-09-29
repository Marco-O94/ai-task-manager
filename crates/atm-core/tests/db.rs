//! M2-DB acceptance (spec §5, §11.2): migrations, constraints, positions, board join,
//! attempts and processes, orphan recovery, transcript entries.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;

use atm_core::db::{
    self, AttemptRow, Db, MAX_ENTRY_PAGE, MIGRATIONS, POSITION_GAP, ProcessFinish, ProcessRow,
    ProjectRow,
};
use atm_types::{
    AppError, AttemptState, ConfigPolicy, CreateTaskReq, Effort, Entry, EntryBody, EntryPage,
    ErrorCode, PermissionMode, ProcessStatus, Settings, StopReason, TaskStatus, ToolStatus,
    UpdateProjectReq, UpdateTaskReq, WorktreeState,
};
use rusqlite::{Connection, ffi};

const NOW: i64 = 1_700_000_000_000;

// ---- fixtures ---------------------------------------------------------------------------------

fn project(id: &str, name: &str) -> ProjectRow {
    ProjectRow {
        id: id.into(),
        name: name.into(),
        repo_path: format!("/repos/{id}"),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::AcceptEdits,
        default_model: None,
        config_policy: ConfigPolicy::Isolated,
        trusted_fingerprint: None,
        allow_bypass: false,
        created_at: NOW,
        updated_at: NOW,
    }
}

fn new_task(project_id: &str, title: &str, status: Option<TaskStatus>) -> CreateTaskReq {
    CreateTaskReq {
        project_id: project_id.into(),
        title: title.into(),
        description: String::new(),
        status,
    }
}

fn attempt(id: &str, task_id: &str, created_at: i64) -> AttemptRow {
    AttemptRow {
        id: id.into(),
        task_id: task_id.into(),
        state: AttemptState::Active,
        branch: format!("atm/{id}"),
        target_branch: "main".into(),
        base_commit: "0".repeat(40),
        worktree_path: format!("/wt/{id}"),
        worktree_state: WorktreeState::Present,
        session_id: format!("session-{id}"),
        session_started: false,
        permission_mode: PermissionMode::AcceptEdits,
        model: None,
        effort: None,
        allow_rules: Vec::new(),
        merge_commit: None,
        created_at,
        updated_at: created_at,
        closed_at: None,
    }
}

fn process(id: &str, attempt_id: &str, seq: u32, instance: &str) -> ProcessRow {
    ProcessRow {
        id: id.into(),
        attempt_id: attempt_id.into(),
        seq,
        prompt: format!("turn {seq}"),
        permission_mode: PermissionMode::AcceptEdits,
        session_id: format!("session-{attempt_id}"),
        resumed: seq > 1,
        status: ProcessStatus::Running,
        stop_reason: None,
        error: None,
        cli_version: None,
        argv_json: r#"["claude","-p"]"#.into(),
        pid: None,
        app_instance_id: instance.into(),
        exit_code: None,
        result_subtype: None,
        is_error: None,
        cost_usd_estimate: None,
        num_turns: None,
        duration_ms: None,
        head_before: None,
        head_after: None,
        started_at: NOW + i64::from(seq),
        finished_at: None,
    }
}

fn finish(status: ProcessStatus, at: i64) -> ProcessFinish {
    ProcessFinish {
        status,
        stop_reason: None,
        error: None,
        exit_code: Some(0),
        result_subtype: Some("success".into()),
        is_error: Some(false),
        cost_usd_estimate: Some(0.25),
        num_turns: Some(3),
        duration_ms: Some(1_500),
        head_after: Some("f".repeat(40)),
        finished_at: at,
    }
}

fn entry(idx: u32, rev: u32, process_id: &str, body: EntryBody) -> Entry {
    Entry {
        idx,
        rev,
        process_id: process_id.into(),
        ts: NOW + i64::from(idx),
        parent_tool_use_id: None,
        body,
    }
}

fn say(idx: u32, rev: u32, text: &str) -> Entry {
    let body = EntryBody::AssistantText { text: text.into() };
    entry(idx, rev, "r1", body)
}

fn tool(idx: u32, process_id: &str, status: ToolStatus) -> Entry {
    let body = EntryBody::ToolCall {
        tool_use_id: format!("toolu_{idx}"),
        name: "Bash".into(),
        summary: "ls".into(),
        input: r#"{"command":"ls"}"#.into(),
        status,
        output: None,
    };
    entry(idx, 1, process_id, body)
}

/// Project `p` with task `t` in todo.
fn seeded() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.insert_project(&project("p", "Progetto")).unwrap();
    db.insert_task("t", &new_task("p", "Task", None), NOW)
        .unwrap();
    db
}

/// `seeded()` plus attempt `a1` with its running turn `r1`.
fn with_attempt() -> Db {
    let db = seeded();
    db.begin_attempt(&attempt("a1", "t", NOW), &process("r1", "a1", 1, "i"), NOW)
        .unwrap();
    db
}

fn err<T>(r: Result<T, AppError>) -> AppError {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e,
    }
}

fn idxs(page: &EntryPage) -> Vec<u32> {
    page.entries.iter().map(|e| e.idx).collect()
}

// ---- raw schema (invariants the typed API cannot even express) --------------------------------

fn raw_attempt(c: &Connection, id: &str, state: &str) -> rusqlite::Result<usize> {
    c.execute(
        "INSERT INTO attempts (id, task_id, state, branch, target_branch, base_commit,
            worktree_path, session_id, permission_mode, created_at, updated_at)
         VALUES (?1, 't', ?2, 'atm/' || ?1, 'main', 'c0', '/wt/' || ?1, 's-' || ?1,
            'acceptEdits', 0, 0)",
        [id, state],
    )
}

fn raw_process(
    c: &Connection,
    id: &str,
    attempt_id: &str,
    seq: u32,
    status: &str,
) -> rusqlite::Result<usize> {
    c.execute(
        "INSERT INTO processes (id, attempt_id, seq, prompt, permission_mode, session_id,
            resumed, status, argv_json, app_instance_id, started_at)
         VALUES (?1, ?2, ?3, 'go', 'acceptEdits', 's-' || ?2, 0, ?4, '[]', 'i', 0)",
        rusqlite::params![id, attempt_id, seq, status],
    )
}

/// Migrated connection with project `p`, task `t`, active attempt `a`, running turn `r` and
/// one entry.
fn raw() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch(db::PRAGMAS).unwrap();
    db::migrate(&mut c).unwrap();
    c.execute_batch(
        "INSERT INTO projects (id, name, repo_path, default_target_branch, created_at, updated_at)
            VALUES ('p', 'P', '/r', 'main', 0, 0);
         INSERT INTO tasks (id, project_id, title, position, created_at, updated_at)
            VALUES ('t', 'p', 'T', 1024, 0, 0);",
    )
    .unwrap();
    raw_attempt(&c, "a", "active").unwrap();
    raw_process(&c, "r", "a", 1, "running").unwrap();
    c.execute(
        "INSERT INTO entries (attempt_id, idx, rev, process_id, kind, payload, ts)
         VALUES ('a', 0, 1, 'r', 'Stderr', '{}', 0)",
        [],
    )
    .unwrap();
    c
}

fn sqlite_code(r: rusqlite::Result<usize>) -> i32 {
    r.expect_err("statement should be rejected")
        .sqlite_error()
        .expect("an SQLite error")
        .extended_code
}

fn count(c: &Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

// ---- migrations -------------------------------------------------------------------------------

#[test]
fn migrates_in_memory_idempotently() {
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch(db::PRAGMAS).unwrap();
    db::migrate(&mut c).unwrap();
    db::migrate(&mut c).unwrap();

    let version: i64 = c
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, MIGRATIONS.len() as i64);
    let foreign_keys: i64 = c
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
    let tables: Vec<String> = c
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tables,
        [
            "attempts",
            "entries",
            "processes",
            "projects",
            "settings",
            "tasks"
        ]
    );

    c.pragma_update(None, "user_version", 99).unwrap();
    assert_eq!(
        err(db::migrate(&mut c)).code,
        ErrorCode::Db,
        "schema from a newer app"
    );

    let db = Db::open_in_memory().unwrap();
    assert_eq!(db.settings().unwrap(), Settings::default());
}

#[test]
fn open_creates_a_private_wal_file_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atm.sqlite3");
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    {
        let db = Db::open(&path).unwrap();
        db.insert_project(&project("p", "Progetto")).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("atm.sqlite3-wal")), 0o600);
    }
    let journal: String = Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(journal, "wal");

    let db = Db::open(&path).unwrap();
    assert_eq!(db.projects().unwrap().len(), 1);

    let missing = dir.path().join("missing").join("atm.sqlite3");
    let e = err(Db::open(&missing));
    assert_eq!(e.code, ErrorCode::Io);
    assert!(e.message.contains("missing"), "{e}");
}

// ---- constraints ------------------------------------------------------------------------------

#[test]
fn foreign_keys_cascade() {
    let db = with_attempt();
    db.upsert_entries("a1", &[say(0, 1, "ciao")]).unwrap();
    db.insert_task("t2", &new_task("p", "Altro", None), NOW)
        .unwrap();
    db.begin_attempt(&attempt("a2", "t2", NOW), &process("r2", "a2", 1, "i"), NOW)
        .unwrap();

    db.delete_task("t").unwrap();
    assert_eq!(err(db.attempt("a1")).code, ErrorCode::NotFound);
    assert_eq!(err(db.process("r1")).code, ErrorCode::NotFound);
    assert!(db.entries_tail("a1", 10).unwrap().entries.is_empty());
    assert_eq!(db.attempt("a2").unwrap().task_id, "t2");

    db.delete_project("p").unwrap();
    assert_eq!(err(db.task("t2")).code, ErrorCode::NotFound);
    assert_eq!(err(db.attempt("a2")).code, ErrorCode::NotFound);
    assert_eq!(err(db.process("r2")).code, ErrorCode::NotFound);
    assert!(db.projects().unwrap().is_empty());

    // Dangling references are refused.
    let e = err(db.insert_task("t3", &new_task("nope", "Orfano", None), NOW));
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(
        err(db.upsert_entries("nope", &[say(0, 1, "x")])).code,
        ErrorCode::NotFound
    );

    // Same cascade at the SQL level, down to the entries.
    let c = raw();
    c.execute("DELETE FROM projects", []).unwrap();
    for table in ["tasks", "attempts", "processes", "entries"] {
        assert_eq!(count(&c, table), 0, "{table}");
    }
}

#[test]
fn check_constraints_reject_invalid_rows() {
    let db = seeded();
    let long = "x".repeat(201);
    for title in ["", long.as_str()] {
        let e = err(db.insert_task("bad", &new_task("p", title, None), NOW));
        assert_eq!(e.code, ErrorCode::Invalid, "title of {} chars", title.len());
    }
    let mut req = new_task("p", "Descrizione enorme", None);
    req.description = "d".repeat(100_001);
    assert_eq!(
        err(db.insert_task("bad", &req, NOW)).code,
        ErrorCode::Invalid
    );
    // Lengths count characters, not bytes.
    db.insert_task("ok", &new_task("p", &"è".repeat(200), None), NOW)
        .unwrap();
    let update = UpdateTaskReq {
        id: "t".into(),
        title: String::new(),
        description: String::new(),
    };
    assert_eq!(err(db.update_task(&update, NOW)).code, ErrorCode::Invalid);
    assert_eq!(
        err(db.insert_project(&project("q", ""))).code,
        ErrorCode::Invalid
    );
    assert_eq!(
        db.task("t").unwrap().title,
        "Task",
        "failed update left no trace"
    );

    let c = raw();
    for sql in [
        "UPDATE projects SET default_permission_mode = 'plan'",
        "UPDATE projects SET config_policy = 'open'",
        "UPDATE projects SET allow_bypass = 2",
        "UPDATE tasks SET status = 'doing'",
        "UPDATE attempts SET state = 'paused'",
        "UPDATE attempts SET worktree_state = 'gone'",
        "UPDATE attempts SET session_started = 2",
        "UPDATE attempts SET permission_mode = 'auto'",
        "UPDATE attempts SET effort = 'huge'",
        "UPDATE attempts SET allow_rules = '[\"Bash(ls)\"'",
        "UPDATE processes SET status = 'zombie'",
        "UPDATE processes SET stop_reason = 'bored'",
        "UPDATE processes SET resumed = 2",
        "UPDATE processes SET is_error = 3",
        "UPDATE processes SET argv_json = 'claude -p'",
        "UPDATE entries SET payload = '{'",
        "INSERT INTO settings (key, value) VALUES ('max_running', 'two')",
    ] {
        assert_eq!(
            sqlite_code(c.execute(sql, [])),
            ffi::SQLITE_CONSTRAINT_CHECK,
            "{sql}"
        );
    }
    // Every enum string of atm-types passes its CHECK.
    for e in Effort::ALL {
        c.execute("UPDATE attempts SET effort = ?1", [e.as_str()])
            .unwrap();
    }
    for s in StopReason::ALL {
        c.execute("UPDATE processes SET stop_reason = ?1", [s.as_str()])
            .unwrap();
    }
    // STRICT tables refuse values of the wrong type.
    let e = sqlite_code(c.execute("UPDATE tasks SET position = 'first'", []));
    assert_eq!(e, ffi::SQLITE_CONSTRAINT_DATATYPE);
}

#[test]
fn partial_unique_indexes_reject_a_second_active_attempt_and_running_turn() {
    let c = raw();
    let e = sqlite_code(raw_attempt(&c, "b", "active"));
    assert_eq!(e, ffi::SQLITE_CONSTRAINT_UNIQUE, "second active attempt");
    raw_attempt(&c, "old", "discarded").unwrap();
    raw_attempt(&c, "done", "merged").unwrap();

    let e = sqlite_code(raw_process(&c, "r2", "a", 2, "running"));
    assert_eq!(e, ffi::SQLITE_CONSTRAINT_UNIQUE, "second running turn");
    raw_process(&c, "r2", "a", 2, "completed").unwrap();
    raw_process(&c, "x1", "old", 1, "running").unwrap();

    // Through the API: Conflict and Busy, with the whole transaction rolled back.
    let db = with_attempt();
    let e = err(db.begin_attempt(&attempt("a2", "t", NOW), &process("r2", "a2", 1, "i"), NOW));
    assert_eq!(e.code, ErrorCode::Conflict);
    assert_eq!(err(db.attempt("a2")).code, ErrorCode::NotFound);
    assert_eq!(err(db.process("r2")).code, ErrorCode::NotFound);

    assert_eq!(
        err(db.begin_turn(&process("r1b", "a1", 2, "i"), NOW)).code,
        ErrorCode::Busy
    );
    assert_eq!(err(db.process("r1b")).code, ErrorCode::NotFound);
    db.finish_process("r1", &finish(ProcessStatus::Completed, NOW + 1))
        .unwrap();
    db.begin_turn(&process("r1b", "a1", 2, "i"), NOW + 2)
        .unwrap();
    db.finish_process("r1b", &finish(ProcessStatus::Completed, NOW + 3))
        .unwrap();
    db.finish_discard("a1", NOW + 4).unwrap();

    let mut dup = attempt("a3", "t", NOW + 5);
    dup.worktree_path = "/wt/a1".into();
    let e = err(db.begin_attempt(&dup, &process("r3", "a3", 1, "i"), NOW + 5));
    assert_eq!(e.code, ErrorCode::Conflict, "worktree path reused");
    let mut dup = attempt("a3", "t", NOW + 5);
    dup.session_id = "session-a1".into();
    let e = err(db.begin_attempt(&dup, &process("r3", "a3", 1, "i"), NOW + 5));
    assert_eq!(e.code, ErrorCode::Conflict, "session id reused");
    db.begin_attempt(
        &attempt("a3", "t", NOW + 5),
        &process("r3", "a3", 1, "i"),
        NOW + 5,
    )
    .unwrap();
}

// ---- positions --------------------------------------------------------------------------------

#[test]
fn positions_use_gap_midpoint_and_column_ends() {
    let db = seeded();
    db.delete_task("t").unwrap();
    for id in ["a", "b", "c"] {
        db.insert_task(id, &new_task("p", id, None), NOW).unwrap();
    }
    let pos = |id: &str| db.task(id).unwrap().position;
    assert_eq!([pos("a"), pos("b"), pos("c")], [1024.0, 2048.0, 3072.0]);

    db.move_task("c", TaskStatus::Todo, Some("b"), NOW).unwrap();
    assert_eq!(pos("c"), 1536.0, "midpoint");
    db.move_task("a", TaskStatus::Todo, None, NOW).unwrap();
    assert_eq!(pos("a"), 3072.0, "max of the others + GAP");
    db.move_task("b", TaskStatus::Todo, Some("c"), NOW).unwrap();
    assert_eq!(pos("b"), 1024.0, "before the first: GAP below it");
    db.move_task("b", TaskStatus::Todo, Some("b"), NOW + 1)
        .unwrap();
    assert_eq!(
        db.task("b").unwrap().updated_at,
        NOW,
        "before itself: no-op"
    );

    db.set_task_status("c", TaskStatus::InReview, NOW).unwrap();
    assert_eq!(pos("c"), POSITION_GAP, "first of an empty column");
    let d = db
        .insert_task("d", &new_task("p", "d", Some(TaskStatus::InReview)), NOW)
        .unwrap();
    assert_eq!((d.status, d.position), (TaskStatus::InReview, 2048.0));
    db.set_task_status("d", TaskStatus::InReview, NOW + 9)
        .unwrap();
    assert_eq!(db.task("d").unwrap().updated_at, NOW, "same status: no-op");

    let e = err(db.move_task("a", TaskStatus::Todo, Some("c"), NOW));
    assert_eq!(e.code, ErrorCode::Invalid, "before_id in another column");
    let e = err(db.move_task("a", TaskStatus::Todo, Some("nope"), NOW));
    assert_eq!(e.code, ErrorCode::Invalid);
    let e = err(db.move_task("nope", TaskStatus::Todo, None, NOW));
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(
        err(db.set_task_status("nope", TaskStatus::Done, NOW)).code,
        ErrorCode::NotFound
    );
}

/// Deterministic PRNG (xorshift64): the test needs no RNG dependency.
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[test]
fn five_hundred_random_moves_keep_strict_order_with_renumbering() {
    let db = Db::open_in_memory().unwrap();
    db.insert_project(&project("p", "Progetto")).unwrap();
    let columns = TaskStatus::ALL;
    let mut model: Vec<Vec<String>> = vec![Vec::new(); columns.len()];
    let mut ids = Vec::new();
    for i in 0..20 {
        let id = format!("t{i:02}");
        let col = i % columns.len();
        db.insert_task(&id, &new_task("p", &id, Some(columns[col])), NOW)
            .unwrap();
        model[col].push(id.clone());
        ids.push(id);
    }
    let positions = || -> HashMap<String, f64> {
        let board = db.board("p").unwrap();
        board
            .into_iter()
            .map(|c| (c.task.id, c.task.position))
            .collect()
    };

    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut renumbered = 0;
    for step in 0..500 {
        let before_move = positions();
        // Half of the moves put the last task of a column right after its first one: that
        // halves the same gap every time, so renumbering must kick in.
        let (id, col, before) = if rng.below(2) == 0
            && let Some(col) = model.iter().position(|c| c.len() >= 3)
        {
            let column = &model[col];
            (
                column[column.len() - 1].clone(),
                col,
                Some(column[1].clone()),
            )
        } else {
            let id = ids[rng.below(ids.len())].clone();
            let col = rng.below(columns.len());
            let others: Vec<&String> = model[col].iter().filter(|t| **t != id).collect();
            let before = others
                .get(rng.below(others.len() + 1))
                .map(|t| (*t).clone());
            (id, col, before)
        };
        db.move_task(&id, columns[col], before.as_deref(), NOW + step)
            .unwrap();

        for column in &mut model {
            column.retain(|t| *t != id);
        }
        let at = before.map_or(model[col].len(), |b| {
            model[col].iter().position(|t| *t == b).unwrap()
        });
        model[col].insert(at, id.clone());

        let board = db.board("p").unwrap();
        assert!(
            board
                .windows(2)
                .all(|w| (w[0].task.status as u8) <= (w[1].task.status as u8)),
            "step {step}: board not grouped by column"
        );
        for (k, status) in columns.iter().enumerate() {
            let column: Vec<_> = board.iter().filter(|c| c.task.status == *status).collect();
            let order: Vec<&str> = column.iter().map(|c| c.task.id.as_str()).collect();
            assert_eq!(order, model[k], "step {step}: order of {status}");
            assert!(
                column
                    .windows(2)
                    .all(|w| w[0].task.position < w[1].task.position),
                "step {step}: positions of {status} not strictly increasing"
            );
        }

        let after = positions();
        if after.iter().any(|(t, p)| *t != id && before_move[t] != *p) {
            renumbered += 1;
            let got: Vec<f64> = model[col].iter().map(|t| after[t]).collect();
            let want: Vec<f64> = (1..=got.len()).map(|k| k as f64 * POSITION_GAP).collect();
            assert_eq!(got, want, "step {step}: renumbered column");
        }
    }
    assert!(renumbered > 0, "no renumbering in 500 moves");
}

// ---- board, attempts, processes ---------------------------------------------------------------

#[test]
fn board_joins_the_active_else_latest_attempt() {
    let db = Db::open_in_memory().unwrap();
    db.insert_project(&project("p", "Progetto")).unwrap();
    for id in ["fresh", "running", "merged", "active", "restarted"] {
        db.insert_task(id, &new_task("p", id, None), NOW).unwrap();
    }
    let done = |at| finish(ProcessStatus::Completed, at);

    // Two turns, the second one still running.
    db.begin_attempt(
        &attempt("a-run", "running", NOW),
        &process("r-run1", "a-run", 1, "i"),
        NOW,
    )
    .unwrap();
    db.finish_process("r-run1", &done(NOW + 1)).unwrap();
    db.begin_turn(&process("r-run2", "a-run", 2, "i"), NOW + 2)
        .unwrap();

    // A discarded attempt, then a newer merged one: the newer wins.
    db.begin_attempt(
        &attempt("a-old", "merged", NOW),
        &process("r-old", "a-old", 1, "i"),
        NOW,
    )
    .unwrap();
    db.finish_process("r-old", &done(NOW + 1)).unwrap();
    db.finish_discard("a-old", NOW + 2).unwrap();
    db.begin_attempt(
        &attempt("a-new", "merged", NOW + 10),
        &process("r-new", "a-new", 1, "i"),
        NOW + 10,
    )
    .unwrap();
    db.finish_process("r-new", &finish(ProcessStatus::Failed, NOW + 11))
        .unwrap();
    db.finish_merge("a-new", &"m".repeat(40), NOW + 12).unwrap();
    db.set_worktree_state("a-new", WorktreeState::Removed, NOW + 13)
        .unwrap();

    // The active attempt wins over a more recent closed one.
    db.begin_attempt(
        &attempt("a-act", "active", NOW),
        &process("r-act", "a-act", 1, "i"),
        NOW,
    )
    .unwrap();
    db.finish_process("r-act", &done(NOW + 1)).unwrap();
    let mut closed = attempt("a-late", "active", NOW + 30);
    closed.state = AttemptState::Discarded;
    closed.closed_at = Some(NOW + 31);
    db.begin_attempt(&closed, &process("r-late", "a-late", 1, "i"), NOW + 30)
        .unwrap();
    db.finish_process("r-late", &done(NOW + 31)).unwrap();

    // Interrupted by an app restart.
    db.begin_attempt(
        &attempt("a-rst", "restarted", NOW),
        &process("r-rst", "a-rst", 1, "old"),
        NOW,
    )
    .unwrap();
    db.mark_orphans("i", NOW + 40).unwrap();

    let board = db.board("p").unwrap();
    let card = |id: &str| board.iter().find(|c| c.task.id == id).unwrap();
    let fresh = card("fresh");
    assert_eq!(
        (
            fresh.attempt_id.as_deref(),
            fresh.attempt_state,
            fresh.running
        ),
        (None, None, false)
    );
    let running = card("running");
    assert_eq!(running.attempt_state, Some(AttemptState::Active));
    assert_eq!(running.last_status, Some(ProcessStatus::Running));
    assert!(running.running);
    let merged = card("merged");
    assert_eq!(merged.attempt_id.as_deref(), Some("a-new"));
    assert_eq!(merged.attempt_state, Some(AttemptState::Merged));
    assert_eq!(merged.last_status, Some(ProcessStatus::Failed));
    assert_eq!(merged.worktree_state, Some(WorktreeState::Removed));
    assert_eq!(merged.task.status, TaskStatus::Done);
    assert_eq!(card("active").attempt_id.as_deref(), Some("a-act"));
    let restarted = card("restarted");
    assert_eq!(restarted.attempt_state, Some(AttemptState::Active));
    assert_eq!(restarted.last_stop_reason, Some(StopReason::AppRestart));
    assert!(!restarted.running);
    assert!(board.iter().all(|c| c.pending_approvals == 0));

    for c in &board {
        assert_eq!(&db.task_card(&c.task.id).unwrap(), c);
    }
    assert_eq!(err(db.task_card("nope")).code, ErrorCode::NotFound);
    assert!(db.board("nope").unwrap().is_empty());
    insta::assert_json_snapshot!("board", board);
}

#[test]
fn projects_are_stored_ordered_and_updated() {
    let db = Db::open_in_memory().unwrap();
    for (id, name) in [("b", "beta"), ("a", "Alpha"), ("g", "gamma")] {
        db.insert_project(&project(id, name)).unwrap();
    }
    let names: Vec<String> = db.projects().unwrap().into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["Alpha", "beta", "gamma"]);
    assert_eq!(
        err(db.insert_project(&project("a", "Altro"))).code,
        ErrorCode::Conflict
    );
    let mut same_repo = project("z", "Zeta");
    same_repo.repo_path = "/repos/a".into();
    assert_eq!(err(db.insert_project(&same_repo)).code, ErrorCode::Conflict);

    let req = UpdateProjectReq {
        id: "a".into(),
        name: "Alfa".into(),
        default_target_branch: "develop".into(),
        default_permission_mode: PermissionMode::Default,
        default_model: Some("opus".into()),
    };
    let p = db.update_project(&req, NOW + 1).unwrap();
    assert_eq!(p, db.project("a").unwrap());
    assert_eq!(
        (
            p.name.as_str(),
            p.default_target_branch.as_str(),
            p.default_permission_mode
        ),
        ("Alfa", "develop", PermissionMode::Default)
    );
    assert_eq!(
        (p.default_model.as_deref(), p.updated_at),
        (Some("opus"), NOW + 1)
    );
    let missing = UpdateProjectReq {
        id: "nope".into(),
        ..req
    };
    assert_eq!(
        err(db.update_project(&missing, NOW)).code,
        ErrorCode::NotFound
    );

    let stored = db.project("a").unwrap().security();
    let p = db
        .set_project_security(
            "a",
            &stored,
            ConfigPolicy::Trusted,
            true,
            Some("ab12"),
            NOW + 2,
        )
        .unwrap();
    assert_eq!(
        (
            p.config_policy,
            p.allow_bypass,
            p.trusted_fingerprint.as_deref()
        ),
        (ConfigPolicy::Trusted, true, Some("ab12"))
    );
    assert!(p.to_project(true).trusted);
    // Compare-and-set: the state read before that change no longer applies.
    let e = err(db.set_project_security(
        "a",
        &stored,
        ConfigPolicy::Trusted,
        true,
        Some("cd34"),
        NOW + 3,
    ));
    assert_eq!(e.code, ErrorCode::Conflict);
    // Autonomo as the default mode, then the bypass revoked: reset in the same statement, and
    // Autonomo cannot be set as the default any more.
    let bypass_default = UpdateProjectReq {
        id: "a".into(),
        name: "Alfa".into(),
        default_target_branch: "develop".into(),
        default_permission_mode: PermissionMode::BypassPermissions,
        default_model: None,
    };
    db.update_project(&bypass_default, NOW + 3).unwrap();
    let p = db
        .set_project_security(
            "a",
            &db.project("a").unwrap().security(),
            ConfigPolicy::Isolated,
            false,
            None,
            NOW + 4,
        )
        .unwrap();
    assert_eq!(
        (
            p.trusted_fingerprint,
            p.default_permission_mode,
            p.updated_at
        ),
        (None, PermissionMode::AcceptEdits, NOW + 4)
    );
    assert_eq!(
        err(db.update_project(&bypass_default, NOW + 5)).code,
        ErrorCode::Invalid
    );
    let e = err(db.set_project_security("nope", &stored, ConfigPolicy::Isolated, false, None, NOW));
    assert_eq!(e.code, ErrorCode::NotFound);
    assert_eq!(err(db.project("nope")).code, ErrorCode::NotFound);
}

#[test]
fn attempt_and_turn_lifecycle_drives_the_task_status() {
    let db = seeded();
    db.insert_task(
        "busy",
        &new_task("p", "Già in corso", Some(TaskStatus::InProgress)),
        NOW,
    )
    .unwrap();
    let mut a1 = attempt("a1", "t", NOW);
    a1.effort = Some(Effort::XHigh);
    a1.model = Some("opus".into());
    db.begin_attempt(&a1, &process("r1", "a1", 1, "i"), NOW + 1)
        .unwrap();

    let t = db.task("t").unwrap();
    assert_eq!(
        (t.status, t.position, t.updated_at),
        (TaskStatus::InProgress, 2048.0, NOW + 1)
    );
    assert_eq!(db.attempt("a1").unwrap(), a1);
    assert_eq!(db.active_attempt("t").unwrap(), Some(a1.clone()));
    let ctx = db.attempt_ctx("a1").unwrap();
    assert_eq!(
        (ctx.attempt, ctx.task, ctx.project.id.as_str()),
        (a1, t, "p")
    );
    assert_eq!(err(db.attempt_ctx("nope")).code, ErrorCode::NotFound);
    assert_eq!(db.next_seq("a1").unwrap(), 2);
    assert_eq!(db.next_seq("nope").unwrap(), 1);

    db.set_process_pid("r1", 4242, Some("2.1.283")).unwrap();
    let r1 = db.process("r1").unwrap();
    assert_eq!(
        (r1.pid, r1.cli_version.as_deref()),
        (Some(4242), Some("2.1.283"))
    );
    assert_eq!(
        err(db.set_process_pid("nope", 1, None)).code,
        ErrorCode::NotFound
    );
    db.set_session("a1", "session-new", true, NOW + 2).unwrap();
    let a = db.attempt("a1").unwrap();
    assert_eq!(
        (a.session_id.as_str(), a.session_started),
        ("session-new", true)
    );
    assert_eq!(
        err(db.set_session("nope", "s", true, NOW)).code,
        ErrorCode::NotFound
    );

    let rules = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let got = db
        .add_allow_rules("a1", &rules(&["Bash(ls:*)", "Read(src/**)"]), NOW)
        .unwrap();
    assert_eq!(got, rules(&["Bash(ls:*)", "Read(src/**)"]));
    let got = db
        .add_allow_rules(
            "a1",
            &rules(&["Read(src/**)", "Edit(a.rs)", "Edit(a.rs)"]),
            NOW,
        )
        .unwrap();
    assert_eq!(got, rules(&["Bash(ls:*)", "Read(src/**)", "Edit(a.rs)"]));
    assert_eq!(db.attempt("a1").unwrap().allow_rules, got);
    assert_eq!(
        err(db.add_allow_rules("nope", &[], NOW)).code,
        ErrorCode::NotFound
    );

    // End of the turn: inprogress → inreview (end of the column), final columns stored.
    let mut fin = finish(ProcessStatus::Completed, NOW + 10);
    fin.duration_ms = Some(u64::from(u32::MAX) + 1);
    db.finish_process("r1", &fin).unwrap();
    let r1 = db.process("r1").unwrap();
    assert_eq!(
        (
            r1.status,
            r1.duration_ms,
            r1.num_turns,
            r1.cost_usd_estimate,
            r1.finished_at
        ),
        (
            ProcessStatus::Completed,
            fin.duration_ms,
            Some(3),
            Some(0.25),
            Some(NOW + 10)
        )
    );
    assert_eq!(r1.info().head_after, fin.head_after);
    assert_eq!(db.task("t").unwrap().status, TaskStatus::InReview);
    assert_eq!(
        db.task("busy").unwrap().status,
        TaskStatus::InProgress,
        "other tasks untouched"
    );
    assert_eq!(
        err(db.finish_process("nope", &fin)).code,
        ErrorCode::NotFound
    );

    // Follow-up: back to inprogress; a turn of a missing attempt is NotFound.
    let mut r2 = process("r2", "a1", 2, "i");
    r2.permission_mode = PermissionMode::Default;
    db.begin_turn(&r2, NOW + 20).unwrap();
    assert_eq!(db.task("t").unwrap().status, TaskStatus::InProgress);
    assert_eq!(db.running_processes().unwrap(), [r2.clone()]);
    assert_eq!(
        err(db.begin_turn(&process("x", "nope", 1, "i"), NOW)).code,
        ErrorCode::NotFound
    );
    let mut killed = finish(ProcessStatus::Killed, NOW + 21);
    killed.stop_reason = Some(StopReason::UserStop);
    db.finish_process("r2", &killed).unwrap();
    let turns = db.attempt_processes("a1").unwrap();
    assert_eq!(turns.iter().map(|p| p.seq).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(turns[1].stop_reason, Some(StopReason::UserStop));
    assert_eq!(turns[1].permission_mode, PermissionMode::Default);

    // Merge: attempt closed, task done; the worktree is listed until removed.
    db.finish_merge("a1", "abc123", NOW + 30).unwrap();
    let a = db.attempt("a1").unwrap();
    assert_eq!(
        (a.state, a.merge_commit.as_deref(), a.closed_at),
        (AttemptState::Merged, Some("abc123"), Some(NOW + 30))
    );
    assert_eq!(db.task("t").unwrap().status, TaskStatus::Done);
    assert_eq!(db.active_attempt("t").unwrap(), None);
    assert_eq!(
        err(db.finish_merge("nope", "x", NOW)).code,
        ErrorCode::NotFound
    );
    let with_worktree = |p| -> Vec<String> {
        db.attempts_with_worktree(p)
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect()
    };
    assert_eq!(with_worktree(Some("p")), ["a1"]);
    db.set_worktree_state("a1", WorktreeState::Removed, NOW + 31)
        .unwrap();
    assert!(with_worktree(None).is_empty());
    assert_eq!(
        err(db.set_worktree_state("nope", WorktreeState::Missing, NOW)).code,
        ErrorCode::NotFound
    );

    // A second attempt, discarded: task back to todo; its worktree goes missing.
    db.begin_attempt(
        &attempt("a2", "t", NOW + 40),
        &process("r3", "a2", 1, "i"),
        NOW + 40,
    )
    .unwrap();
    db.finish_process("r3", &finish(ProcessStatus::Failed, NOW + 41))
        .unwrap();
    db.set_worktree_state("a2", WorktreeState::Missing, NOW + 42)
        .unwrap();
    assert_eq!(with_worktree(None), ["a2"]);
    assert!(with_worktree(Some("other")).is_empty());
    db.finish_discard("a2", NOW + 43).unwrap();
    assert_eq!(db.task("t").unwrap().status, TaskStatus::Todo);
    let ids: Vec<String> = db
        .task_attempts("t")
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert_eq!(ids, ["a1", "a2"]);

    // Discarding while the task sits in done leaves it there.
    db.begin_attempt(
        &attempt("a3", "t", NOW + 50),
        &process("r4", "a3", 1, "i"),
        NOW + 50,
    )
    .unwrap();
    db.finish_process("r4", &finish(ProcessStatus::Completed, NOW + 51))
        .unwrap();
    db.set_task_status("t", TaskStatus::Done, NOW + 52).unwrap();
    db.finish_discard("a3", NOW + 53).unwrap();
    assert_eq!(db.task("t").unwrap().status, TaskStatus::Done);
    assert_eq!(
        err(db.finish_discard("nope", NOW)).code,
        ErrorCode::NotFound
    );

    let update = UpdateTaskReq {
        id: "t".into(),
        title: "Nuovo titolo".into(),
        description: "Dettagli".into(),
    };
    let t = db.update_task(&update, NOW + 60).unwrap();
    assert_eq!(
        (t.title.as_str(), t.description.as_str(), t.updated_at),
        ("Nuovo titolo", "Dettagli", NOW + 60)
    );
    let missing = UpdateTaskReq {
        id: "nope".into(),
        ..update
    };
    assert_eq!(err(db.update_task(&missing, NOW)).code, ErrorCode::NotFound);
}

#[test]
fn mark_orphans_fails_turns_of_other_app_instances() {
    let db = Db::open_in_memory().unwrap();
    db.insert_project(&project("p", "Progetto")).unwrap();
    for id in ["t1", "t2", "t3"] {
        db.insert_task(id, &new_task("p", id, None), NOW).unwrap();
    }
    db.insert_task("t4", &new_task("p", "t4", Some(TaskStatus::InReview)), NOW)
        .unwrap();
    db.begin_attempt(
        &attempt("a1", "t1", NOW),
        &process("r1", "a1", 1, "old"),
        NOW,
    )
    .unwrap();
    db.set_process_pid("r1", 4242, None).unwrap();
    db.begin_attempt(
        &attempt("a2", "t2", NOW),
        &process("r2", "a2", 1, "now"),
        NOW,
    )
    .unwrap();
    db.set_task_status("t3", TaskStatus::InProgress, NOW)
        .unwrap();

    let marked = db.mark_orphans("now", NOW + 50).unwrap();
    assert_eq!(marked.len(), 1);
    let r1 = &marked[0];
    assert_eq!(
        (
            r1.id.as_str(),
            r1.status,
            r1.stop_reason,
            r1.pid,
            r1.finished_at
        ),
        (
            "r1",
            ProcessStatus::Failed,
            Some(StopReason::AppRestart),
            Some(4242),
            Some(NOW + 50)
        )
    );
    assert_eq!(&db.process("r1").unwrap(), r1);
    assert_eq!(db.process("r2").unwrap().status, ProcessStatus::Running);
    let status = |id| db.task(id).unwrap().status;
    assert_eq!(
        [status("t1"), status("t2"), status("t3"), status("t4")],
        [
            TaskStatus::InReview,
            TaskStatus::InProgress,
            TaskStatus::InReview,
            TaskStatus::InReview
        ]
    );
    assert_eq!(db.task("t1").unwrap().updated_at, NOW + 50);
    assert!(
        db.mark_orphans("now", NOW + 60).unwrap().is_empty(),
        "idempotent"
    );
}

#[test]
fn settings_default_and_round_trip() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(db.settings().unwrap(), Settings::default());
    let custom = Settings {
        claude_path_override: Some("/opt/claude/bin/claude".into()),
        default_model: Some("opus".into()),
        max_running: 4,
        allow_env_api_key: true,
        worktree_root: "/Volumes/dev/worktrees".into(),
        editor_app: "Zed".into(),
        remove_worktree_after_merge: false,
    };
    db.save_settings(&custom).unwrap();
    assert_eq!(db.settings().unwrap(), custom);
    db.save_settings(&Settings::default()).unwrap();
    assert_eq!(db.settings().unwrap(), Settings::default());
}

// ---- entries ----------------------------------------------------------------------------------

#[test]
fn upsert_is_idempotent_by_rev() {
    let db = with_attempt();
    assert_eq!(db.next_entry_idx("a1").unwrap(), 0);
    let first = say(0, 1, "uno");
    let second = say(1, 1, "due");
    db.upsert_entries("a1", &[first.clone(), second.clone()])
        .unwrap();
    db.upsert_entries("a1", &[first.clone(), second.clone()])
        .unwrap();
    db.upsert_entries("a1", &[say(1, 0, "più vecchio"), say(1, 1, "stesso rev")])
        .unwrap();
    assert_eq!(
        db.entries_tail("a1", 10).unwrap().entries,
        [first, second.clone()]
    );

    let newer = say(0, 2, "uno, rivisto");
    db.upsert_entries("a1", &[newer.clone(), say(0, 1, "in ritardo")])
        .unwrap();
    let page = db.entries_tail("a1", 10).unwrap();
    assert_eq!(
        page,
        EntryPage {
            entries: vec![newer, second],
            has_more: false
        }
    );
    assert_eq!(db.next_entry_idx("a1").unwrap(), 2);
}

#[test]
fn entries_paginate_backwards() {
    let db = with_attempt();
    let all: Vec<Entry> = (0..450).map(|i| say(i, 1, &format!("riga {i}"))).collect();
    db.upsert_entries("a1", &all).unwrap();

    let tail = db.entries_tail("a1", 200).unwrap();
    assert_eq!((idxs(&tail), tail.has_more), ((250..450).collect(), true));
    assert_eq!(tail.entries[0], all[250]);
    let page = db.entries_before("a1", 250, 200).unwrap();
    assert_eq!((idxs(&page), page.has_more), ((50..250).collect(), true));
    let page = db.entries_before("a1", 50, 200).unwrap();
    assert_eq!((idxs(&page), page.has_more), ((0..50).collect(), false));
    let page = db.entries_before("a1", 200, 200).unwrap();
    assert_eq!(
        (idxs(&page), page.has_more),
        ((0..200).collect(), false),
        "exact fit"
    );

    let clamped = db.entries_tail("a1", 1000).unwrap();
    assert_eq!(clamped.entries.len(), MAX_ENTRY_PAGE as usize);
    let clamped = db.entries_before("a1", 450, 1000).unwrap();
    assert_eq!(idxs(&clamped), (250..450).collect::<Vec<_>>());
    let none = db.entries_tail("a1", 0).unwrap();
    assert!(none.entries.is_empty() && none.has_more);
    let empty = EntryPage {
        entries: Vec::new(),
        has_more: false,
    };
    assert_eq!(db.entries_before("a1", 0, 10).unwrap(), empty);
    assert_eq!(db.entries_tail("nope", 10).unwrap(), empty);
    assert_eq!(db.next_entry_idx("a1").unwrap(), 450);
}

#[test]
fn cancel_open_tools_closes_only_the_open_calls_of_the_process() {
    let db = with_attempt();
    let awaiting = ToolStatus::AwaitingApproval {
        approval_id: "ap1".into(),
        can_remember: true,
        reason: None,
    };
    db.upsert_entries(
        "a1",
        &[
            tool(0, "r1", ToolStatus::Running),
            tool(1, "r1", awaiting),
            tool(2, "r1", ToolStatus::Succeeded),
            tool(3, "r0", ToolStatus::Running),
            say(4, 1, "testo"),
        ],
    )
    .unwrap();

    let cancelled = db.cancel_open_tools("a1", "r1").unwrap();
    let want = [0, 1].map(|i| Entry {
        rev: 2,
        ..tool(i, "r1", ToolStatus::Cancelled)
    });
    assert_eq!(cancelled, want);
    let stored = db.entries_tail("a1", 10).unwrap().entries;
    assert_eq!(stored[..2], want);
    assert_eq!(stored[2], tool(2, "r1", ToolStatus::Succeeded));
    assert_eq!(stored[3], tool(3, "r0", ToolStatus::Running));
    assert!(
        db.cancel_open_tools("a1", "r1").unwrap().is_empty(),
        "idempotent"
    );
}
