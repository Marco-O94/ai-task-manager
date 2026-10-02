//! Mock of the task panel commands: detail, attempts, approvals, transcript, diff, merge.
//! Owner: M2-UI-TASK. Tasks come from `board::task`; lifecycle moves go to
//! `board::set_task_status` and the running count to `board::update_env`.
//!
//! Turns replay `fixtures/*.json` (`simple`, `approval`, `flood` = its templates repeated to
//! 10k entries in forwarder-sized batches) as a runner would: entries are stored first, then
//! sent as `Upsert` to every subscription, with typing previews and approvals that wait for
//! `respond_approval`. Seeded attempts, on the board baseline tasks:
//! - `task-inprogress`: a running turn, replayed from its first subscription;
//! - `task-inreview`: one finished turn, ready for review and merge;
//! - `task-done`: a merged attempt;
//! - `task-header`, `task-images`, `task-forms` (Autopilota of `sito-web`): a finished turn
//!   whose verification is running, passed, and failed once (1/2) with its output.
//!
//! Plans (`plan.rs`) get theirs from [`plan_attempt`], on the `plan` fixture.
//!
//! Query parameters, next to `?task=`: `fixture=simple|approval|flood` (the running turn
//! defaults to `approval`, the finished history and new turns to `simple`) and
//! `merge=clean|conflicts|dirty|nothing` (branch status and `merge_attempt` outcome).
//! `lagged=1` resends the `Snapshot` to every view at each approval request, as the backend
//! does after a `Lagged` (spec §6.5).

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;

use atm_types::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen_futures::spawn_local;

use super::{board, emit, new_id, now_ms, send_transcript, sleep};

const SIMPLE: &str = include_str!("fixtures/simple.json");
const APPROVAL: &str = include_str!("fixtures/approval.json");
const FLOOD: &str = include_str!("fixtures/flood.json");
const PLAN: &str = include_str!("fixtures/plan.json");
const DIFF: &str = include_str!("fixtures/diff.json");

const REPO: &str = "/Users/demo/demo";
const SNAPSHOT_TAIL: usize = 200;
const MAX_PAGE: u32 = 200;
const FLOOD_ENTRIES: usize = 10_000;
/// Forwarder batch: at most 200 entries every 50 ms (spec §6.5, §7.11).
const FORWARD_BATCH: usize = 200;
const FORWARD_MS: i32 = 50;
const STEP_MS: i32 = 450;
const TOOL_MS: i32 = 350;
const TYPING_MS: i32 = 110;
const TYPING_PARTS: usize = 3;
const POLL_MS: i32 = 100;
/// Lines of the generated file that exercises "mostra tutto" (over 2000 lines).
const LONG_FILE_LINES: u32 = 2_400;
const CONFLICT_FILES: [&str; 2] = ["tests/parser.rs", "README.md"];
/// Tail of the failed verification of `task-forms`.
const VERIFY_FAILED: &str = "FAIL src/forms.test.js\n  ✕ rifiuta un'email senza dominio (4 ms)\n\n    expect(received).toBe(expected)\n    Expected: false\n    Received: true\n\nTests: 1 failed, 17 passed, 18 total\n";
/// Start of the "Risolvi con l'agente" prompt: after that turn the preview is clean (§8.7).
const CONFLICT_PROMPT: &str = "This branch conflicts with";

/// One fixture step: an entry body, nested under a subagent's `tool_use_id` when `parent` is
/// set. A `ToolCall` holds its final state (`AwaitingApproval` = ask first, then succeed).
#[derive(Deserialize)]
struct Step {
    #[serde(default)]
    parent: Option<String>,
    body: EntryBody,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fixture {
    Simple,
    Approval,
    Flood,
    /// The planner's read-only turn (`plan.rs`), never chosen by `?fixture=`.
    Plan,
}

impl Fixture {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "simple" => Some(Self::Simple),
            "approval" => Some(Self::Approval),
            "flood" => Some(Self::Flood),
            _ => None,
        }
    }

    fn steps(self) -> Vec<Step> {
        match self {
            Self::Simple => parse_steps(SIMPLE),
            Self::Approval => parse_steps(APPROVAL),
            Self::Flood => flood_steps(),
            Self::Plan => parse_steps(PLAN),
        }
    }
}

fn parse_steps(json: &str) -> Vec<Step> {
    serde_json::from_str(json).expect("mock fixture")
}

/// The flood templates repeated to [`FLOOD_ENTRIES`] steps, `{n}` = the step number.
fn flood_steps() -> Vec<Step> {
    let templates: Vec<Value> = serde_json::from_str(FLOOD).expect("mock fixture");
    let templates: Vec<String> = templates.iter().map(Value::to_string).collect();
    (0..FLOOD_ENTRIES)
        .map(|n| {
            let json = templates[n % templates.len()].replace("{n}", &n.to_string());
            serde_json::from_str(&json).expect("mock fixture")
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Merge {
    Clean,
    Conflicts,
    Dirty,
    Nothing,
}

impl Merge {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "clean" => Some(Self::Clean),
            "conflicts" => Some(Self::Conflicts),
            "dirty" => Some(Self::Dirty),
            "nothing" => Some(Self::Nothing),
            _ => None,
        }
    }
}

/// How a turn ended; `Abandoned` = the attempt was discarded or forgotten meanwhile.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Completed,
    Stopped,
    DeniedAndStopped,
    Abandoned,
}

