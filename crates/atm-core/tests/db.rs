//! M2-DB acceptance (spec §5, §11.2): migrations, constraints, positions, board join,
//! attempts and processes, orphan recovery, transcript entries.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use atm_core::attachments;
use atm_core::db::{
    self, AttachmentRow, AttemptRow, Db, MAX_ENTRY_PAGE, MIGRATIONS, POSITION_GAP, ProcessFinish,
    ProcessRow, ProjectRow,
};
use atm_types::{
    AppError, AttemptState, ConfigPolicy, CreateTaskReq, Effort, Entry, EntryBody, EntryPage,
    ErrorCode, MAX_ATTACHMENTS_PER_TASK, MAX_PLAN_TITLE, MAX_PROJECT_DESCRIPTION, PermissionMode,
    PlanState, ProcessStatus, Settings, StopReason, TaskKind, TaskStatus, ToolStatus,
    UpdateProjectReq, UpdateTaskReq, VerifyState, WorktreeState,
};
use rusqlite::{Connection, ffi};

const NOW: i64 = 1_700_000_000_000;

// ---- fixtures ---------------------------------------------------------------------------------

fn project(id: &str, name: &str) -> ProjectRow {
    ProjectRow {
        id: id.into(),
        name: name.into(),
        description: String::new(),
        repo_path: format!("/repos/{id}"),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::AcceptEdits,
        default_model: None,
        config_policy: ConfigPolicy::Isolated,
        trusted_fingerprint: None,
        allow_bypass: false,
        created_at: NOW,
        updated_at: NOW,
        autopilot: false,
        autopilot_merge: false,
        verify_command: None,
        verify_timeout_secs: 600,
        autopilot_max_fixes: 2,
    }
}

