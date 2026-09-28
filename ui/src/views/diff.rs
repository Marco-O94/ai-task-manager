//! Modifiche tab (spec §8.6, §9.2): branch status alerts, per-file diff viewer, Aggiorna,
//! Merge, "Risolvi con l'agente", Elimina branch. Owner: M2-UI-TASK.

use atm_types::{
    AppError, AttemptIdReq, AttemptState, AttemptView, BranchStatus, DeleteBranch, DiffLine,
    DiffResult, FileDiff, FileStatus, GetBranchStatus, GetDiff, Id, LineKind, SendFollowUp,
    SendFollowUpReq, Task,
};
use std::collections::HashMap;

use icons::{ChevronRight, GitBranch, GitMerge, RefreshCw, Trash2, Wrench};
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::ui::alert::{Alert, AlertDescription, AlertTitle};
use crate::ui::badge::{Badge, BadgeVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::collapsible::{Collapsible, CollapsibleContent, CollapsibleTrigger};
use crate::ui::empty::{Empty, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::skeleton::Skeleton;
use crate::views::merge_dialog::MergeDialog;
use crate::views::transcript::ON_DESTRUCTIVE;

/// Lines of a file shown before "Mostra tutto" (spec §9.2).
const MAX_LINES: usize = 2_000;
/// Files that start expanded (the first ones of a diff).
const OPEN_FILES: usize = 20;

/// Provided by [`DiffView`] to its merge dialog: the latest branch status and a refresh.
#[derive(Clone, Copy)]
pub struct DiffCtx {
    pub status: RwSignal<Option<BranchStatus>>,
    /// Bump to refetch the diff and the branch status.
    pub refresh: RwSignal<u64>,
}

/// Expanded and "Mostra tutto" of a file, kept by path across refetches. `Arc` signals, so
/// that they outlive the file views rebuilt with each new diff.
#[derive(Clone)]
struct FileUi {
    open: ArcRwSignal<bool>,
    show_all: ArcRwSignal<bool>,
}

impl FileUi {
    fn new(open: bool) -> Self {
        Self {
            open: ArcRwSignal::new(open),
            show_all: ArcRwSignal::new(false),
        }
    }
}

/// Keeps the state of the files still in the diff; the first [`OPEN_FILES`] new ones start open.
fn reconcile(ui: &mut HashMap<String, FileUi>, files: &[FileDiff]) {
    let mut old = std::mem::take(ui);
    for (i, f) in files.iter().enumerate() {
        let state = old
            .remove(&f.path)
            .unwrap_or_else(|| FileUi::new(i < OPEN_FILES));
        ui.insert(f.path.clone(), state);
    }
}

/// Follow-up of "Risolvi con l'agente" (spec §8.7).
fn conflict_prompt(target: &str, files: &[String]) -> String {
    format!(
        "This branch conflicts with `{target}` in: {}. Run `git merge {target}`, resolve every \
         conflict preserving both intents, run the project's tests if available, and commit the \
         merge. Do not push.",
        files.join(", ")
    )
}

/// Sends the "Risolvi con l'agente" follow-up.
pub fn resolve_with_agent(ctx: AppCtx, attempt_id: Id, target: &str, files: &[String]) {
    let req = SendFollowUpReq {
        attempt_id,
        prompt: conflict_prompt(target, files),
        permission_mode: None,
        fresh_session: false,
    };
    spawn_local(async move {
        match ipc::call::<SendFollowUp>(&req).await {
            Ok(_) => ctx
                .toasts
                .info("Conflitti affidati all'agente: segui il turno nel tab Agente."),
            Err(e) => ctx.toasts.app_error(&e),
        }
    });
}

/// Fetches `get_diff` and `get_branch_status` on open, on `changed` of the task while
/// visible, and on "Aggiorna". `task` feeds the merge dialog's default message.
#[component]
pub fn DiffView(attempt_id: Id, #[prop(into)] task: Signal<Task>) -> impl IntoView {
    let ctx = use_app();
    let diff = RwSignal::new(None::<Result<DiffResult, AppError>>);
    let files_ui = StoredValue::new(HashMap::<String, FileUi>::new());
    let status = RwSignal::new(None::<BranchStatus>);
    // Inline rather than a toast: it would repeat on every `changed` (e.g. a missing worktree).
    let status_error = RwSignal::new(None::<AppError>);
    let refresh = RwSignal::new(0u64);
    let loading = RwSignal::new(false);
    let merge_open = RwSignal::new(false);
    let fetches = StoredValue::new(0u64);
    provide_context(DiffCtx { status, refresh });
    let id = StoredValue::new(attempt_id.clone());

    // Mounted only while the tab is visible: mount = "on open".
    Effect::new(move |_| {
        refresh.track();
        ctx.detail_version.track();
        let seq = fetches
            .try_update_value(|n| {
                *n += 1;
                *n
            })
            .unwrap_or_default();
        let current = move || fetches.try_get_value() == Some(seq);
        let req = AttemptIdReq {
            attempt_id: id.get_value(),
        };
        loading.set(true);
        let status_req = req.clone();
        spawn_local(async move {
            let res = ipc::call::<GetDiff>(&req).await;
            if !current() {
                return;
            }
            loading.try_set(false);
            // An unchanged diff (e.g. an approval during a turn) keeps the rendered files.
            let unchanged = diff.try_with_untracked(
                |old| matches!((old, &res), (Some(Ok(old)), Ok(new)) if old == new),
            );
            if unchanged != Some(false) {
                return;
            }
            if let Ok(d) = &res {
                files_ui.try_update_value(|ui| reconcile(ui, &d.files));
            }
            diff.try_set(Some(res));
        });
        spawn_local(async move {
            let res = ipc::call::<GetBranchStatus>(&status_req).await;
            if !current() {
                return;
            }
            // No status disables Merge.
            let (ok, err) = match res {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e)),
            };
            status.try_set(ok);
            status_error.try_set(err);
        });
    });

    let summary = move || {
        diff.with(|d| match d {
            Some(Ok(d)) => format!("{} file · +{} −{}", d.files.len(), d.additions, d.deletions),
            _ => String::new(),
        })
    };
    let conflicts = move || status.with(|s| s.as_ref().is_some_and(|s| !s.conflicts.is_empty()));
    let resolve = move |_| {
        if let Some(s) = status.get_untracked() {
            resolve_with_agent(ctx, id.get_value(), &s.target_branch, &s.conflicts);
        }
    };

    view! {
        <div class="flex flex-col gap-3 p-4" data-view="diff">
            <div class="flex flex-wrap items-center gap-2">
                <span class="text-sm font-medium" data-diff-summary="">{summary}</span>
                <div class="ml-auto flex flex-wrap gap-2">
                    <Show when=conflicts>
                        <Button variant=ButtonVariant::Outline size=ButtonSize::Sm on:click=resolve>
                            <Wrench />
                            "Risolvi con l'agente"
                        </Button>
                    </Show>
                    <Button
                        variant=ButtonVariant::Outline
                        size=ButtonSize::Sm
                        attr:disabled=move || loading.get()
                        on:click=move |_| refresh.update(|n| *n += 1)
                    >
                        <span class=move || if loading.get() { "animate-spin" } else { "" }>
                            <RefreshCw class="size-4" />
                        </span>
                        "Aggiorna"
                    </Button>
                    <Button
                        size=ButtonSize::Sm
                        attr:data-action="merge"
                        attr:disabled=move || {
                            status.with(|s| s.as_ref().is_none_or(|s| s.merge_blocked.is_some()))
                        }
                        on:click=move |_| merge_open.set(true)
                    >
                        <GitMerge />
                        "Merge"
                    </Button>
                </div>
            </div>
            {move || status.with(|s| s.as_ref().map(status_alerts))}
            {move || {
                status_error
                    .get()
                    .map(|e| {
                        view! {
                            <Alert class="border-destructive/50 text-destructive">
                                <AlertTitle>"Stato del branch non disponibile"</AlertTitle>
                                <AlertDescription>{e.to_string()}</AlertDescription>
                            </Alert>
                        }
                    })
            }}
            {move || diff.with(|d| files_view(d.as_ref(), files_ui))}
            <MergeDialog open=merge_open attempt_id task />
        </div>
    }
}