struct Attempt {
    view: AttemptView,
    processes: Vec<ProcessInfo>,
    /// The mock DB: ascending `idx`.
    entries: Vec<Entry>,
    typing: Option<String>,
    fixture: Fixture,
    merge: Merge,
    /// Bumped by every new turn and by discard: an older replay stops without finalizing.
    turn: u32,
    stop: bool,
    /// Seeded running turn, replayed from the first subscription.
    deferred_start: bool,
    /// Approvals of the running turn: id → the decision once answered.
    approvals: HashMap<Id, Option<ApprovalDecision>>,
    /// "Approva sempre" rules, `Tool(input)` of the fixture step.
    allow_rules: Vec<String>,
}

impl Attempt {
    /// The last [`SNAPSHOT_TAIL`] entries and the typing preview.
    fn snapshot(&self) -> TranscriptMsg {
        let start = self.entries.len().saturating_sub(SNAPSHOT_TAIL);
        TranscriptMsg::Snapshot {
            entries: self.entries[start..].to_vec(),
            has_more: start > 0,
            typing: self.typing.clone(),
        }
    }

    fn new(task: &Task, target_branch: String, fixture: Fixture, merge: Merge) -> Self {
        let id = new_id();
        let branch = format!("atm/{}-{}", &id[..8], slug(&task.title));
        Self {
            view: AttemptView {
                task_id: task.id.clone(),
                state: AttemptState::Active,
                worktree_path: format!("/Users/demo/.ai-task-manager/worktrees/demo/{}", &id[..8]),
                id,
                branch,
                target_branch,
                base_commit: fake_sha(),
                worktree_state: WorktreeState::Present,
                permission_mode: PermissionMode::AcceptEdits,
                model: None,
                effort: None,
                subagent_model: None,
                max_subagents: None,
                subagents_used: 0,
                session_started: false,
                merge_commit: None,
                running: false,
                pending_approvals: 0,
                created_at: now_ms(),
                closed_at: None,
                started_by_attempt: None,
                verify_state: None,
                verify_head: None,
                verify_fixes: 0,
                verify_summary: None,
            },
            processes: Vec::new(),
            entries: Vec::new(),
            typing: None,
            fixture,
            merge,
            turn: 0,
            stop: false,
            deferred_start: false,
            approvals: HashMap::new(),
            allow_rules: Vec::new(),
        }
    }

    /// New running process with its `UserMessage`.
    fn open_turn(&mut self, prompt: String) -> ProcessInfo {
        self.turn += 1;
        self.stop = false;
        self.view.running = true;
        self.approvals.clear();
        let process = ProcessInfo {
            id: new_id(),
            seq: self.processes.len() as u32 + 1,
            prompt: prompt.clone(),
            status: ProcessStatus::Running,
            stop_reason: None,
            result_subtype: None,
            is_error: None,
            cost_usd_estimate: None,
            duration_ms: None,
            num_turns: None,
            head_after: None,
            started_at: now_ms(),
            finished_at: None,
        };
        self.processes.push(process.clone());
        self.add(None, EntryBody::UserMessage { text: prompt });
        process
    }

    /// Finalizes the running process (spec §7.7), `Cancelled` for the tools still open.
    fn close_turn(&mut self, end: End) -> Vec<Entry> {
        let cancelled = self.cancel_open_tools();
        let (status, stop_reason) = match end {
            End::Completed => (ProcessStatus::Completed, None),
            _ => (ProcessStatus::Killed, Some(StopReason::UserStop)),
        };
        let result = self.entries.iter().rev().find_map(|e| match &e.body {
            EntryBody::TurnEnd {
                subtype,
                is_error,
                cost_usd_estimate,
                num_turns,
                ..
            } => Some((subtype.clone(), *is_error, *cost_usd_estimate, *num_turns)),
            _ => None,
        });
        let now = now_ms();
        if let Some(p) = self.processes.last_mut() {
            p.status = status;
            p.stop_reason = stop_reason;
            p.finished_at = Some(now);
            p.duration_ms = Some((now - p.started_at).max(0) as u64);
            p.head_after = Some(fake_sha());
            if end == End::Completed
                && let Some((subtype, is_error, cost, turns)) = result
            {
                p.result_subtype = Some(subtype);
                p.is_error = Some(is_error);
                p.cost_usd_estimate = cost;
                p.num_turns = turns;
            }
        }
        let resolved = end == End::Completed
            && self
                .processes
                .last()
                .is_some_and(|p| p.prompt.starts_with(CONFLICT_PROMPT));
        if resolved {
            self.merge = Merge::Clean;
        }
        self.view.running = false;
        self.view.session_started = true;
        self.view.pending_approvals = 0;
        self.approvals.clear();
        self.stop = false;
        self.typing = None;
        cancelled
    }

    fn add(&mut self, parent: Option<String>, body: EntryBody) -> Entry {
        let entry = Entry {
            idx: self.entries.last().map_or(0, |e| e.idx + 1),
            rev: 1,
            process_id: self
                .processes
                .last()
                .map(|p| p.id.clone())
                .unwrap_or_default(),
            ts: now_ms(),
            parent_tool_use_id: parent,
            body,
        };
        self.entries.push(entry.clone());
        entry
    }

    fn update(&mut self, idx: u32, body: EntryBody) -> Option<Entry> {
        let entry = self.entries.iter_mut().rev().find(|e| e.idx == idx)?;
        entry.rev += 1;
        entry.body = body;
        Some(entry.clone())
    }