fn new_task(project_id: &str, title: &str, status: Option<TaskStatus>) -> CreateTaskReq {
    CreateTaskReq {
        project_id: project_id.into(),
        title: title.into(),
        description: String::new(),
        status,
        parent_id: None,
        auto: false,
        after_id: None,
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
        subagent_model: None,
        max_subagents: None,
        subagents_used: 0,
        started_by_attempt: None,
        allow_rules: Vec::new(),
        merge_commit: None,
        created_at,
        updated_at: created_at,
        closed_at: None,
        verify_state: None,
        verify_head: None,
        verify_fixes: 0,
        verify_summary: None,
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

fn attachment(id: &str, task_id: &str, created_at: i64) -> AttachmentRow {
    AttachmentRow {
        id: id.into(),
        task_id: task_id.into(),
        name: format!("{id}.txt"),
        size: 10,
        created_at,
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
            "task_attachments",
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

/// A database of the first schema with rows in every table migrates to the second: the rows
/// are kept, the new columns take their defaults, the new table is there and usable.
#[test]
fn migrates_a_v1_database_with_rows_to_v2() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atm.sqlite3");
    {
        let mut c = Connection::open(&path).unwrap();
        c.execute_batch(db::PRAGMAS).unwrap();
        let tx = c.transaction().unwrap();
        tx.execute_batch(MIGRATIONS[0]).unwrap();
        tx.pragma_update(None, "user_version", 1).unwrap();
        tx.commit().unwrap();
        c.execute_batch(
            "INSERT INTO projects (id, name, repo_path, default_target_branch, default_model,
                created_at, updated_at)
                VALUES ('p', 'Vecchio', '/r', 'main', 'opus', 1, 2);
             INSERT INTO tasks (id, project_id, title, position, created_at, updated_at)
                VALUES ('t', 'p', 'T', 1024, 0, 0);",
        )
        .unwrap();
        raw_attempt(&c, "a", "active").unwrap();
        raw_process(&c, "r", "a", 1, "running").unwrap();
    }

    let db = Db::open(&path).unwrap();
    let version: i64 = Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, MIGRATIONS.len() as i64);
    let p = db.project("p").unwrap();
    assert_eq!(
        (
            p.name.as_str(),
            p.default_model.as_deref(),
            p.description.as_str()
        ),
        ("Vecchio", Some("opus"), "")
    );
    let a = db.attempt("a").unwrap();
    assert_eq!((a.branch.as_str(), a.task_id.as_str()), ("atm/a", "t"));
    assert_eq!(
        (a.subagent_model, a.max_subagents, a.subagents_used),
        (None, None, 0)
    );
    assert_eq!(db.process("r").unwrap().status, ProcessStatus::Running);
    assert!(db.task_attachments("t").unwrap().is_empty());
    db.insert_attachments(&[attachment("x", "t", NOW)]).unwrap();
    assert_eq!(db.attachment_count("t").unwrap(), 1);
    assert_eq!(db.count_subagent("a", NOW).unwrap(), Some(1));
    let req = UpdateProjectReq {
        id: "p".into(),
        name: "Vecchio".into(),
        default_target_branch: "main".into(),
        default_permission_mode: PermissionMode::AcceptEdits,
        default_model: None,
        description: "Ora descritto".into(),
        autopilot: false,
        autopilot_merge: false,
        verify_command: None,
        verify_timeout_secs: 600,
        autopilot_max_fixes: 2,
    };
    assert_eq!(
        db.update_project(&req, NOW).unwrap().description,
        "Ora descritto"
    );
}

/// A database of the second schema with rows migrates to the third: the rows are kept, every
/// task is top-level, every attempt was started by the user, and the new columns are usable.
#[test]
fn migrates_a_v2_database_with_rows_to_v3() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atm.sqlite3");
    {
        let mut c = Connection::open(&path).unwrap();
        c.execute_batch(db::PRAGMAS).unwrap();
        let tx = c.transaction().unwrap();
        tx.execute_batch(MIGRATIONS[0]).unwrap();
        tx.execute_batch(MIGRATIONS[1]).unwrap();
        tx.pragma_update(None, "user_version", 2).unwrap();
        tx.commit().unwrap();
        c.execute_batch(
            "INSERT INTO projects (id, name, description, repo_path, default_target_branch,
                created_at, updated_at)
                VALUES ('p', 'Vecchio', 'Descritto', '/r', 'main', 1, 2);
             INSERT INTO tasks (id, project_id, title, status, position, created_at, updated_at)
                VALUES ('t', 'p', 'T', 'done', 1024, 0, 0);
             INSERT INTO task_attachments (id, task_id, name, size, created_at)
                VALUES ('x', 't', 'log.txt', 3, 0);",
        )
        .unwrap();
        raw_attempt(&c, "a", "active").unwrap();
        c.execute(
            "UPDATE attempts SET max_subagents = 2, subagents_used = 1 WHERE id = 'a'",
            [],
        )
        .unwrap();
        raw_process(&c, "r", "a", 1, "running").unwrap();
    }

    let db = Db::open(&path).unwrap();
    let version: i64 = Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, MIGRATIONS.len() as i64);
    assert_eq!(db.project("p").unwrap().description, "Descritto");
    let t = db.task("t").unwrap();
    assert_eq!((t.title.as_str(), t.parent_id), ("T", None));
    let a = db.attempt("a").unwrap();
    assert_eq!(
        (a.max_subagents, a.subagents_used, a.started_by_attempt),
        (Some(2), 1, None)
    );
    assert_eq!(db.process("r").unwrap().status, ProcessStatus::Running);
    assert_eq!(db.attachment_count("t").unwrap(), 1);
    let card = db.task_card("t").unwrap();
    assert_eq!((card.subtasks_done, card.subtasks_total), (0, 0));

    let child = CreateTaskReq {
        parent_id: Some("t".into()),
        ..new_task("p", "Figlio", Some(TaskStatus::Done))
    };
    assert_eq!(
        db.insert_task("c", &child, NOW)
            .unwrap()
            .parent_id
            .as_deref(),
        Some("t")
    );
    let card = db.task_card("t").unwrap();
    assert_eq!((card.subtasks_done, card.subtasks_total), (1, 1));
    assert_eq!(db.task_children("t").unwrap(), ["c"]);
}

/// Migration 0005 on a v4 DB with rows: every task stays a visible task, no plan, no launch,
/// and the one-active-plan index holds.
#[test]
fn migrates_a_v4_database_with_rows_to_v5() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atm.sqlite3");
    {
        let mut c = Connection::open(&path).unwrap();
        c.execute_batch(db::PRAGMAS).unwrap();
        let tx = c.transaction().unwrap();
        for sql in &MIGRATIONS[..4] {
            tx.execute_batch(sql).unwrap();
        }
        tx.pragma_update(None, "user_version", 4).unwrap();
        tx.commit().unwrap();
        c.execute_batch(
            "INSERT INTO projects (id, name, repo_path, default_target_branch, created_at,
                updated_at)
                VALUES ('p', 'Vecchio', '/r', 'main', 1, 2);
             INSERT INTO tasks (id, project_id, title, status, position, created_at, updated_at,
                auto)
                VALUES ('t', 'p', 'T', 'todo', 1024, 0, 0, 1);",
        )
        .unwrap();
    }

    let db = Db::open(&path).unwrap();
    let t = db.task("t").unwrap();
    assert_eq!((t.kind, t.auto), (TaskKind::Task, true));
    assert_eq!(db.latest_plan("p").unwrap(), None);
    assert!(db.projects_with_launch().unwrap().is_empty());
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "INSERT INTO tasks (id, project_id, title, status, position, created_at, updated_at,
            kind, plan_state) VALUES ('x', 'p', 'X', 'todo', 1, 0, 0, 'plan', 'awaiting')",
    )
    .unwrap();
    let e = c
        .execute_batch(
            "INSERT INTO tasks (id, project_id, title, status, position, created_at, updated_at,
                kind, plan_state) VALUES ('y', 'p', 'Y', 'todo', 1, 0, 0, 'plan', 'running')",
        )
        .unwrap_err();
    assert!(e.to_string().contains("UNIQUE"), "{e}");
    let e = c
        .execute_batch("UPDATE tasks SET kind = 'other' WHERE id = 't'")
        .unwrap_err();
    assert!(e.to_string().contains("CHECK"), "{e}");
}

