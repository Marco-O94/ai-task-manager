//! List view of the board (spec F4): the same cards as the columns, as one semantic table in
//! column order then position, each sub-task right under its parent (a parent's sub-tasks
//! fold with its "↳ n/m" toggle, not persisted). No search, filter or sort yet. Owner:
//! UI-TASKS.

use std::collections::HashSet;

use atm_types::{Id, Millis, TaskCard, TaskStatus};
use icons::{ChevronDown, Pencil};
use leptos::prelude::*;
use wasm_bindgen::JsValue;

use super::{BoardState, SubtaskProgress, branch_line, card_badges, column_title};
use crate::state::board::nested;
use crate::ui::empty::{Empty, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::skeleton::Skeleton;
use crate::views::task_dialog::TaskDialogMode;
use crate::widgets::status::{FOCUS_RING, Status, StatusDot};

const TH: &str = "px-4 font-medium";
const TD: &str = "px-4";

/// Rows are keyed by task id: a row is never reused for another task, so its handlers may
/// hold the id. Each row reads the latest version of its card, like the kanban cards.
#[component]
pub(super) fn TaskList(board: BoardState) -> impl IntoView {
    let ctx = board.ctx;
    // The open task panel takes 55 % of the width: Branch and Aggiornato make room.
    let narrow = Memo::new(move |_| ctx.open_task.with(Option::is_some));
    // Parents whose sub-tasks are folded.
    let folded = RwSignal::new(HashSet::<Id>::new());
    let rows = Memo::new(move |_| {
        let rows = board.cards.with(|cards| nested(cards));
        folded.with(|folded| {
            rows.into_iter()
                .filter(|c| {
                    c.task
                        .parent_id
                        .as_ref()
                        .is_none_or(|p| !folded.contains(p))
                })
                .collect::<Vec<_>>()
        })
    });

    view! {
        <section class="flex min-h-0 flex-1 flex-col" data-view="task-list">
            // A plain scroller: the sticky header needs the table's own scroll container.
            <div class="min-h-0 flex-1 overflow-y-auto">
                <Show
                    when=move || board.loaded.get()
                    fallback=|| view! { <ListSkeleton /> }
                >
                    <Show
                        when=move || rows.with(|r| !r.is_empty())
                        fallback=|| {
                            view! {
                                <Empty>
                                    <EmptyHeader>
                                        <EmptyTitle>"Nessun task"</EmptyTitle>
                                        <EmptyDescription>
                                            "Crea il primo con «Nuovo task»."
                                        </EmptyDescription>
                                    </EmptyHeader>
                                </Empty>
                            }
                        }
                    >
                        <table
                            class="w-full table-fixed border-collapse text-left text-[13px]"
                            aria-label="Task del progetto"
                        >
                            <thead class="bg-background text-muted-foreground sticky top-0 z-10 text-xs">
                                <tr class="border-border border-y">
                                    <th scope="col" class="h-8 pr-4 pl-5 font-medium">
                                        "Titolo"
                                    </th>
                                    <th scope="col" class=format!("{TH} w-32 xl:w-36")>
                                        "Stato"
                                    </th>
                                    <th scope="col" class=format!("{TH} w-44 xl:w-48")>
                                        "Agente"
                                    </th>
                                    <th
                                        scope="col"
                                        class=format!("{TH} w-36 xl:w-56")
                                        class=("hidden", move || narrow.get())
                                    >
                                        "Branch"
                                    </th>
                                    <th
                                        scope="col"
                                        class="w-28 pr-5 pl-4 text-right font-medium xl:w-32"
                                        class=("hidden", move || narrow.get())
                                    >
                                        "Aggiornato"
                                    </th>
                                </tr>
                            </thead>
                            <tbody>
                                <For each=move || rows.get() key=|c| c.task.id.clone() let:card>
                                    <Row board card narrow folded />
                                </For>
                            </tbody>
                        </table>
                    </Show>
                </Show>
            </div>
        </section>
    }
}

#[component]
fn Row(
    board: BoardState,
    card: TaskCard,
    narrow: Memo<bool>,
    folded: RwSignal<HashSet<Id>>,
) -> impl IntoView {
    let ctx = board.ctx;
    let id = card.task.id.clone();
    let card = {
        let id = id.clone();
        Memo::new(move |_| {
            board
                .cards
                .with(|cards| cards.iter().find(|c| c.task.id == id).cloned())
                .unwrap_or_else(|| card.clone())
        })
    };
    let selected = {
        let id = id.clone();
        Memo::new(move |_| ctx.open_task.with(|t| t.as_ref() == Some(&id)))
    };
    let open = {
        let id = id.clone();
        move |_| ctx.open_task.set(Some(id.clone()))
    };
    let edit = move |_| {
        board.task_dialog.set(Some(TaskDialogMode::Edit(
            card.with_untracked(|c| c.task.clone()),
        )));
    };
    let status = Memo::new(move |_| card.with(|c| c.task.status));
    let cancelled = move || status.get() == TaskStatus::Cancelled;
    let updated = Memo::new(move |_| card.with(|c| c.task.updated_at));
    let child = Memo::new(move |_| card.with(|c| c.task.parent_id.is_some()));
    let progress = Memo::new(move |_| card.with(|c| (c.subtasks_done, c.subtasks_total)));
    let is_folded = {
        let id = id.clone();
        Memo::new(move |_| folded.with(|f| f.contains(&id)))
    };
    let toggle = {
        let id = id.clone();
        move |_| {
            folded.update(|f| {
                if !f.remove(&id) {
                    f.insert(id.clone());
                }
            })
        }
    };

    view! {
        <tr
            class="group border-border hover:bg-muted/60 border-b"
            class=("bg-accent/60", move || selected.get())
            data-row-task-id=id
        >
            <td class="py-2.5 pr-4 pl-5" class=("pl-9", move || child.get())>
                <div class="flex items-center gap-1">
                    <Show when=move || child.get()>
                        <span class="text-muted-foreground shrink-0 font-mono text-[11px]" aria-hidden="true">
                            "↳"
                        </span>
                    </Show>
                    <button
                        type="button"
                        class=format!(
                            "min-w-0 flex-1 truncate rounded-sm text-left font-medium hover:underline {FOCUS_RING}",
                        )
                        class=("line-through", cancelled)
                        class=("text-muted-foreground", cancelled)
                        aria-current=move || selected.get().then_some("true")
                        title=move || card.with(|c| c.task.title.clone())
                        on:click=open
                    >
                        {move || card.with(|c| c.task.title.clone())}
                    </button>
                    <button
                        type="button"
                        class="text-muted-foreground hover:text-foreground rounded-sm p-0.5 opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
                        title="Modifica"
                        aria-label="Modifica il task"
                        on:click=edit
                    >
                        <Pencil class="size-3.5" />
                    </button>
                    // After the title: the title button stays the row's first one.
                    <Show when=move || progress.with(|(_, total)| *total > 0)>
                        <button
                            type="button"
                            class=format!(
                                "hover:bg-muted ml-1 inline-flex shrink-0 items-center gap-1 rounded-sm px-1 {FOCUS_RING}",
                            )
                            aria-expanded=move || (!is_folded.get()).to_string()
                            aria-label=move || {
                                let (done, total) = progress.get();
                                let verb = if is_folded.get() { "Mostra" } else { "Nascondi" };
                                format!("{verb} i sotto task ({done} su {total} fatti)")
                            }
                            data-action="toggle-subtasks"
                            on:click=toggle.clone()
                        >
                            {move || {
                                let (done, total) = progress.get();
                                view! { <SubtaskProgress done total compact=true /> }
                            }}
                            <span
                                class="text-muted-foreground transition-transform"
                                class=("-rotate-90", move || is_folded.get())
                            >
                                <ChevronDown class="size-3.5" />
                            </span>
                        </button>
                    </Show>
                </div>
            </td>
            <td class=TD>
                {move || {
                    let status = status.get();
                    view! {
                        <span class="inline-flex items-center gap-2 whitespace-nowrap">
                            <StatusDot status=Status::of_column(status) />
                            {column_title(status)}
                        </span>
                    }
                }}
            </td>
            <td class=TD>
                <div class="flex flex-wrap items-center gap-1.5 py-1.5">
                    {move || {
                        card.with(|c| match card_badges(ctx, c) {
                            Some(badges) => badges.into_any(),
                            None => view! { <span class="text-muted-foreground">"—"</span> }.into_any(),
                        })
                    }}
                </div>
            </td>
            <td class=format!("{TD} max-w-0") class=("hidden", move || narrow.get())>
                {move || {
                    card.with(|c| match branch_line(c) {
                        Some(branch) => branch.into_any(),
                        None => view! { <span class="text-muted-foreground">"—"</span> }.into_any(),
                    })
                }}
            </td>
            <td
                class="text-muted-foreground pr-5 pl-4 text-right whitespace-nowrap tabular-nums"
                class=("hidden", move || narrow.get())
                title=move || updated_title(updated.get())
            >
                {move || updated_text(updated.get())}
            </td>
        </tr>
    }
}

#[component]
fn ListSkeleton() -> impl IntoView {
    view! {
        <div class="flex flex-col gap-2 px-5 pt-2">
            <Skeleton class="h-8 w-full" />
            <Skeleton class="h-8 w-full" />
            <Skeleton class="h-8 w-full" />
        </div>
    }
}

/// Italian short month names, in `Date::getMonth` order.
const MONTHS: [&str; 12] = [
    "gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic",
];
const DAY_MS: f64 = 86_400_000.0;

/// An instant in the local time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalTime {
    /// Local calendar days since 1970-01-01: equal for two instants of the same local day.
    day: i64,
    year: u32,
    /// 0 = January.
    month: u32,
    date: u32,
    hour: u32,
    minute: u32,
}