    fn cancel_open_tools(&mut self) -> Vec<Entry> {
        let Some(process) = self.processes.last().map(|p| p.id.clone()) else {
            return Vec::new();
        };
        let mut cancelled = Vec::new();
        for e in self.entries.iter_mut().rev() {
            if e.process_id != process {
                break;
            }
            if let EntryBody::ToolCall { status, .. } = &mut e.body
                && matches!(
                    status,
                    ToolStatus::Running | ToolStatus::AwaitingApproval { .. }
                )
            {
                *status = ToolStatus::Cancelled;
                e.rev += 1;
                cancelled.push(e.clone());
            }
        }
        cancelled.reverse();
        cancelled
    }

    /// Replays `steps` at once, as a finished turn (seeded history).
    fn materialize(&mut self, steps: Vec<Step>) {
        for step in steps {
            let body = match step.body {
                EntryBody::ToolCall {
                    tool_use_id,
                    name,
                    summary,
                    input,
                    status,
                    output,
                } => EntryBody::ToolCall {
                    tool_use_id,
                    name,
                    summary,
                    input,
                    status: match status {
                        ToolStatus::AwaitingApproval { .. } => ToolStatus::Succeeded,
                        status => status,
                    },
                    output,
                },
                body => body,
            };
            self.add(step.parent, body);
        }
    }
}

#[derive(Default)]
struct State {
    attempts: Vec<Attempt>,
    /// Transcript subscription → attempt.
    subs: HashMap<Id, Id>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(seed());
}

fn seed() -> State {
    let fixture = query("fixture").and_then(|f| Fixture::parse(&f));
    let merge = query("merge")
        .and_then(|m| Merge::parse(&m))
        .unwrap_or(Merge::Clean);
    let mut attempts = Vec::new();
    if let Some(task) = board::task("task-inprogress") {
        let mut a = Attempt::new(
            &task,
            "main".into(),
            fixture.unwrap_or(Fixture::Approval),
            merge,
        );
        a.view.permission_mode = PermissionMode::Default;
        a.open_turn(prompt_of(&task));
        a.deferred_start = true;
        attempts.push(a);
    }
    for (id, merged) in [("task-inreview", false), ("task-done", true)] {
        let Some(task) = board::task(id) else {
            continue;
        };
        let fixture = fixture.unwrap_or(Fixture::Simple);
        let mut a = Attempt::new(&task, "main".into(), fixture, merge);
        a.open_turn(prompt_of(&task));
        a.materialize(fixture.steps());
        a.close_turn(End::Completed);
        if merged {
            a.view.state = AttemptState::Merged;
            a.view.merge_commit = Some(fake_sha());
            a.view.closed_at = Some(now_ms());
            a.view.worktree_state = WorktreeState::Removed;
        }
        attempts.push(a);
    }
    let verifications = [
        ("task-header", VerifyState::Running, 0, None),
        (
            "task-images",
            VerifyState::Passed,
            0,
            Some("Tests: 42 passed, 42 total\n"),
        ),
        ("task-forms", VerifyState::Failed, 1, Some(VERIFY_FAILED)),
    ];
    for (id, state, fixes, summary) in verifications {
        let Some(task) = board::task(id) else {
            continue;
        };
        let mut a = Attempt::new(&task, "main".into(), Fixture::Simple, merge);
        a.open_turn(prompt_of(&task));
        a.materialize(Fixture::Simple.steps());
        a.close_turn(End::Completed);
        a.view.verify_state = Some(state);
        a.view.verify_head = Some(fake_sha());
        a.view.verify_fixes = fixes;
        a.view.verify_summary = summary.map(Into::into);
        attempts.push(a);
    }
    State {
        attempts,
        subs: HashMap::new(),
    }
}

fn prompt_of(task: &Task) -> String {
    if task.description.trim().is_empty() {
        task.title.clone()
    } else {
        format!("{}\n\n{}", task.title, task.description)
    }
}

/// Serves one attempt-side command: `req` is the serialized `C::Req`, the result the
/// serialized `C::Res`.
pub async fn handle(cmd: &str, req: Value) -> Result<Value, AppError> {
    match cmd {
        GetTaskDetail::NAME => reply(task_detail(&parse::<IdReq>(req)?.id)),
        StartAttempt::NAME => reply(start_attempt(parse(req)?)),
        SendFollowUp::NAME => reply(send_follow_up(parse(req)?)),
        StopAttempt::NAME => reply(stop_attempt(&parse::<AttemptIdReq>(req)?.attempt_id)),
        RespondApproval::NAME => reply(respond_approval(parse(req)?)),
        GetEntries::NAME => reply(get_entries(parse(req)?)),
        GetDiff::NAME => reply(get_diff(&parse::<AttemptIdReq>(req)?.attempt_id)),
        GetBranchStatus::NAME => reply(branch_status(&parse::<AttemptIdReq>(req)?.attempt_id)),
        MergeAttempt::NAME => reply(merge_attempt(parse(req)?)),
        DiscardAttempt::NAME => reply(discard_attempt(&parse::<AttemptIdReq>(req)?.attempt_id)),
        DeleteBranch::NAME => reply(delete_branch(&parse::<AttemptIdReq>(req)?.attempt_id)),
        OpenAttempt::NAME => {
            let req: OpenAttemptReq = parse(req)?;
            reply(with_attempt(&req.attempt_id, |_| ()))
        }
        _ => Err(AppError::not_implemented(cmd)),
    }
}

fn parse<T: DeserializeOwned>(req: Value) -> Result<T, AppError> {
    Ok(serde_json::from_value(req)?)
}