/// A database of the third schema with rows migrates to the fourth: the rows are kept with the
/// autopilot off and the documented defaults, no attempt was verified, and the new columns'
/// CHECKs and `after_id`'s `ON DELETE SET NULL` hold.
#[test]
fn migrates_a_v3_database_with_rows_to_v4() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("atm.sqlite3");
    {
        let mut c = Connection::open(&path).unwrap();
        c.execute_batch(db::PRAGMAS).unwrap();
        let tx = c.transaction().unwrap();
        for sql in &MIGRATIONS[..3] {
            tx.execute_batch(sql).unwrap();
        }
        tx.pragma_update(None, "user_version", 3).unwrap();
        tx.commit().unwrap();
        c.execute_batch(
            "INSERT INTO projects (id, name, repo_path, default_target_branch, created_at,
                updated_at)
                VALUES ('p', 'Vecchio', '/r', 'main', 1, 2);
             INSERT INTO tasks (id, project_id, title, status, position, created_at, updated_at)
                VALUES ('t', 'p', 'T', 'inreview', 1024, 0, 0);
             INSERT INTO tasks (id, project_id, title, status, position, created_at, updated_at,
                parent_id)
                VALUES ('s', 'p', 'S', 'todo', 1024, 0, 0, 't');",
        )
        .unwrap();
        raw_attempt(&c, "a", "active").unwrap();
    }

    let db = Db::open(&path).unwrap();
    let version: i64 = Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, MIGRATIONS.len() as i64);
    let p = db.project("p").unwrap();
    assert_eq!(
        (
            p.autopilot,
            p.autopilot_merge,
            p.verify_command,
            p.verify_timeout_secs,
            p.autopilot_max_fixes
        ),
        (false, false, None, 600, 2)
    );
    let s = db.task("s").unwrap();
    assert_eq!(
        (s.auto, s.after_id, s.parent_id.as_deref()),
        (false, None, Some("t"))
    );
    let a = db.attempt("a").unwrap();
    assert_eq!(
        (
            a.verify_state,
            a.verify_head,
            a.verify_fixes,
            a.verify_summary
        ),
        (None, None, 0, None)
    );
    let card = db.task_card("t").unwrap();
    assert_eq!(
        (card.verify_state, card.verify_fixes, card.verifying),
        (None, 0, false)
    );

    let c = Connection::open(&path).unwrap();
    c.execute_batch(db::PRAGMAS).unwrap();
    for bad in [
        "UPDATE projects SET verify_timeout_secs = 9",
        "UPDATE projects SET verify_timeout_secs = 3601",
        "UPDATE projects SET autopilot_max_fixes = 6",
        "UPDATE projects SET verify_command = ''",
        "UPDATE projects SET autopilot = 2",
        "UPDATE tasks SET auto = 2",
        "UPDATE attempts SET verify_state = 'done'",
        "UPDATE attempts SET verify_fixes = -1",
    ] {
        let e = c.execute(bad, []).unwrap_err();
        assert_eq!(
            e.sqlite_error().unwrap().extended_code,
            ffi::SQLITE_CONSTRAINT_CHECK,
            "{bad}"
        );
    }
    c.execute("UPDATE tasks SET after_id = 't' WHERE id = 's'", [])
        .unwrap();
    assert_eq!(db.task("s").unwrap().after_id.as_deref(), Some("t"));
    c.execute("UPDATE tasks SET parent_id = NULL WHERE id = 's'", [])
        .unwrap();
    db.delete_task("t").unwrap();
    assert_eq!(db.task("s").unwrap().after_id, None);
}

