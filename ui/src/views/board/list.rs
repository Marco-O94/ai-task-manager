//! List view of the board (spec F4): the same cards as the columns, as one semantic table in
//! column order then position. No search, filter or sort yet. Owner: UI-TASKS.

use atm_types::{Millis, TaskCard};
use icons::Pencil;
use leptos::prelude::*;
use wasm_bindgen::JsValue;

use super::{BoardState, branch_line, card_badges, column_title};
use crate::state::board::by_column;
use crate::ui::empty::{Empty, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::scroll_area::ScrollArea;
use crate::ui::skeleton::Skeleton;
use crate::views::task_dialog::TaskDialogMode;

const TH: &str = "px-2 py-2 text-left font-medium";
const TD: &str = "px-2 py-2 align-top";

/// Rows are keyed by task id: a row is never reused for another task, so its handlers may
/// hold the id. Each row reads the latest version of its card, like the kanban cards.
#[component]
pub(super) fn TaskList(board: BoardState) -> impl IntoView {
    let ctx = board.ctx;
    // The open task panel takes 55 % of the width: Branch and Aggiornato make room.
    let narrow = Memo::new(move |_| ctx.open_task.with(Option::is_some));
    let rows = Memo::new(move |_| board.cards.with(|cards| by_column(cards)));

    view! {
        <section class="flex min-h-0 flex-1 flex-col" data-view="task-list">
            <ScrollArea class="min-h-0 flex-1">
                <div class="px-4 pt-3 pb-4">
                    <Show when=move || board.loaded.get() fallback=|| view! { <ListSkeleton /> }>
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
                            <table class="w-full text-sm" aria-label="Task del progetto">
                                <thead class="text-muted-foreground border-b text-xs">
                                    <tr>
                                        <th scope="col" class=TH>
                                            "Titolo"
                                        </th>
                                        <th scope="col" class=format!("{TH} w-28")>
                                            "Stato"
                                        </th>
                                        <th scope="col" class=format!("{TH} w-44")>
                                            "Agente"
                                        </th>
                                        <th
                                            scope="col"
                                            class=format!("{TH} w-44")
                                            class=("hidden", move || narrow.get())
                                        >
                                            "Branch"
                                        </th>
                                        <th
                                            scope="col"
                                            class=format!("{TH} w-32")
                                            class=("hidden", move || narrow.get())
                                        >
                                            "Aggiornato"
                                        </th>
                                    </tr>
                                </thead>
                                <tbody>
                                    <For each=move || rows.get() key=|c| c.task.id.clone() let:card>
                                        <Row board card narrow />
                                    </For>
                                </tbody>
                            </table>
                        </Show>
                    </Show>
                </div>
            </ScrollArea>
        </section>
    }
}

#[component]
fn Row(board: BoardState, card: TaskCard, narrow: Memo<bool>) -> impl IntoView {
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
    let updated = Memo::new(move |_| card.with(|c| c.task.updated_at));

    view! {
        <tr
            class="group hover:bg-muted/50 border-b last:border-b-0"
            class=("bg-muted/60", move || selected.get())
            data-row-task-id=id
        >
            <td class=TD>
                <div class="flex items-start gap-1">
                    <button
                        type="button"
                        class="focus-visible:ring-ring/50 min-w-0 flex-1 rounded-sm text-left font-medium break-words outline-none hover:underline focus-visible:ring-[3px]"
                        aria-current=move || selected.get().then_some("true")
                        on:click=open
                    >
                        {move || card.with(|c| c.task.title.clone())}
                    </button>
                    <button
                        type="button"
                        class="text-muted-foreground hover:text-foreground rounded p-0.5 opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
                        title="Modifica"
                        aria-label="Modifica il task"
                        on:click=edit
                    >
                        <Pencil class="size-3.5" />
                    </button>
                </div>
                {move || {
                    card.with(|c| first_line(&c.task.description).map(str::to_owned))
                        .map(|line| {
                            view! { <p class="text-muted-foreground mt-0.5 line-clamp-1 text-xs">{line}</p> }
                        })
                }}
            </td>
            <td class=format!("{TD} text-muted-foreground whitespace-nowrap")>
                {move || card.with(|c| column_title(c.task.status))}
            </td>
            <td class=TD>
                {move || {
                    card.with(|c| match card_badges(ctx, c) {
                        Some(badges) => badges.into_any(),
                        None => view! { <span class="text-muted-foreground">"—"</span> }.into_any(),
                    })
                }}
            </td>
            <td class=TD class=("hidden", move || narrow.get())>
                // A cell's own max-width does not bound an auto table layout: the div does.
                <div class="max-w-44">
                    {move || {
                        card.with(|c| match branch_line(c) {
                            Some(branch) => branch.into_any(),
                            None => view! { <span class="text-muted-foreground">"—"</span> }.into_any(),
                        })
                    }}
                </div>
            </td>
            <td
                class=format!("{TD} text-muted-foreground whitespace-nowrap")
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
        <div class="flex flex-col gap-2">
            <Skeleton class="h-8 w-full" />
            <Skeleton class="h-8 w-full" />
            <Skeleton class="h-8 w-full" />
        </div>
    }
}

/// First non-blank line of a description, trimmed: the muted line under the title.
fn first_line(description: &str) -> Option<&str> {
    description.lines().map(str::trim).find(|l| !l.is_empty())
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

/// "Aggiornato" cell: `—` for no timestamp (0), else [`short_date`] against now.
fn updated_text(ms: Millis) -> String {
    if ms == 0 {
        return "—".into();
    }
    short_date(LocalTime::at(ms), LocalTime::now())
}

/// Its tooltip: the full date, e.g. "12 set 2025, 14:05".
fn updated_title(ms: Millis) -> Option<String> {
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

    #[test]
    fn the_description_line_is_the_first_non_blank_one() {
        assert_eq!(
            first_line("  \n\n  Primo passo \nsecondo"),
            Some("Primo passo")
        );
        assert_eq!(first_line("Una riga"), Some("Una riga"));
        assert_eq!(first_line(" \n\t\n"), None);
        assert_eq!(first_line(""), None);
    }
}