fn reply<T: Serialize>(res: Result<T, AppError>) -> Result<Value, AppError> {
    Ok(serde_json::to_value(res?)?)
}

/// A view subscribed to `attempt_id`'s transcript: replay with `super::send_transcript`
/// (a `Snapshot` first), stopping when it returns `false`.
pub fn on_subscribe(sub_id: &str, attempt_id: &str) {
    let found = STATE.with_borrow_mut(|s| {
        s.subs.insert(sub_id.to_owned(), attempt_id.to_owned());
        let a = find_mut(s, attempt_id)?;
        let snapshot = a.snapshot();
        let deferred = mem::take(&mut a.deferred_start).then_some(a.turn);
        Some((snapshot, deferred))
    });
    let (snapshot, deferred) = found.unwrap_or((
        TranscriptMsg::Snapshot {
            entries: Vec::new(),
            has_more: false,
            typing: None,
        },
        None,
    ));
    if !send_transcript(sub_id, &snapshot) {
        on_unsubscribe(sub_id);
    }
    if let Some(turn) = deferred {
        running_changed(1);
        spawn_local(run_turn(attempt_id.to_owned(), turn));
    }
}

/// `unsubscribe_transcript` (the channel is already detached).
pub fn on_unsubscribe(sub_id: &str) {
    STATE.with_borrow_mut(|s| s.subs.remove(sub_id));
}

/// For `board.rs`: fills the attempt fields of a card (`attempt_id`, `attempt_state`,
/// `branch`, `running`, `pending_approvals`, `last_status`, `last_stop_reason`,
/// `worktree_state`).
pub fn decorate(card: &mut TaskCard) {
    STATE.with_borrow(|s| {
        let Some(a) = s
            .attempts
            .iter()
            .filter(|a| a.view.task_id == card.task.id)
            .max_by_key(|a| (a.view.state == AttemptState::Active, a.view.created_at))
        else {
            return;
        };
        let last = a.processes.last();
        card.attempt_id = Some(a.view.id.clone());
        card.attempt_state = Some(a.view.state);
        card.branch = Some(a.view.branch.clone());
        card.running = a.view.running;
        card.pending_approvals = a.view.pending_approvals;
        card.last_status = last.map(|p| p.status);
        card.last_stop_reason = last.and_then(|p| p.stop_reason);
        card.worktree_state = Some(a.view.worktree_state);
        if a.view.state == AttemptState::Active {
            // No verifier here: a `running` one stands for the live one.
            card.verifying = a.view.verify_state == Some(VerifyState::Running);
            card.verify_state = a.view.verify_state;
            card.verify_fixes = a.view.verify_fixes;
        }
    });
}

/// For `board.rs`: the task was deleted, or its project removed; drop its attempts.
#[allow(dead_code)] // called by the stateful board mock (M2-UI-BOARD), not by the M1 baseline
pub fn forget_task(task_id: &str) {
    STATE.with_borrow_mut(|s| s.attempts.retain(|a| a.view.task_id != task_id));
}

/// For `plan.rs`: the attempt of a plan, whose hidden task is not on the board (mode Default,
/// the `plan` fixture). `running`: its turn starts now (`deferred`: at its first subscription,
/// like `task-inprogress`) and ends on its own or with `stop_attempt`; else one finished turn,
/// for a seeded plan.
pub fn plan_attempt(plan: &Task, running: bool, deferred: bool) -> Id {
    let mut a = Attempt::new(plan, "main".into(), Fixture::Plan, Merge::Nothing);
    a.view.permission_mode = PermissionMode::Default;
    a.open_turn(plan.description.clone());
    let (id, turn) = (a.view.id.clone(), a.turn);
    if !running {
        a.materialize(Fixture::Plan.steps());
        a.close_turn(End::Completed);
    }
    a.deferred_start = running && deferred;
    STATE.with_borrow_mut(|s| s.attempts.push(a));
    if running && !deferred {
        running_changed(1);
        spawn_local(run_turn(id.clone(), turn));
    }
    id
}

/// For `plan.rs`: `None` while the plan's turn runs, else whether it completed (not stopped).
pub fn plan_turn_completed(attempt_id: &str) -> Option<bool> {
    with_attempt(attempt_id, |a| {
        (!a.view.running).then(|| {
            a.processes
                .last()
                .is_some_and(|p| p.status == ProcessStatus::Completed)
        })
    })
    .ok()
    .flatten()
}

fn find_mut<'a>(s: &'a mut State, attempt_id: &str) -> Option<&'a mut Attempt> {
    s.attempts.iter_mut().find(|a| a.view.id == attempt_id)
}

fn with_attempt<R>(attempt_id: &str, f: impl FnOnce(&mut Attempt) -> R) -> Result<R, AppError> {
    STATE
        .with_borrow_mut(|s| find_mut(s, attempt_id).map(f))
        .ok_or_else(|| AppError::not_found(format!("Tentativo {attempt_id} non trovato.")))
}

fn task_detail(task_id: &str) -> Result<TaskDetail, AppError> {
    let task = board::task(task_id)
        .ok_or_else(|| AppError::not_found(format!("Task {task_id} non trovato.")))?;
    let subtasks = board::subtask_cards(task_id);
    Ok(STATE.with_borrow(|s| {
        let mine = || s.attempts.iter().filter(|a| a.view.task_id == task_id);
        let active = mine().find(|a| a.view.state == AttemptState::Active);
        let mut closed_attempts: Vec<AttemptView> = mine()
            .filter(|a| a.view.state != AttemptState::Active)
            .map(|a| a.view.clone())
            .collect();
        closed_attempts.sort_by_key(|a| a.created_at);
        TaskDetail {
            task,
            attempt: active.map(|a| a.view.clone()),
            processes: active.map(|a| a.processes.clone()).unwrap_or_default(),
            closed_attempts,
            attachments: super::attachments::of_task(task_id),
            subtasks,
        }
    }))
}