/// The scheduler's queue: auto, todo, no active attempt, dependency done (or none), parent not
/// cancelled, this project only, in position order.
#[test]
fn autopilot_candidates_follow_the_rules() {
    use TaskStatus::{Cancelled, Done, InReview, Todo};
    let db = Db::open_in_memory().unwrap();
    db.insert_project(&project("p", "Progetto")).unwrap();
    db.insert_project(&project("q", "Altro")).unwrap();
    let add = |id: &str, project: &str, status, auto, after: Option<&str>, parent: Option<&str>| {
        let req = CreateTaskReq {
            auto,
            after_id: after.map(Into::into),
            parent_id: parent.map(Into::into),
            ..new_task(project, id, Some(status))
        };
        db.insert_task(id, &req, NOW).unwrap();
    };
    add("open", "p", Todo, true, None, None);
    add("manual", "p", Todo, false, None, None);
    add("waits", "p", Todo, true, Some("open"), None);
    add("dep", "p", Done, false, None, None);
    add("ready", "p", Todo, true, Some("dep"), None);
    add("review", "p", InReview, true, None, None);
    add("dead", "p", Cancelled, false, None, None);
    add("orphaned", "p", Todo, true, None, Some("dead"));
    add("child", "p", Todo, true, None, Some("dep"));
    add("busy", "p", Todo, true, None, None);
    add("elsewhere", "q", Todo, true, None, None);
    db.begin_attempt(&attempt("a", "busy", NOW), &process("r", "a", 1, "i"), NOW)
        .unwrap();
    db.set_task_status("busy", Todo, NOW).unwrap();

    let ids = |project| -> Vec<String> {
        db.autopilot_candidates(project, true)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect()
    };
    assert_eq!(ids("p"), ["open", "ready", "child"]);
    assert_eq!(ids("q"), ["elsewhere"]);

    db.set_task_status("open", Done, NOW).unwrap();
    db.set_task_auto("ready", false, NOW).unwrap();
    assert_eq!(ids("p"), ["waits", "child"]);
    assert_eq!(
        err(db.set_task_auto("nope", true, NOW)).code,
        ErrorCode::NotFound
    );
}

/// Round 2026-10-02: a plan is a hidden task (board, sub-task cards, candidates), one active
/// per project; its created tasks are launched (`launch` or `auto`) in one transaction with
/// the state, `launch` is cleared when the task starts; startup fails a plan cut mid-turn.
#[test]
fn plans_are_hidden_and_launch_their_tasks() {
    let db = seeded();
    let plan = db
        .insert_plan("pl", "p", "\n  Rifai il sito\ncon calma", NOW)
        .unwrap();
    assert_eq!(
        (plan.kind, plan.title.as_str(), plan.status),
        (TaskKind::Plan, "Rifai il sito", TaskStatus::Todo)
    );
    assert_eq!(db.task("t").unwrap().kind, TaskKind::Task);
    assert_eq!(db.plan_state("pl").unwrap(), PlanState::Running);
    assert_eq!(db.latest_plan("p").unwrap().as_deref(), Some("pl"));
    assert_eq!(db.latest_plan("nope").unwrap(), None);
    assert_eq!(
        err(db.insert_plan("pl2", "p", "Altro", NOW)).code,
        ErrorCode::Conflict
    );
    assert_eq!(
        err(db.insert_plan("pl3", "p", " \n ", NOW)).code,
        ErrorCode::Invalid
    );
    assert_eq!(err(db.plan_state("t")).code, ErrorCode::NotFound);

    // Hidden: board, candidates (even with auto and launch set by hand).
    db.set_task_auto("pl", true, NOW).unwrap();
    db.set_task_launch("pl", true, NOW).unwrap();
    let board: Vec<String> = db
        .board("p")
        .unwrap()
        .into_iter()
        .map(|c| c.task.id)
        .collect();
    assert_eq!(board, ["t"]);
    assert!(db.autopilot_candidates("p", true).unwrap().is_empty());
    assert!(db.projects_with_launch().unwrap().is_empty());
    db.set_task_auto("pl", false, NOW).unwrap();
    db.set_task_launch("pl", false, NOW).unwrap();

    // Created tasks: `planned_by`, oldest first; one already started is not launched.
    for id in ["c1", "c2", "c3"] {
        db.insert_task(id, &new_task("p", id, None), NOW).unwrap();
        db.set_planned_by(id, "pl", NOW).unwrap();
    }
    db.set_task_status("c3", TaskStatus::InReview, NOW).unwrap();
    // A sub-task of a created task: listed, never handed over (its parent's agent starts it).
    let mut sub = new_task("p", "c1s", None);
    sub.parent_id = Some("c1".into());
    db.insert_task("c1s", &sub, NOW).unwrap();
    db.set_planned_by("c1s", "pl", NOW).unwrap();
    let view = db.plan_view("pl").unwrap();
    assert_eq!(view.prompt, "\n  Rifai il sito\ncon calma");
    assert_eq!((view.state, view.attempt_id), (PlanState::Running, None));
    let created: Vec<&str> = view.created.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(created, ["c1", "c2", "c3", "c1s"]);
    assert_eq!(view.created[3].parent_id.as_deref(), Some("c1"));

    db.begin_attempt(&attempt("pa", "pl", NOW), &process("pr", "pa", 1, "i"), NOW)
        .unwrap();
    assert_eq!(
        db.plan_view("pl").unwrap().attempt_id.as_deref(),
        Some("pa")
    );
    assert!(db.stale_plans().unwrap().is_empty(), "its turn runs");
    assert!(
        db.set_plan_state("pl", &[PlanState::Running], PlanState::Awaiting, NOW)
            .unwrap()
    );
    assert!(
        !db.set_plan_state("pl", &[PlanState::Running], PlanState::Failed, NOW)
            .unwrap()
    );
    assert_eq!(db.launch_plan("pl", false, NOW).unwrap(), ["c1", "c2"]);
    assert_eq!(db.plan_state("pl").unwrap(), PlanState::Started);
    assert_eq!(
        err(db.launch_plan("pl", false, NOW)).code,
        ErrorCode::Conflict
    );
    assert_eq!(db.projects_with_launch().unwrap(), ["p"]);
    let ids = |auto| -> Vec<String> {
        db.autopilot_candidates("p", auto)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect()
    };
    assert_eq!(ids(false), ["c1", "c2"]);
    db.begin_attempt(&attempt("a1", "c1", NOW), &process("r1", "a1", 1, "i"), NOW)
        .unwrap();
    db.set_task_status("c1", TaskStatus::Todo, NOW).unwrap();
    db.finish_discard("a1", NOW).unwrap();
    assert_eq!(ids(false), ["c2"], "launch cleared on start");

    // A second plan once the first ended; with the autopilot its tasks get `auto`.
    db.insert_plan("pm", "p", "Secondo", NOW + 1).unwrap();
    assert_eq!(db.latest_plan("p").unwrap().as_deref(), Some("pm"));
    db.insert_task("c4", &new_task("p", "c4", None), NOW)
        .unwrap();
    db.set_planned_by("c4", "pm", NOW).unwrap();
    assert_eq!(db.launch_plan("pm", true, NOW).unwrap(), ["c4"]);
    let c4 = db.task("c4").unwrap();
    assert!(c4.auto);
    assert_eq!(ids(false), ["c2"]);
    assert!(db.task("c2").unwrap().launch);
    assert!(!db.task("c1s").unwrap().launch);

    // Leaving Da fare cancels the launch: back in todo it no longer starts by itself.
    db.move_task("c2", TaskStatus::Cancelled, None, NOW)
        .unwrap();
    db.move_task("c2", TaskStatus::Todo, None, NOW).unwrap();
    assert!(!db.task("c2").unwrap().launch);
    assert!(ids(false).is_empty());
    assert!(db.projects_with_launch().unwrap().is_empty());

    // Startup: a running plan without a running turn fails.
    db.insert_plan("pn", "p", "Terzo", NOW + 2).unwrap();
    assert_eq!(db.stale_plans().unwrap(), [("pn".to_owned(), None)]);

    // Deleting a plan keeps its tasks; the project takes everything.
    db.delete_task("pm").unwrap();
    assert_eq!(db.task("c4").unwrap().title, "c4");
    db.delete_project("p").unwrap();
    assert_eq!(err(db.task("pl")).code, ErrorCode::NotFound);
}

