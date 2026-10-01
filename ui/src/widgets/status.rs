//! Status palette of the design (tokens `--status-*` in `style/tailwind.css`): the kanban
//! columns and the agent state share it, as dots, badges and bars. Also the pill tabs of the
//! topbar and of the board toolbar. Class strings are whole literals so that Tailwind's
//! scanner sees them.

use atm_types::{AttemptState, ProcessStatus, TaskCard, TaskStatus, VerifyState};
use icons::LoaderCircle;
use leptos::prelude::*;

use crate::views::board::interrupted_by_app;

/// One colour of the status palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Todo,
    Running,
    Waiting,
    Review,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    /// Colour of a kanban column: "In corso" is `running`, "In revisione" is `review`.
    pub fn of_column(status: TaskStatus) -> Self {
        match status {
            TaskStatus::Todo => Self::Todo,
            TaskStatus::InProgress => Self::Running,
            TaskStatus::InReview => Self::Review,
            TaskStatus::Done => Self::Done,
            TaskStatus::Cancelled => Self::Cancelled,
        }
    }

    /// Solid fill: dots, bar segments.
    pub fn bg(self) -> &'static str {
        match self {
            Self::Todo => "bg-status-todo",
            Self::Running => "bg-status-running",
            Self::Waiting => "bg-status-waiting",
            Self::Review => "bg-status-review",
            Self::Done => "bg-status-done",
            Self::Failed => "bg-status-failed",
            Self::Cancelled => "bg-status-cancelled",
        }
    }

    /// Tinted background plus text of a badge.
    pub fn tint(self) -> &'static str {
        match self {
            Self::Todo => "bg-status-todo/12 text-status-todo",
            Self::Running => "bg-status-running/12 text-status-running",
            Self::Waiting => "bg-status-waiting/12 text-status-waiting",
            Self::Review => "bg-status-review/12 text-status-review",
            Self::Done => "bg-status-done/12 text-status-done",
            Self::Failed => "bg-status-failed/12 text-status-failed",
            Self::Cancelled => "bg-status-cancelled/12 text-status-cancelled",
        }
    }

    /// Live states, whose dot pings: the agent works or waits for the user.
    pub fn pings(self) -> bool {
        matches!(self, Self::Running | Self::Waiting)
    }
}

/// The agent's state on a card, as its badge label and colour; `None` without an attempt
/// or with an active one that has not run yet. A pending approval wins over running.
pub fn agent_state(card: &TaskCard) -> Option<(&'static str, Status)> {
    if card.pending_approvals > 0 {
        return Some(("Attende approvazione", Status::Waiting));
    }
    if card.running {
        return Some(("In esecuzione", Status::Running));
    }
    match card.attempt_state? {
        AttemptState::Merged => Some(("Mergiato", Status::Done)),
        AttemptState::Discarded => Some(("Scartato", Status::Cancelled)),
        // Neutral and still: amber and the ping mean "waiting for you".
        AttemptState::Active if interrupted_by_app(card.last_stop_reason) => {
            Some(("Interrotto", Status::Todo))
        }
        AttemptState::Active => match card.last_status? {
            ProcessStatus::Failed => Some(("Fallito", Status::Failed)),
            ProcessStatus::Killed => Some(("Fermato", Status::Cancelled)),
            ProcessStatus::Completed => Some(("Pronto al merge", Status::Review)),
            // Not running per the registry: the process is ending.
            ProcessStatus::Running => Some(("In esecuzione", Status::Running)),
        },
    }
}

/// The autopilot's verification of the active attempt (spec A3) as its badge label, colour
/// and `data-verify`; `max_fixes` is the project's `autopilot_max_fixes`.
pub fn verify_state(card: &TaskCard, max_fixes: u32) -> Option<(String, Status, &'static str)> {
    if card.verifying {
        return Some(("Verifica…".into(), Status::Running, "running"));
    }
    Some(match card.verify_state? {
        VerifyState::Running => ("Verifica…".into(), Status::Running, "running"),
        VerifyState::Passed => ("Verificato".into(), Status::Done, "passed"),
        VerifyState::Failed => (
            format!("Verifica fallita ({}/{max_fixes})", card.verify_fixes),
            Status::Failed,
            "failed",
        ),
        VerifyState::Error => ("Verifica non eseguita".into(), Status::Failed, "error"),
    })
}

/// "In coda": a task given to the autopilot that waits in Da fare for a free slot or for
/// its dependency (`after_id`).
pub fn queued(card: &TaskCard) -> bool {
    card.task.auto
        && card.task.status == TaskStatus::Todo
        && card.attempt_state != Some(AttemptState::Active)
}

/// Small round dot of `status`; `ping` adds the halo of the live states (hidden with
/// reduced motion).
#[component]
pub fn StatusDot(status: Status, #[prop(optional)] ping: bool) -> impl IntoView {
    let bg = status.bg();
    view! {
        <span class=format!("relative inline-flex size-1.5 shrink-0 rounded-full {bg}")>
            {ping
                .then(|| {
                    view! {
                        <span class=format!(
                            "absolute inset-0 rounded-full {bg} animate-ping motion-reduce:hidden",
                        ) />
                    }
                })}
        </span>
    }
}