fn status_alerts(s: &BranchStatus) -> AnyView {
    let target = &s.target_branch;
    let mut alerts: Vec<(bool, String, String)> = Vec::new();
    if let Some(reason) = &s.merge_blocked {
        alerts.push((false, "Merge non disponibile".into(), reason.clone()));
    }
    if !s.head_ok {
        alerts.push((
            true,
            "HEAD non corretto".into(),
            "Il worktree non è sul branch del tentativo: il merge è bloccato finché non torna \
             sul branch."
                .into(),
        ));
    }
    if !s.conflicts.is_empty() {
        alerts.push((
            true,
            format!("Conflitti con {target}"),
            format!(
                "Il merge andrebbe in conflitto in: {}.",
                s.conflicts.join(", ")
            ),
        ));
    }
    if s.behind > 0 {
        alerts.push((
            false,
            format!("{target} è avanti di {} commit", s.behind),
            "Il merge squash parte dalla punta attuale del target, senza rebase.".into(),
        ));
    }
    if let Some(path) = &s.target_checked_out_at {
        alerts.push((
            false,
            format!("{target} è in checkout"),
            format!(
                "Il merge aggiornerà {path} con un fast-forward e fallirà se il checkout ha \
                 modifiche locali che si sovrappongono."
            ),
        ));
    }
    if s.dirty {
        alerts.push((
            false,
            "Modifiche non committate".into(),
            "Il worktree ha modifiche non committate: finiscono nel commit automatico prima del \
             merge."
                .into(),
        ));
    }
    alerts
        .into_iter()
        .map(|(error, title, text)| {
            let class = if error {
                "border-destructive/50 text-destructive"
            } else {
                "border-warning/60 bg-warning-light/40 dark:bg-warning-dark/20"
            };
            view! {
                <Alert class=class>
                    <AlertTitle>{title}</AlertTitle>
                    <AlertDescription>{text}</AlertDescription>
                </Alert>
            }
        })
        .collect_view()
        .into_any()
}