fn start_attempt(req: StartAttemptReq) -> Result<AttemptView, AppError> {
    let task = board::task(&req.task_id)
        .ok_or_else(|| AppError::not_found(format!("Task {} non trovato.", req.task_id)))?;
    let fixture = query("fixture")
        .and_then(|f| Fixture::parse(&f))
        .unwrap_or(Fixture::Simple);
    let merge = query("merge")
        .and_then(|m| Merge::parse(&m))
        .unwrap_or(Merge::Clean);
    // Like the core: the requested model, else the project's default, else the settings'.
    let model = req
        .model
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty())
        .or_else(|| board::default_model(&task.project_id));
    let view = STATE.with_borrow_mut(|s| {
        let busy = s
            .attempts
            .iter()
            .any(|a| a.view.task_id == task.id && a.view.state == AttemptState::Active);
        if busy {
            return Err(AppError::conflict("Il task ha già un tentativo attivo."));
        }
        let mut a = Attempt::new(&task, req.target_branch, fixture, merge);
        a.view.permission_mode = req.permission_mode;
        a.view.model = model;
        a.view.effort = req.effort;
        a.view.subagent_model = req.subagent_model;
        a.view.max_subagents = req.max_subagents;
        a.open_turn(prompt_of(&task));
        let view = a.view.clone();
        s.attempts.push(a);
        Ok(view)
    })?;
    board::set_task_status(&task.id, TaskStatus::InProgress);
    begin(&view.id);
    Ok(view)
}

fn send_follow_up(req: SendFollowUpReq) -> Result<ProcessInfo, AppError> {
    let process = with_attempt(&req.attempt_id, |a| {
        if a.view.state != AttemptState::Active {
            return Err(AppError::invalid("Il tentativo è chiuso."));
        }
        if a.view.running {
            return Err(AppError::busy("Un turno è già in corso."));
        }
        Ok(a.open_turn(req.prompt))
    })??;
    if let Some(task) = task_of(&req.attempt_id) {
        board::set_task_status(&task, TaskStatus::InProgress);
    }
    begin(&req.attempt_id);
    Ok(process)
}

/// Publishes the `UserMessage` of the turn just opened and replays it in the background.
fn begin(attempt_id: &str) {
    let Ok((user, turn)) = with_attempt(attempt_id, |a| (a.entries.last().cloned(), a.turn)) else {
        return;
    };
    if let Some(user) = user {
        broadcast(
            attempt_id,
            &TranscriptMsg::Upsert {
                entries: vec![user],
            },
        );
    }
    running_changed(1);
    changed(attempt_id);
    spawn_local(run_turn(attempt_id.to_owned(), turn));
}

fn stop_attempt(attempt_id: &str) -> Result<(), AppError> {
    with_attempt(attempt_id, |a| {
        if a.view.running {
            a.stop = true;
        }
    })
}

fn respond_approval(req: RespondApprovalReq) -> Result<(), AppError> {
    with_attempt(&req.attempt_id, |a| {
        match a.approvals.get_mut(&req.approval_id) {
            Some(slot @ None) => {
                *slot = Some(req.decision);
                Ok(())
            }
            _ => Err(AppError::not_found("L'approvazione non è più in attesa.")),
        }
    })?
}

fn get_entries(req: GetEntriesReq) -> Result<EntryPage, AppError> {
    if req.limit > MAX_PAGE {
        return Err(AppError::invalid(format!("limit oltre {MAX_PAGE}")));
    }
    Ok(STATE.with_borrow_mut(|s| {
        let Some(a) = find_mut(s, &req.attempt_id) else {
            return EntryPage {
                entries: Vec::new(),
                has_more: false,
            };
        };
        let older = &a.entries[..a.entries.partition_point(|e| e.idx < req.before_idx)];
        let start = older.len().saturating_sub(req.limit as usize);
        EntryPage {
            entries: older[start..].to_vec(),
            has_more: start > 0,
        }
    }))
}

fn get_diff(attempt_id: &str) -> Result<DiffResult, AppError> {
    let present = with_attempt(attempt_id, |a| {
        a.view.worktree_state == WorktreeState::Present
    })?;
    if !present {
        return Err(AppError::new(
            ErrorCode::WorktreeMissing,
            "Il worktree del tentativo non esiste più.",
        ));
    }
    let mut diff: DiffResult = serde_json::from_str(DIFF).expect("mock fixture");
    let long = (1..=LONG_FILE_LINES).map(|n| DiffLine {
        kind: LineKind::Add,
        old_no: None,
        new_no: Some(n),
        text: format!("    ({n}, \"{:04x}\"),", n * 7919 % 65_536),
    });
    let hunk = DiffLine {
        kind: LineKind::Hunk,
        old_no: None,
        new_no: None,
        text: format!("@@ -0,0 +1,{LONG_FILE_LINES} @@"),
    };
    diff.files.push(FileDiff {
        path: "src/generated/tables.rs".into(),
        old_path: None,
        status: FileStatus::Added,
        additions: LONG_FILE_LINES,
        deletions: 0,
        binary: false,
        too_large: false,
        omitted: false,
        lines: std::iter::once(hunk).chain(long).collect(),
    });
    diff.additions = diff.files.iter().map(|f| f.additions).sum();
    diff.deletions = diff.files.iter().map(|f| f.deletions).sum();
    Ok(diff)
}