impl LocalTime {
    /// WASM only (`js_sys::Date` has no host implementation): the host tests build the fields.
    fn at(ms: Millis) -> Self {
        let date = js_sys::Date::new(&JsValue::from_f64(ms as f64));
        // Minutes to add to local time to get UTC, for that instant (DST included).
        let offset_ms = date.get_timezone_offset() * 60_000.0;
        Self {
            day: ((ms as f64 - offset_ms) / DAY_MS).floor() as i64,
            year: date.get_full_year(),
            month: date.get_month(),
            date: date.get_date(),
            hour: date.get_hours(),
            minute: date.get_minutes(),
        }
    }

    fn now() -> Self {
        Self::at(js_sys::Date::now() as Millis)
    }

    fn month_name(self) -> &'static str {
        MONTHS[self.month as usize % 12]
    }
}

/// "Aggiornato" cell (and the panel's "creato"): `—` for no timestamp (0), else
/// [`short_date`] against now.
pub(crate) fn updated_text(ms: Millis) -> String {
    if ms == 0 {
        return "—".into();
    }
    short_date(LocalTime::at(ms), LocalTime::now())
}

/// Its tooltip: the full date, e.g. "12 set 2025, 14:05".
pub(crate) fn updated_title(ms: Millis) -> Option<String> {
    (ms != 0).then(|| full_date(LocalTime::at(ms)))
}