fn files_view(
    diff: Option<&Result<DiffResult, AppError>>,
    files_ui: StoredValue<HashMap<String, FileUi>>,
) -> AnyView {
    match diff {
        None => view! {
            <div class="flex flex-col gap-2">
                <Skeleton class="h-9 w-full" />
                <Skeleton class="h-40 w-full" />
                <Skeleton class="h-9 w-full" />
            </div>
        }
        .into_any(),
        Some(Err(e)) => {
            let text = e.to_string();
            view! {
                <Alert class="border-destructive/50 text-destructive">
                    <AlertTitle>"Diff non disponibile"</AlertTitle>
                    <AlertDescription>{text}</AlertDescription>
                </Alert>
            }
            .into_any()
        }
        Some(Ok(d)) if d.files.is_empty() => view! {
            <Empty>
                <EmptyHeader>
                    <EmptyTitle>"Nessuna modifica"</EmptyTitle>
                    <EmptyDescription>"Il branch non cambia nulla rispetto al target."</EmptyDescription>
                </EmptyHeader>
            </Empty>
        }
        .into_any(),
        Some(Ok(d)) => {
            let truncated = d.truncated.then(|| {
                view! {
                    <Alert>
                        <AlertDescription>
                            "Diff oltre il limite: alcuni file mostrano solo le statistiche."
                        </AlertDescription>
                    </Alert>
                }
            });
            let files = d
                .files
                .iter()
                .map(|f| {
                    let ui = files_ui
                        .try_with_value(|ui| ui.get(&f.path).cloned())
                        .flatten()
                        .unwrap_or_else(|| FileUi::new(true));
                    view! { <FileBlock file=f.clone() ui /> }
                })
                .collect_view();
            view! {
                {truncated}
                <div class="flex flex-col gap-2">{files}</div>
            }
            .into_any()
        }
    }
}