fn branch_status(attempt_id: &str) -> Result<BranchStatus, AppError> {
    with_attempt(attempt_id, |a| {
        let mut status = BranchStatus {
            target_branch: a.view.target_branch.clone(),
            ahead: a.processes.len() as u32,
            behind: 0,
            dirty: a.view.running,
            head_ok: true,
            conflicts: Vec::new(),
            target_checked_out_at: None,
            merge_blocked: None,
        };
        match a.merge {
            Merge::Clean => {}
            Merge::Conflicts => {
                status.behind = 2;
                status.conflicts = CONFLICT_FILES.map(String::from).to_vec();
            }
            Merge::Dirty => status.target_checked_out_at = Some(REPO.into()),
            Merge::Nothing => status.ahead = 0,
        }
        status.merge_blocked = if a.view.state != AttemptState::Active {
            Some("Il tentativo è chiuso.".into())
        } else if a.view.running {
            Some("Un turno è in corso: attendi la fine o fermalo.".into())
        } else {
            None
        };
        status
    })
}

fn merge_attempt(req: MergeAttemptReq) -> Result<MergeOutcome, AppError> {
    if req.message.trim().is_empty() {
        return Err(AppError::invalid("Il messaggio del commit è vuoto."));
    }
    let outcome = with_attempt(&req.attempt_id, |a| {
        if a.view.state != AttemptState::Active {
            return Err(AppError::invalid("Il tentativo è chiuso."));
        }
        if a.view.running {
            return Err(AppError::busy(
                "Un turno è in corso: fermalo prima del merge.",
            ));
        }
        match a.merge {
            Merge::Conflicts => Ok(MergeOutcome::Conflicts {
                files: CONFLICT_FILES.map(String::from).to_vec(),
            }),
            Merge::Dirty => Err(AppError::new(
                ErrorCode::TargetCheckoutDirty,
                format!(
                    "Il checkout di {} in {REPO} ha modifiche locali che il merge sovrascriverebbe.",
                    a.view.target_branch
                ),
            )),
            Merge::Nothing => Ok(MergeOutcome::NothingToMerge),
            Merge::Clean => {
                let commit = fake_sha();
                a.view.state = AttemptState::Merged;
                a.view.merge_commit = Some(commit.clone());
                a.view.closed_at = Some(now_ms());
                a.view.worktree_state = WorktreeState::Removed;
                Ok(MergeOutcome::Merged {
                    commit,
                    strategy: MergeStrategy::UpdateRef,
                    cleanup_warning: None,
                })
            }
        }
    })??;
    if matches!(outcome, MergeOutcome::Merged { .. }) {
        if let Some(task) = task_of(&req.attempt_id) {
            board::set_task_status(&task, TaskStatus::Done);
        }
        changed(&req.attempt_id);
    }
    Ok(outcome)
}

fn discard_attempt(attempt_id: &str) -> Result<(), AppError> {
    let (was_running, cancelled) = with_attempt(attempt_id, |a| {
        if a.view.state != AttemptState::Active {
            return Err(AppError::invalid("Il tentativo è già chiuso."));
        }
        let was_running = a.view.running;
        let cancelled = if was_running {
            a.close_turn(End::Stopped)
        } else {
            Vec::new()
        };
        a.turn += 1; // the replay in flight ends without finalizing
        a.view.state = AttemptState::Discarded;
        a.view.closed_at = Some(now_ms());
        a.view.worktree_state = WorktreeState::Removed;
        Ok((was_running, cancelled))
    })??;
    if was_running {
        broadcast(attempt_id, &TranscriptMsg::Typing { text: None });
        if !cancelled.is_empty() {
            broadcast(attempt_id, &TranscriptMsg::Upsert { entries: cancelled });
        }
        running_changed(-1);
    }
    if let Some(task) = task_of(attempt_id) {
        board::set_task_status(&task, TaskStatus::Todo);
    }
    changed(attempt_id);
    Ok(())
}

fn delete_branch(attempt_id: &str) -> Result<(), AppError> {
    with_attempt(attempt_id, |a| {
        if a.view.state == AttemptState::Merged && a.view.branch.starts_with("atm/") {
            Ok(())
        } else {
            Err(AppError::invalid(
                "Si possono eliminare solo i branch atm/… dei tentativi mergiati.",
            ))
        }
    })?
}