/// "oggi, 14:05" and "ieri, 09:12", else "12 set" this year and "12 set 2025" before.
fn short_date(t: LocalTime, now: LocalTime) -> String {
    let time = format!("{:02}:{:02}", t.hour, t.minute);
    match now.day - t.day {
        0 => format!("oggi, {time}"),
        1 => format!("ieri, {time}"),
        _ if t.year == now.year => format!("{} {}", t.date, t.month_name()),
        _ => format!("{} {} {}", t.date, t.month_name(), t.year),
    }
}

fn full_date(t: LocalTime) -> String {
    format!(
        "{} {} {}, {:02}:{:02}",
        t.date,
        t.month_name(),
        t.year,
        t.hour,
        t.minute
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: i64, (year, month, date): (u32, u32, u32), (hour, minute): (u32, u32)) -> LocalTime {
        LocalTime {
            day,
            year,
            month,
            date,
            hour,
            minute,
        }
    }

    #[test]
    fn dates_are_relative_to_the_local_day() {
        // 2026-09-29 is local day 20_725.
        let now = at(20_725, (2026, 8, 29), (10, 0));
        let today = at(20_725, (2026, 8, 29), (0, 5));
        assert_eq!(short_date(today, now), "oggi, 00:05");
        let yesterday = at(20_724, (2026, 8, 28), (23, 59));
        assert_eq!(short_date(yesterday, now), "ieri, 23:59");
        let this_year = at(20_455, (2026, 0, 2), (8, 30));
        assert_eq!(short_date(this_year, now), "2 gen");
        let last_year = at(20_343, (2025, 8, 12), (14, 5));
        assert_eq!(short_date(last_year, now), "12 set 2025");
        assert_eq!(full_date(last_year), "12 set 2025, 14:05");
        // A clock a little ahead: tomorrow is not "oggi".
        let ahead = at(20_726, (2026, 8, 30), (1, 0));
        assert_eq!(short_date(ahead, now), "30 set");
    }
}