#[component]
fn FileBlock(file: FileDiff, ui: FileUi) -> impl IntoView {
    let placeholder = if file.binary {
        Some("File binario: contenuto non mostrato.")
    } else if file.too_large {
        Some("Diff troppo grande (oltre 256 KiB): solo statistiche.")
    } else if file.omitted {
        Some("Omesso: il diff supera il limite complessivo (4 MiB o 300 file).")
    } else {
        None
    };
    let open = RwSignal::from(ui.open);
    let show_all = RwSignal::from(ui.show_all);
    let (letter, label, variant) = status_badge(file.status);
    let path = match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    };
    let total = file.lines.len();
    let lines = StoredValue::new(file.lines);
    let body = move || {
        if let Some(text) = placeholder {
            return view! { <p class="text-muted-foreground px-3 py-2 text-xs italic">{text}</p> }
                .into_any();
        }
        let limit = if show_all.get() { total } else { MAX_LINES };
        let rows = lines.with_value(|ls| ls.iter().take(limit).map(line_view).collect_view());
        let more = (total > limit).then(|| {
            view! {
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::Sm
                    class="m-2"
                    on:click=move |_| show_all.set(true)
                >
                    {format!("Mostra tutto ({total} righe)")}
                </Button>
            }
        });
        view! {
            <div class="overflow-x-auto">
                <table class="w-full border-collapse font-mono text-xs">
                    <tbody>{rows}</tbody>
                </table>
            </div>
            {more}
        }
        .into_any()
    };
    view! {
        <div class="rounded-lg border" data-file=file.path.clone()>
            <Collapsible open>
                <CollapsibleTrigger class="hover:bg-accent/50 flex w-full min-w-0 items-center gap-2 px-3 py-2 text-left text-sm">
                    <span class=move || {
                        tw_merge!("shrink-0 transition-transform", if open.get() { "rotate-90" } else { "" })
                    }>
                        <ChevronRight class="size-4" />
                    </span>
                    <span title=label>
                        <Badge
                            variant
                            size=crate::ui::badge::BadgeSize::Sm
                            class=if file.status == FileStatus::Deleted { ON_DESTRUCTIVE } else { "" }
                        >
                            {letter}
                        </Badge>
                    </span>
                    <span class="min-w-0 flex-1 truncate font-mono text-xs" title=path.clone()>
                        {path.clone()}
                    </span>
                    {placeholder.map(|_| {
                        let tag = if file.binary {
                            "binario"
                        } else if file.too_large {
                            "troppo grande"
                        } else {
                            "omesso"
                        };
                        view! { <Badge variant=BadgeVariant::Muted>{tag}</Badge> }
                    })}
                    <span class="text-success shrink-0 font-mono text-xs">
                        {format!("+{}", file.additions)}
                    </span>
                    <span class="text-destructive shrink-0 font-mono text-xs">
                        {format!("−{}", file.deletions)}
                    </span>
                </CollapsibleTrigger>
                <CollapsibleContent class="border-t">{move || open.get().then(body)}</CollapsibleContent>
            </Collapsible>
        </div>
    }
}

fn status_badge(status: FileStatus) -> (&'static str, &'static str, BadgeVariant) {
    match status {
        FileStatus::Added => ("A", "Aggiunto", BadgeVariant::Success),
        FileStatus::Modified => ("M", "Modificato", BadgeVariant::Info),
        FileStatus::Deleted => ("D", "Eliminato", BadgeVariant::Destructive),
        FileStatus::Renamed => ("R", "Rinominato", BadgeVariant::Warning),
        FileStatus::Copied => ("C", "Copiato", BadgeVariant::Secondary),
        FileStatus::TypeChanged => ("T", "Tipo cambiato", BadgeVariant::Muted),
    }
}