/// One turn in the background: the replay, then the finalization (unless abandoned).
async fn run_turn(attempt_id: Id, turn: u32) {
    let Ok(fixture) = with_attempt(&attempt_id, |a| a.fixture) else {
        return;
    };
    let result = if fixture == Fixture::Flood {
        flood(&attempt_id, turn).await
    } else {
        replay(&attempt_id, turn, fixture.steps()).await
    };
    let end = result.err().unwrap_or(End::Completed);
    if end == End::Abandoned {
        return;
    }
    let Ok(cancelled) = with_attempt(&attempt_id, |a| a.close_turn(end)) else {
        return;
    };
    set_typing(&attempt_id, None);
    if !cancelled.is_empty() {
        broadcast(&attempt_id, &TranscriptMsg::Upsert { entries: cancelled });
    }
    if matches!(end, End::Stopped | End::DeniedAndStopped) {
        // Like the core (M6): the CLI's error `result` after the interrupt, without its text.
        let body = EntryBody::TurnEnd {
            subtype: "error_during_execution".into(),
            is_error: true,
            duration_ms: Some(700),
            num_turns: Some(1),
            cost_usd_estimate: Some(0.01),
            permission_denials: u32::from(end == End::DeniedAndStopped),
            text: None,
            limit: None,
            stopped: Some(StopReason::UserStop),
        };
        let _ = push(&attempt_id, None, body);
    }
    let notice = match end {
        End::Stopped => Some("Esecuzione fermata dall'utente."),
        End::DeniedAndStopped => Some("Turno fermato con «Rifiuta e ferma»."),
        End::Completed | End::Abandoned => None,
    };
    if let Some(text) = notice {
        let body = EntryBody::Notice {
            level: Level::Info,
            text: text.into(),
            action: None,
        };
        let _ = push(&attempt_id, None, body);
    }
    if let Some(task) = task_of(&attempt_id) {
        board::set_task_status(&task, TaskStatus::InReview);
    }
    running_changed(-1);
    changed(&attempt_id);
}

async fn replay(attempt_id: &str, turn: u32, steps: Vec<Step>) -> Result<(), End> {
    // Subagents stay running until their nested steps are over.
    let mut open: Vec<(String, u32, EntryBody)> = Vec::new();
    let mut steps = steps.into_iter().peekable();
    while let Some(step) = steps.next() {
        sleep(STEP_MS).await;
        check(attempt_id, turn)?;
        while let Some((tool, ..)) = open.last()
            && step.parent.as_deref() != Some(tool.as_str())
        {
            let (_, idx, body) = open.pop().expect("checked above");
            update(attempt_id, idx, body)?;
        }
        match step.body {
            EntryBody::AssistantText { ref text } | EntryBody::Thinking { ref text } => {
                let text = text.clone();
                stream_typing(attempt_id, turn, &text).await?;
                push(attempt_id, step.parent, step.body)?;
                set_typing(attempt_id, None);
            }
            EntryBody::ToolCall {
                ref tool_use_id, ..
            } => {
                let nests = steps
                    .peek()
                    .is_some_and(|next| next.parent.as_ref() == Some(tool_use_id));
                if let Some(parent) = tool_call(attempt_id, turn, step, nests).await? {
                    open.push(parent);
                }
            }
            body => {
                push(attempt_id, step.parent, body)?;
            }
        }
    }
    for (_, idx, body) in open.into_iter().rev() {
        update(attempt_id, idx, body)?;
    }
    Ok(())
}

/// One tool call: approval first when the step asks for it and no "Approva sempre" rule
/// matches, then its result. A subagent (`nests`) is returned still running, with its final
/// body, to be closed after its nested steps.
async fn tool_call(
    attempt_id: &str,
    turn: u32,
    step: Step,
    nests: bool,
) -> Result<Option<(String, u32, EntryBody)>, End> {
    let EntryBody::ToolCall {
        tool_use_id,
        name,
        summary,
        input,
        status,
        output,
    } = step.body
    else {
        return Ok(None);
    };
    let call = |status, output| EntryBody::ToolCall {
        tool_use_id: tool_use_id.clone(),
        name: name.clone(),
        summary: summary.clone(),
        input: input.clone(),
        status,
        output,
    };
    let rule = format!("{name}({input})");
    let remembered =
        with_attempt(attempt_id, |a| a.allow_rules.contains(&rule)).map_err(|_| End::Abandoned)?;
    let (idx, done) = match status {
        ToolStatus::AwaitingApproval {
            can_remember,
            reason,
            ..
        } if !remembered => {
            let approval_id = new_id();
            let asking = ToolStatus::AwaitingApproval {
                approval_id: approval_id.clone(),
                can_remember,
                reason,
            };
            let idx = push(attempt_id, step.parent, call(asking, None))?;
            match wait_decision(attempt_id, turn, &approval_id).await? {
                ApprovalDecision::Allow { remember } => {
                    if remember && can_remember {
                        let _ = with_attempt(attempt_id, |a| a.allow_rules.push(rule));
                    }
                    update(attempt_id, idx, call(ToolStatus::Running, None))?;
                }
                ApprovalDecision::Deny { message, interrupt } => {
                    update(attempt_id, idx, call(ToolStatus::Denied { message }, None))?;
                    return if interrupt {
                        Err(End::DeniedAndStopped)
                    } else {
                        Ok(None)
                    };
                }
            }
            (idx, ToolStatus::Succeeded)
        }
        ToolStatus::AwaitingApproval { .. } => (
            push(attempt_id, step.parent, call(ToolStatus::Running, None))?,
            ToolStatus::Succeeded,
        ),
        status => (
            push(attempt_id, step.parent, call(ToolStatus::Running, None))?,
            status,
        ),
    };
    let done = call(done, output);
    if nests {
        return Ok(Some((tool_use_id.clone(), idx, done)));
    }
    sleep(TOOL_MS).await;
    check(attempt_id, turn)?;
    update(attempt_id, idx, done)?;
    Ok(None)
}

