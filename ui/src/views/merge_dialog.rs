//! Squash-merge dialog (spec §8.7): editable message, outcome (Merged, NothingToMerge,
//! Conflicts, TargetCheckoutDirty). Also the discard confirmation. Owner: M2-UI-TASK.

use atm_types::{
    AppError, AttemptIdReq, DiscardAttempt, ErrorCode, Id, MergeAttempt, MergeAttemptReq,
    MergeOutcome, MergeStrategy, Task, merge_message,
};
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::app::use_app;
use crate::ipc;
use crate::ui::alert::{Alert, AlertDescription, AlertTitle};
use crate::ui::button::{Button, ButtonVariant};
use crate::ui::dialog::{
    Dialog, DialogBody, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader,
    DialogTitle,
};
use crate::ui::label::Label;
use crate::ui::spinner::Spinner;
use crate::ui::textarea::Textarea;
use crate::views::diff::{DiffCtx, resolve_with_agent};

/// Outcome shown in the dialog; `Merged` closes it instead.
#[derive(Clone)]
enum Blocked {
    Conflicts(Vec<String>),
    NothingToMerge,
    Failed(AppError),
}

/// The message starts as `atm_types::merge_message(title, description, attempt_id)`.
#[component]
pub fn MergeDialog(
    open: RwSignal<bool>,
    attempt_id: Id,
    #[prop(into)] task: Signal<Task>,
) -> impl IntoView {
    let ctx = use_app();
    let diff = use_context::<DiffCtx>();
    let attempt_id = StoredValue::new(attempt_id);
    let message = RwSignal::new(String::new());
    let blocked = RwSignal::new(None::<Blocked>);
    let busy = RwSignal::new(false);
    let target = move || {
        diff.and_then(|d| {
            d.status
                .with(|s| s.as_ref().map(|s| s.target_branch.clone()))
        })
        .unwrap_or_else(|| "il branch target".into())
    };

    // A fresh default message and no outcome every time the dialog opens.
    Effect::new(move |was_open: Option<bool>| {
        let is_open = open.get();
        if is_open && was_open != Some(true) {
            let default = task.with_untracked(|t| {
                merge_message(&t.title, &t.description, &attempt_id.get_value())
            });
            message.set(default);
            blocked.set(None);
        }
        is_open
    });

    let merge = move |_| {
        busy.set(true);
        blocked.set(None);
        let req = MergeAttemptReq {
            attempt_id: attempt_id.get_value(),
            message: message.get_untracked(),
        };
        spawn_local(async move {
            let res = ipc::call::<MergeAttempt>(&req).await;
            busy.try_set(false);
            let outcome = match res {
                Ok(MergeOutcome::Merged {
                    commit,
                    strategy,
                    cleanup_warning,
                }) => {
                    let how = match strategy {
                        MergeStrategy::UpdateRef => "",
                        MergeStrategy::FfCheckedOut => " (fast-forward del checkout)",
                    };
                    ctx.toasts.success(format!(
                        "Merge completato: {}{how}.",
                        &commit[..commit.len().min(10)]
                    ));
                    if let Some(w) = cleanup_warning {
                        ctx.toasts.error(format!("Worktree non rimosso: {w}"));
                    }
                    open.try_set(false);
                    None
                }
                Ok(MergeOutcome::NothingToMerge) => Some(Blocked::NothingToMerge),
                Ok(MergeOutcome::Conflicts { files }) => Some(Blocked::Conflicts(files)),
                Err(e) => Some(Blocked::Failed(e)),
            };
            // After a merge the `changed` event swaps the panel to the closed attempt, whose
            // worktree may be gone: refetch only for the outcomes that leave it open.
            if outcome.is_some()
                && let Some(d) = diff
            {
                d.refresh.try_update(|n| *n += 1);
            }
            blocked.try_set(outcome);
        });
    };

    let outcome = move || {
        blocked.get().map(|b| match b {
            Blocked::Conflicts(files) => {
                let list = files.join(", ");
                let resolve = move |_| {
                    resolve_with_agent(ctx, attempt_id.get_value(), &untrack(target), &files);
                    open.set(false);
                };
                view! {
                    <Alert class="border-destructive/50 text-destructive" attr:data-outcome="conflicts">
                        <AlertTitle>{format!("Conflitti con {}", target())}</AlertTitle>
                        <AlertDescription>
                            <p>"Il merge non è stato eseguito e nessun ref è stato toccato."</p>
                            <p class="font-mono text-xs">{list}</p>
                        </AlertDescription>
                        <Button variant=ButtonVariant::Outline class="mt-3" on:click=resolve>
                            "Risolvi con l'agente"
                        </Button>
                    </Alert>
                }
                .into_any()
            }
            Blocked::NothingToMerge => view! {
                <Alert attr:data-outcome="nothing">
                    <AlertTitle>"Niente da mergiare"</AlertTitle>
                    <AlertDescription>
                        {format!("Il branch non introduce modifiche rispetto a {}.", target())}
                    </AlertDescription>
                </Alert>
            }
            .into_any(),
            Blocked::Failed(e) if e.code == ErrorCode::TargetCheckoutDirty => view! {
                <Alert
                    class="border-warning/60 bg-warning-light/40 dark:bg-warning-dark/20"
                    attr:data-outcome="target-dirty"
                >
                    <AlertTitle>{format!("Il checkout di {} ha modifiche locali", target())}</AlertTitle>
                    <AlertDescription>
                        <p>
                            "Le modifiche non committate nel checkout si sovrappongono al merge: Git non ha toccato nulla. Committale o annullale nel checkout, poi riprova."
                        </p>
                        <p class="text-muted-foreground text-xs">{e.message}</p>
                    </AlertDescription>
                </Alert>
            }
            .into_any(),
            Blocked::Failed(e) => view! {
                <Alert class="border-destructive/50 text-destructive" attr:data-outcome="error">
                    <AlertTitle>"Merge non riuscito"</AlertTitle>
                    <AlertDescription>{e.to_string()}</AlertDescription>
                </Alert>
            }
            .into_any(),
        })
    };

    view! {
        <Dialog open>
            <DialogContent class="sm:max-w-2xl" data_name_prefix="MergeDialog">
                <DialogBody>
                    <DialogHeader>
                        <DialogTitle>{move || format!("Squash merge in {}", target())}</DialogTitle>
                        <DialogDescription>
                            "Un solo commit con tutto il lavoro del tentativo. Il messaggio è modificabile."
                        </DialogDescription>
                    </DialogHeader>
                    <div class="flex flex-col gap-2">
                        <Label html_for="merge-message">"Messaggio del commit"</Label>
                        <Textarea
                            id="merge-message"
                            bind_value=message
                            rows=8u32
                            class="font-mono text-xs"
                        />
                    </div>
                    {outcome}
                    <DialogFooter>
                        <DialogClose>"Annulla"</DialogClose>
                        <Button
                            attr:data-action="confirm-merge"
                            attr:disabled=move || busy.get() || message.with(|m| m.trim().is_empty())
                            on:click=merge
                        >
                            {move || busy.get().then(|| view! { <Spinner /> })}
                            "Esegui merge"
                        </Button>
                    </DialogFooter>
                </DialogBody>
            </DialogContent>
        </Dialog>
    }
}