const NUM_CLASS: &str = "text-muted-foreground w-12 border-r px-2 text-right align-top select-none";

fn line_view(line: &DiffLine) -> impl IntoView + use<> {
    let (class, sign, kind) = match line.kind {
        LineKind::Add => ("bg-success/10", "+", "add"),
        LineKind::Del => ("bg-destructive/10", "-", "del"),
        LineKind::Hunk => ("bg-info/10 text-muted-foreground", "", "hunk"),
        LineKind::Meta => ("text-muted-foreground italic", "", "meta"),
        LineKind::Context => ("", " ", "context"),
    };
    view! {
        <tr class=class data-line=kind>
            <td class=NUM_CLASS>{line.old_no}</td>
            <td class=NUM_CLASS>{line.new_no}</td>
            <td class="text-muted-foreground w-4 pl-2 align-top select-none">{sign}</td>
            <td class="pr-2 pl-1 whitespace-pre">{line.text.clone()}</td>
        </tr>
    }
}

/// Modifiche tab of a closed attempt: where it went, and "Elimina branch" once merged.
#[component]
pub fn ClosedAttempt(attempt: AttemptView) -> impl IntoView {
    let ctx = use_app();
    let deleting = RwSignal::new(false);
    let merged = attempt.state == AttemptState::Merged;
    let text = match (&attempt.state, &attempt.merge_commit) {
        (AttemptState::Merged, Some(commit)) => format!(
            "Mergiato in {} con il commit {}.",
            attempt.target_branch,
            &commit[..commit.len().min(10)]
        ),
        (AttemptState::Merged, None) => format!("Mergiato in {}.", attempt.target_branch),
        _ => "Tentativo scartato: il lavoro resta salvato sul suo branch.".into(),
    };
    let id = StoredValue::new(attempt.id);
    let delete = move |_| {
        deleting.set(true);
        let req = AttemptIdReq {
            attempt_id: id.get_value(),
        };
        spawn_local(async move {
            match ipc::call::<DeleteBranch>(&req).await {
                Ok(()) => ctx.toasts.success("Branch eliminato."),
                Err(e) => ctx.toasts.app_error(&e),
            }
            deleting.try_set(false);
        });
    };
    view! {
        <div class="flex flex-col gap-3 p-4" data-view="closed-attempt">
            <Alert>
                <AlertTitle>{text}</AlertTitle>
                <AlertDescription>
                    <span class="inline-flex items-center gap-1 font-mono text-xs">
                        <GitBranch class="size-3" />
                        {attempt.branch}
                    </span>
                </AlertDescription>
            </Alert>
            {merged
                .then(|| {
                    view! {
                        <Button
                            variant=ButtonVariant::Outline
                            size=ButtonSize::Sm
                            class="self-start"
                            attr:disabled=move || deleting.get()
                            on:click=delete
                        >
                            <Trash2 />
                            "Elimina branch"
                        </Button>
                    }
                })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            additions: 1,
            deletions: 0,
            binary: false,
            too_large: false,
            omitted: false,
            lines: Vec::new(),
        }
    }

    #[test]
    fn reconcile_keeps_the_state_of_the_files_still_in_the_diff() {
        let mut ui = HashMap::new();
        reconcile(&mut ui, &[file("a"), file("b")]);
        ui["a"].open.set(false);
        ui["b"].show_all.set(true);

        let files: Vec<_> = (0..=OPEN_FILES).map(|i| file(&format!("n{i}"))).collect();
        reconcile(&mut ui, &[&[file("b"), file("a")], &files[..]].concat());
        assert!(!ui["a"].open.get_untracked());
        assert!(ui["b"].show_all.get_untracked());
        // New files open by position; the ones past [`OPEN_FILES`] start collapsed.
        assert!(ui["n0"].open.get_untracked());
        assert!(!ui[&format!("n{}", OPEN_FILES - 2)].open.get_untracked());

        reconcile(&mut ui, &[file("b")]);
        assert_eq!(ui.len(), 1);
    }
}