async fn wait_decision(
    attempt_id: &str,
    turn: u32,
    approval_id: &str,
) -> Result<ApprovalDecision, End> {
    with_attempt(attempt_id, |a| {
        a.approvals.insert(approval_id.to_owned(), None);
        a.view.pending_approvals += 1;
    })
    .map_err(|_| End::Abandoned)?;
    changed(attempt_id);
    let mut lagged = query("lagged").is_some();
    let decision = loop {
        sleep(POLL_MS).await;
        check(attempt_id, turn)?;
        // Once the views have rendered the request.
        if mem::take(&mut lagged)
            && let Ok(snapshot) = with_attempt(attempt_id, |a| a.snapshot())
        {
            broadcast(attempt_id, &snapshot);
        }
        let answer = with_attempt(attempt_id, |a| a.approvals.get(approval_id).cloned())
            .ok()
            .flatten()
            .flatten();
        if let Some(decision) = answer {
            break decision;
        }
    };
    let _ = with_attempt(attempt_id, |a| {
        a.approvals.remove(approval_id);
        a.view.pending_approvals = a.view.pending_approvals.saturating_sub(1);
    });
    changed(attempt_id);
    Ok(decision)
}

async fn stream_typing(attempt_id: &str, turn: u32, text: &str) -> Result<(), End> {
    let chars: Vec<char> = text.chars().collect();
    for part in 1..=TYPING_PARTS {
        let shown: String = chars[..chars.len() * part / (TYPING_PARTS + 1)]
            .iter()
            .collect();
        set_typing(attempt_id, Some(shown));
        sleep(TYPING_MS).await;
        check(attempt_id, turn)?;
    }
    Ok(())
}

/// [`FLOOD_ENTRIES`] entries sent like the forwarder does under load, then a `TurnEnd`.
async fn flood(attempt_id: &str, turn: u32) -> Result<(), End> {
    let mut steps = flood_steps().into_iter().peekable();
    while steps.peek().is_some() {
        let batch = with_attempt(attempt_id, |a| {
            steps
                .by_ref()
                .take(FORWARD_BATCH)
                .map(|s| a.add(s.parent, s.body))
                .collect::<Vec<_>>()
        })
        .map_err(|_| End::Abandoned)?;
        broadcast(attempt_id, &TranscriptMsg::Upsert { entries: batch });
        sleep(FORWARD_MS).await;
        check(attempt_id, turn)?;
    }
    let body = EntryBody::TurnEnd {
        subtype: "success".into(),
        is_error: false,
        duration_ms: Some(2_600),
        num_turns: Some(FLOOD_ENTRIES as u32 / 4),
        cost_usd_estimate: Some(4.2),
        permission_denials: 0,
        text: None,
        limit: None,
        stopped: None,
    };
    push(attempt_id, None, body)?;
    Ok(())
}

/// `Err` when the replay must end: stop requested, or the turn superseded or gone.
fn check(attempt_id: &str, turn: u32) -> Result<(), End> {
    match with_attempt(attempt_id, |a| (a.turn, a.stop)) {
        Ok((t, _)) if t != turn => Err(End::Abandoned),
        Ok((_, true)) => Err(End::Stopped),
        Ok(_) => Ok(()),
        Err(_) => Err(End::Abandoned),
    }
}

/// Stores a new entry, then sends it (spec §6.5: persist, then broadcast).
fn push(attempt_id: &str, parent: Option<String>, body: EntryBody) -> Result<u32, End> {
    let entry = with_attempt(attempt_id, |a| a.add(parent, body)).map_err(|_| End::Abandoned)?;
    let idx = entry.idx;
    broadcast(
        attempt_id,
        &TranscriptMsg::Upsert {
            entries: vec![entry],
        },
    );
    Ok(idx)
}

fn update(attempt_id: &str, idx: u32, body: EntryBody) -> Result<(), End> {
    let entry = with_attempt(attempt_id, |a| a.update(idx, body))
        .ok()
        .flatten()
        .ok_or(End::Abandoned)?;
    broadcast(
        attempt_id,
        &TranscriptMsg::Upsert {
            entries: vec![entry],
        },
    );
    Ok(())
}

fn set_typing(attempt_id: &str, text: Option<String>) {
    if with_attempt(attempt_id, |a| a.typing = text.clone()).is_ok() {
        broadcast(attempt_id, &TranscriptMsg::Typing { text });
    }
}

fn broadcast(attempt_id: &str, msg: &TranscriptMsg) {
    // Collected first: no borrow is held while a view handles the message.
    let subs: Vec<Id> = STATE.with_borrow(|s| {
        s.subs
            .iter()
            .filter(|(_, a)| a.as_str() == attempt_id)
            .map(|(sub, _)| sub.clone())
            .collect()
    });
    for sub in subs {
        if !send_transcript(&sub, msg) {
            on_unsubscribe(&sub);
        }
    }
}

fn task_of(attempt_id: &str) -> Option<Id> {
    with_attempt(attempt_id, |a| a.view.task_id.clone()).ok()
}

/// `changed{project, task}` of the attempt's task, as the backend emits after each commit.
fn changed(attempt_id: &str) {
    let Some(task) = task_of(attempt_id).and_then(|id| board::task(&id)) else {
        return;
    };
    let payload = Changed {
        project_id: Some(task.project_id),
        task_id: Some(task.id),
    };
    emit(EVENT_CHANGED, &payload);
}

fn running_changed(delta: i32) {
    board::update_env(|env| env.running = env.running.saturating_add_signed(delta));
}

fn query(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        .map(str::to_owned)
}

fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(24).collect();
    if slug.is_empty() {
        "task".into()
    } else {
        slug.trim_end_matches('-').into()
    }
}

fn fake_sha() -> String {
    let hex = new_id().replace('-', "");
    format!("{hex}{}", &hex[..8])
}