#[test]
fn plan_title_is_the_first_line_truncated() {
    assert_eq!(db::plan_title("  \n Uno \n due"), "Uno");
    assert_eq!(db::plan_title(""), "");
    let long = db::plan_title(&"x".repeat(200));
    assert_eq!(long.chars().count(), MAX_PLAN_TITLE);
    assert!(long.ends_with('…'));
}

/// `update_task` changes `auto` and `after_id` only when asked; `Some(None)` clears the
/// dependency, an unknown one is a missing reference.
#[test]
fn update_task_sets_auto_and_after_only_when_given() {
    let db = seeded();
    db.insert_task("d", &new_task("p", "Dipendenza", None), NOW)
        .unwrap();
    let update = |auto, after_id| UpdateTaskReq {
        id: "t".into(),
        title: "Task".into(),
        description: String::new(),
        auto,
        after_id,
    };
    let t = db
        .update_task(&update(Some(true), Some(Some("d".into()))), NOW)
        .unwrap();
    assert_eq!((t.auto, t.after_id.as_deref()), (true, Some("d")));
    let t = db.update_task(&update(None, None), NOW).unwrap();
    assert_eq!((t.auto, t.after_id.as_deref()), (true, Some("d")));
    let t = db
        .update_task(&update(Some(false), Some(None)), NOW)
        .unwrap();
    assert_eq!((t.auto, t.after_id), (false, None));
    let e = err(db.update_task(&update(None, Some(Some("nope".into()))), NOW));
    assert_eq!(e.code, ErrorCode::NotFound);
}