/// Tinted badge of an agent state (see [`agent_state`]), with its dot; `spin` shows a
/// spinner instead (a verification in progress).
#[component]
pub fn AgentBadge(
    #[prop(into)] label: String,
    status: Status,
    #[prop(optional)] spin: bool,
) -> impl IntoView {
    view! {
        <span class=format!(
            "inline-flex h-5 items-center gap-1.5 rounded-md px-1.5 text-[11px] font-medium whitespace-nowrap {}",
            status.tint(),
        )>
            {if spin {
                view! { <LoaderCircle class="size-3 animate-spin motion-reduce:animate-none" attr:aria-hidden="true" /> }
                    .into_any()
            } else {
                view! { <StatusDot status ping=status.pings() /> }.into_any()
            }}
            {label}
        </span>
    }
}

/// Focus ring of the design's interactive elements.
pub const FOCUS_RING: &str = "outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";

/// `role=tablist` of pill tabs (topbar pages, Kanban | Lista).
pub const PILL_TABLIST: &str = "inline-flex h-8 items-center rounded-lg bg-muted p-0.5";

/// One pill tab; the board toolbar narrows it with `px-2.5` (tw_merge).
pub fn pill_tab(selected: bool) -> &'static str {
    if selected {
        "inline-flex h-7 items-center gap-1.5 rounded-md px-3 text-[13px] font-medium bg-background text-foreground shadow-xs outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
    } else {
        "inline-flex h-7 items-center gap-1.5 rounded-md px-3 text-[13px] font-medium text-muted-foreground hover:text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atm_types::{StopReason, Task, WorktreeState};

    fn card() -> TaskCard {
        TaskCard {
            task: Task {
                id: "t".into(),
                project_id: "p".into(),
                title: "Titolo".into(),
                description: String::new(),
                status: TaskStatus::InProgress,
                position: 1.0,
                created_at: 0,
                updated_at: 0,
                parent_id: None,
                auto: false,
                after_id: None,
            },
            attempt_id: Some("a".into()),
            attempt_state: Some(AttemptState::Active),
            branch: Some("task/titolo".into()),
            running: false,
            pending_approvals: 0,
            last_status: None,
            last_stop_reason: None,
            worktree_state: Some(WorktreeState::Present),
            subtasks_done: 0,
            subtasks_total: 0,
            verifying: false,
            verify_state: None,
            verify_fixes: 0,
        }
    }

    #[test]
    fn columns_map_to_their_status_colour() {
        assert_eq!(Status::of_column(TaskStatus::InProgress), Status::Running);
        assert_eq!(Status::of_column(TaskStatus::InReview), Status::Review);
        assert_eq!(
            Status::of_column(TaskStatus::Cancelled).bg(),
            "bg-status-cancelled"
        );
    }

    #[test]
    fn agent_state_follows_the_attempt() {
        let state = |f: fn(&mut TaskCard)| {
            let mut c = card();
            f(&mut c);
            agent_state(&c)
        };
        assert_eq!(state(|_| {}), None);
        assert_eq!(state(|c| c.attempt_state = None), None);
        let running = state(|c| c.running = true);
        assert_eq!(running, Some(("In esecuzione", Status::Running)));
        let waiting = state(|c| {
            c.running = true;
            c.pending_approvals = 2;
        });
        assert_eq!(waiting, Some(("Attende approvazione", Status::Waiting)));
        let failed = state(|c| c.last_status = Some(ProcessStatus::Failed));
        assert_eq!(failed, Some(("Fallito", Status::Failed)));
        let review = state(|c| c.last_status = Some(ProcessStatus::Completed));
        assert_eq!(review, Some(("Pronto al merge", Status::Review)));
        let interrupted = state(|c| {
            c.last_status = Some(ProcessStatus::Killed);
            c.last_stop_reason = Some(StopReason::AppShutdown);
        });
        assert_eq!(interrupted, Some(("Interrotto", Status::Todo)));
        let merged = state(|c| c.attempt_state = Some(AttemptState::Merged));
        assert_eq!(merged, Some(("Mergiato", Status::Done)));
        let discarded = state(|c| c.attempt_state = Some(AttemptState::Discarded));
        assert_eq!(discarded, Some(("Scartato", Status::Cancelled)));
    }

    #[test]
    fn verification_and_queue_follow_the_card() {
        let verify = |f: fn(&mut TaskCard)| {
            let mut c = card();
            f(&mut c);
            verify_state(&c, 2).map(|(label, _, name)| (label, name))
        };
        assert_eq!(verify(|_| {}), None);
        let live = verify(|c| c.verifying = true);
        assert_eq!(live, Some(("Verifica…".into(), "running")));
        let passed = verify(|c| c.verify_state = Some(VerifyState::Passed));
        assert_eq!(passed, Some(("Verificato".into(), "passed")));
        let failed = verify(|c| {
            c.verify_state = Some(VerifyState::Failed);
            c.verify_fixes = 1;
        });
        assert_eq!(failed, Some(("Verifica fallita (1/2)".into(), "failed")));

        let mut c = card();
        assert!(!queued(&c));
        c.task.auto = true;
        c.task.status = TaskStatus::Todo;
        assert!(!queued(&c), "an active attempt is not queued");
        c.attempt_state = None;
        assert!(queued(&c));
    }
}