/// "Scarta" confirmation: stops the turn, snapshot commit, removes the worktree; the branch
/// stays (spec §5.4).
#[component]
pub fn DiscardDialog(open: RwSignal<bool>, attempt_id: Id, branch: String) -> impl IntoView {
    let ctx = use_app();
    let attempt_id = StoredValue::new(attempt_id);
    let busy = RwSignal::new(false);
    let discard = move |_| {
        busy.set(true);
        let req = AttemptIdReq {
            attempt_id: attempt_id.get_value(),
        };
        spawn_local(async move {
            match ipc::call::<DiscardAttempt>(&req).await {
                Ok(()) => {
                    ctx.toasts.info("Tentativo scartato.");
                    open.try_set(false);
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
            busy.try_set(false);
        });
    };
    view! {
        <Dialog open>
            <DialogContent class="sm:max-w-md" data_name_prefix="DiscardDialog">
                <DialogBody>
                    <DialogHeader>
                        <DialogTitle>"Scartare il tentativo?"</DialogTitle>
                        <DialogDescription>
                            {format!(
                                "Il turno in corso viene fermato, il lavoro viene salvato con un commit su {branch} e il worktree viene rimosso. Il branch resta nel repository.",
                            )}
                        </DialogDescription>
                    </DialogHeader>
                    <DialogFooter>
                        <DialogClose>"Annulla"</DialogClose>
                        <Button
                            variant=ButtonVariant::Destructive
                            attr:data-action="confirm-discard"
                            attr:disabled=move || busy.get()
                            on:click=discard
                        >
                            "Scarta"
                        </Button>
                    </DialogFooter>
                </DialogBody>
            </DialogContent>
        </Dialog>
    }
}