/// The verification columns: begin, finish with the summary and the commit, the fix counter,
/// the active attempt's state on its card, and the startup recovery of a cut verification.
#[test]
fn verify_state_lifecycle() {
    let db = with_attempt();
    db.begin_verify("a1", "h1", NOW).unwrap();
    let a = db.attempt("a1").unwrap();
    assert_eq!(
        (a.verify_state, a.verify_head.as_deref()),
        (Some(VerifyState::Running), Some("h1"))
    );
    db.finish_verify("a1", VerifyState::Failed, Some("1 failed"), "h2", NOW)
        .unwrap();
    assert_eq!(db.add_verify_fix("a1", NOW).unwrap(), 1);
    assert_eq!(db.add_verify_fix("a1", NOW).unwrap(), 2);
    let a = db.attempt("a1").unwrap();
    assert_eq!(
        (
            a.verify_state,
            a.verify_head.as_deref(),
            a.verify_summary.as_deref(),
            a.verify_fixes
        ),
        (Some(VerifyState::Failed), Some("h2"), Some("1 failed"), 2)
    );
    let view = a.view(false, 0);
    assert_eq!(
        (view.verify_state, view.verify_fixes),
        (Some(VerifyState::Failed), 2)
    );
    let card = db.task_card("t").unwrap();
    assert_eq!(
        (card.verify_state, card.verify_fixes),
        (Some(VerifyState::Failed), 2)
    );

    db.begin_verify("a1", "h3", NOW).unwrap();
    assert_eq!(db.attempt("a1").unwrap().verify_summary, None);
    assert_eq!(db.mark_stale_verifies(NOW).unwrap(), ["a1"]);
    assert_eq!(
        db.attempt("a1").unwrap().verify_state,
        Some(VerifyState::Error)
    );
    assert!(db.mark_stale_verifies(NOW).unwrap().is_empty());

    // A closed attempt's verification is not the card's.
    db.finish_discard("a1", NOW).unwrap();
    let card = db.task_card("t").unwrap();
    assert_eq!((card.verify_state, card.verify_fixes), (None, 0));

    for missing in [
        err(db.begin_verify("nope", "h", NOW)),
        err(db.finish_verify("nope", VerifyState::Passed, None, "h", NOW)),
        err(db.add_verify_fix("nope", NOW)),
    ] {
        assert_eq!(missing.code, ErrorCode::NotFound);
    }
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
        auto: None,
        after_id: None,
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
        "UPDATE attempts SET max_subagents = 11",
        "UPDATE attempts SET max_subagents = -1",
        "UPDATE attempts SET subagents_used = -1",
        "UPDATE projects SET description = substr(hex(zeroblob(5001)), 1, 10001)",
        "INSERT INTO task_attachments (id, task_id, name, size, created_at)
            VALUES ('x', 't', '', 1, 0)",
        "INSERT INTO task_attachments (id, task_id, name, size, created_at)
            VALUES ('x', 't', 'a.txt', -1, 0)",
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
        description: "Il sito di prova".into(),
        autopilot: false,
        autopilot_merge: false,
        verify_command: None,
        verify_timeout_secs: 600,
        autopilot_max_fixes: 2,
    };
    let p = db.update_project(&req, NOW + 1).unwrap();
    assert_eq!(p, db.project("a").unwrap());
    assert_eq!(
        (
            p.name.as_str(),
            p.default_target_branch.as_str(),
            p.default_permission_mode,
            p.description.as_str()
        ),
        (
            "Alfa",
            "develop",
            PermissionMode::Default,
            "Il sito di prova"
        )
    );
    assert_eq!(p.to_project(false).description, "Il sito di prova");
    // The description's CHECK counts characters, not bytes.
    let long = UpdateProjectReq {
        description: "è".repeat(MAX_PROJECT_DESCRIPTION),
        ..req.clone()
    };
    db.update_project(&long, NOW + 1).unwrap();
    let too_long = UpdateProjectReq {
        description: "d".repeat(MAX_PROJECT_DESCRIPTION + 1),
        ..req.clone()
    };
    assert_eq!(
        err(db.update_project(&too_long, NOW + 1)).code,
        ErrorCode::Invalid
    );
    let p = db.update_project(&req, NOW + 1).unwrap();
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
        description: String::new(),
        autopilot: false,
        autopilot_merge: false,
        verify_command: None,
        verify_timeout_secs: 600,
        autopilot_max_fixes: 2,
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
        auto: None,
        after_id: None,
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
        notifications: false,
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

// ---- feature round 2026-09-29: attachments, sub-agents, cleanup ids ------------------------

#[test]
fn attachments_are_listed_capped_and_deleted() {
    let db = seeded();
    db.insert_task("t2", &new_task("p", "Altro", None), NOW)
        .unwrap();
    let first: Vec<AttachmentRow> = (0..MAX_ATTACHMENTS_PER_TASK - 1)
        .map(|i| attachment(&format!("x{i:02}"), "t", NOW + i as i64))
        .collect();
    db.insert_attachments(&first).unwrap();
    assert_eq!(
        db.attachment_count("t").unwrap(),
        MAX_ATTACHMENTS_PER_TASK - 1
    );

    // Past the limit nothing of the call is inserted (one transaction, counted again).
    let two = [
        attachment("y1", "t", NOW + 100),
        attachment("y2", "t", NOW + 101),
    ];
    let e = err(db.insert_attachments(&two));
    assert_eq!(e.code, ErrorCode::Invalid);
    assert!(
        e.message.contains(&MAX_ATTACHMENTS_PER_TASK.to_string()),
        "{e}"
    );
    assert_eq!(err(db.attachment("y1")).code, ErrorCode::NotFound);
    db.insert_attachments(&two[..1]).unwrap();
    assert_eq!(db.attachment_count("t").unwrap(), MAX_ATTACHMENTS_PER_TASK);
    let listed = db.task_attachments("t").unwrap();
    assert_eq!(listed.first().map(|a| a.id.as_str()), Some("x00"));
    assert_eq!(listed.last(), Some(&two[0]), "oldest first");
    assert_eq!(db.attachment_count("t2").unwrap(), 0, "per task");

    // Duplicate id, gone task, empty name.
    let e = err(db.insert_attachments(&[attachment("y1", "t2", NOW)]));
    assert_eq!(e.code, ErrorCode::Conflict);
    let e = err(db.insert_attachments(&[attachment("z", "nope", NOW)]));
    assert_eq!(e.code, ErrorCode::NotFound);
    let mut unnamed = attachment("z", "t2", NOW);
    unnamed.name = String::new();
    assert_eq!(
        err(db.insert_attachments(&[unnamed])).code,
        ErrorCode::Invalid
    );

    // The copy's path follows from the ids and the name.
    let view = attachments::view(Path::new("/data"), "p", &db.attachment("y1").unwrap());
    assert_eq!(
        (view.path.as_str(), view.name.as_str(), view.size),
        ("/data/attachments/p/t/y1/y1.txt", "y1.txt", 10)
    );

    assert_eq!(db.delete_attachment("y1").unwrap(), two[0]);
    assert_eq!(err(db.attachment("y1")).code, ErrorCode::NotFound);
    assert_eq!(err(db.delete_attachment("y1")).code, ErrorCode::NotFound);

    // Rows cascade with their task and project.
    db.insert_attachments(&[attachment("w", "t2", NOW)])
        .unwrap();
    db.delete_task("t").unwrap();
    assert_eq!(db.attachment_count("t").unwrap(), 0);
    db.delete_project("p").unwrap();
    assert_eq!(err(db.attachment("w")).code, ErrorCode::NotFound);
}

#[test]
fn subagent_spawns_are_counted_up_to_the_limit() {
    let db = seeded();
    for id in ["t2", "t3"] {
        db.insert_task(id, &new_task("p", id, None), NOW).unwrap();
    }
    let mut limited = attempt("a1", "t", NOW);
    limited.subagent_model = Some("haiku".into());
    limited.max_subagents = Some(2);
    db.begin_attempt(&limited, &process("r1", "a1", 1, "i"), NOW)
        .unwrap();
    assert_eq!(db.attempt("a1").unwrap(), limited);
    assert_eq!(db.count_subagent("a1", NOW + 1).unwrap(), Some(1));
    assert_eq!(db.count_subagent("a1", NOW + 2).unwrap(), Some(2));
    assert_eq!(db.count_subagent("a1", NOW + 3).unwrap(), None);
    let a = db.attempt("a1").unwrap();
    assert_eq!((a.subagents_used, a.updated_at), (2, NOW + 2));
    let view = a.view(false, 0);
    assert_eq!(
        (
            view.subagent_model.as_deref(),
            view.max_subagents,
            view.subagents_used
        ),
        (Some("haiku"), Some(2), 2)
    );

    // No limit: always counted. Zero: never.
    db.begin_attempt(&attempt("a2", "t2", NOW), &process("r2", "a2", 1, "i"), NOW)
        .unwrap();
    for n in 1..=12 {
        assert_eq!(db.count_subagent("a2", NOW).unwrap(), Some(n));
    }
    let mut none = attempt("a3", "t3", NOW);
    none.max_subagents = Some(0);
    db.begin_attempt(&none, &process("r3", "a3", 1, "i"), NOW)
        .unwrap();
    assert_eq!(db.count_subagent("a3", NOW).unwrap(), None);
    assert_eq!(
        err(db.count_subagent("nope", NOW)).code,
        ErrorCode::NotFound
    );

    // Over the limit of the contract: refused by the CHECK, nothing written.
    db.finish_discard("a3", NOW).unwrap();
    let mut over = attempt("a4", "t3", NOW);
    over.max_subagents = Some(11);
    let e = err(db.begin_attempt(&over, &process("r4", "a4", 1, "i"), NOW));
    assert_eq!(e.code, ErrorCode::Invalid);
    assert_eq!(err(db.attempt("a4")).code, ErrorCode::NotFound);
}

/// Attachment folders and logs live outside the DB: their ids are read before the cascade.
#[test]
fn task_and_attempt_ids_for_cleanup() {
    let db = seeded();
    db.insert_task("t2", &new_task("p", "Altro", None), NOW + 1)
        .unwrap();
    db.begin_attempt(&attempt("a1", "t", NOW), &process("r1", "a1", 1, "i"), NOW)
        .unwrap();
    db.begin_attempt(
        &attempt("a2", "t2", NOW + 1),
        &process("r2", "a2", 1, "i"),
        NOW,
    )
    .unwrap();
    db.finish_discard("a1", NOW + 2).unwrap();
    db.begin_attempt(
        &attempt("a3", "t", NOW + 3),
        &process("r3", "a3", 1, "i"),
        NOW,
    )
    .unwrap();

    assert_eq!(db.project_task_ids("p").unwrap(), ["t", "t2"]);
    assert_eq!(db.task_attempt_ids("t").unwrap(), ["a1", "a3"]);
    assert_eq!(db.project_attempt_ids("p").unwrap(), ["a1", "a2", "a3"]);
    assert!(db.project_task_ids("nope").unwrap().is_empty());
    assert!(db.project_attempt_ids("nope").unwrap().is_empty());
    assert!(db.task_attempt_ids("nope").unwrap().is_empty());
}

/// Sub-tasks (migration 0003): the parent is stored, the cards count done and total children,
/// the children are listed, deleting the parent cascades, the starter is a plain id.
#[test]
fn subtasks_are_stored_counted_listed_and_cascade() {
    let db = seeded();
    let child = |title: &str, status| CreateTaskReq {
        parent_id: Some("t".into()),
        ..new_task("p", title, Some(status))
    };
    db.insert_task("c1", &child("Uno", TaskStatus::Done), NOW)
        .unwrap();
    db.insert_task("c2", &child("Due", TaskStatus::Todo), NOW + 1)
        .unwrap();
    db.insert_task("c3", &child("Tre", TaskStatus::Todo), NOW + 2)
        .unwrap();
    db.insert_task("o", &new_task("p", "Altro", None), NOW + 3)
        .unwrap();
    assert_eq!(db.task("c1").unwrap().parent_id.as_deref(), Some("t"));
    assert_eq!(db.task("o").unwrap().parent_id, None);

    let board = db.board("p").unwrap();
    let counts = |id: &str| {
        let c = board.iter().find(|c| c.task.id == id).unwrap();
        (c.subtasks_done, c.subtasks_total)
    };
    assert_eq!(counts("t"), (1, 3));
    assert_eq!(counts("c1"), (0, 0));
    assert_eq!(counts("o"), (0, 0));
    let card = db.task_card("t").unwrap();
    assert_eq!((card.subtasks_done, card.subtasks_total), (1, 3));

    assert_eq!(db.task_children("t").unwrap(), ["c1", "c2", "c3"]);
    assert!(db.task_children("o").unwrap().is_empty());
    assert!(db.task_children("nope").unwrap().is_empty());
    let cards: Vec<_> = db
        .subtask_cards("t")
        .unwrap()
        .into_iter()
        .map(|c| c.task.id)
        .collect();
    assert_eq!(cards, ["c2", "c3", "c1"], "board order: todo before done");
    assert!(db.subtask_cards("nope").unwrap().is_empty());

    let orphan = CreateTaskReq {
        parent_id: Some("nope".into()),
        ..new_task("p", "Orfano", None)
    };
    assert!(db.insert_task("x", &orphan, NOW).is_err(), "unknown parent");

    let mut started = attempt("a2", "c2", NOW);
    started.started_by_attempt = Some("gone".into());
    db.begin_attempt(&started, &process("r2", "a2", 1, "i"), NOW)
        .unwrap();
    assert_eq!(
        db.attempt("a2").unwrap().started_by_attempt.as_deref(),
        Some("gone")
    );
    assert_eq!(
        db.attempt("a2")
            .unwrap()
            .view(false, 0)
            .started_by_attempt
            .as_deref(),
        Some("gone")
    );

    db.delete_task("t").unwrap();
    for id in ["t", "c1", "c2", "c3"] {
        assert_eq!(err(db.task(id)).code, ErrorCode::NotFound, "{id}");
    }
    assert_eq!(err(db.attempt("a2")).code, ErrorCode::NotFound);
    assert_eq!(db.task("o").unwrap().title, "Altro");
}
