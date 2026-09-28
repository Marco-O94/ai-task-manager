//! Agente tab: live transcript list (spec §9.4) with subagent nesting, typing preview,
//! "Carica precedenti", autoscroll and "Vai all'ultimo". Owner: M2-UI-TASK.

use atm_types::{
    Entry, EntryBody, GetEntries, GetEntriesReq, Id, Level, LimitKind, NoticeAction, SendFollowUp,
    SendFollowUpReq, ToolOutput, ToolStatus, UnsubscribeTranscript, UnsubscribeTranscriptReq,
};
use icons::{ArrowDown, Ban, Brain, Check, ChevronRight, CircleStop, Hourglass, SquareTerminal, X};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use tw_merge::tw_merge;
use wasm_bindgen::prelude::*;

use crate::app::{AppCtx, use_app};
use crate::ipc;
use crate::state::transcript::{Row, TranscriptStore};
use crate::ui::alert::{Alert, AlertDescription, AlertTitle};
use crate::ui::badge::{Badge, BadgeVariant};
use crate::ui::bubble::{Bubble, BubbleAlign, BubbleContent, BubbleVariant};
use crate::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::ui::collapsible::{Collapsible, CollapsibleContent, CollapsibleTrigger};
use crate::ui::empty::{Empty, EmptyDescription, EmptyHeader, EmptyTitle};
use crate::ui::marker::{Marker, MarkerContent, MarkerVariant};
use crate::ui::message::{Message, MessageAlign, MessageContent};
use crate::ui::skeleton::Skeleton;
use crate::ui::spinner::Spinner;
use crate::views::approval::ApprovalCard;

/// "In fondo" = less than this many pixels from the bottom (spec §9.4).
const BOTTOM_SLACK_PX: i32 = 48;
/// Entries per "Carica precedenti".
const PAGE: u32 = 100;
/// Frames during which "Carica precedenti" holds its scroll anchor.
const ANCHOR_FRAMES: u32 = 12;
/// Characters of the typing preview kept on screen (the buffer is a whole content block).
const TYPING_TAIL: usize = 2_000;
/// Prompt of "Nuova sessione" after a failed resume; the backend prefixes the task and the
/// branch history (`fresh_session`, spec §7.9).
const NEW_SESSION_PROMPT: &str = "Continue the task from where the previous session stopped.";

type Subscription = (Id, ipc::Channel, Closure<dyn FnMut(JsValue)>);
type Observer = (web_sys::ResizeObserver, Closure<dyn FnMut(JsValue)>);

/// Subscribes on mount (`ipc::subscribe_transcript`), unsubscribes on cleanup.
#[component]
pub fn Transcript(attempt_id: Id) -> impl IntoView {
    let ctx = use_app();
    let attempt_id = StoredValue::new(attempt_id);
    let store = TranscriptStore::new();
    let loaded = RwSignal::new(false);
    let loading_older = RwSignal::new(false);
    // Bumped to resubscribe: the new `Snapshot` brings back a hidden tail.
    let generation = RwSignal::new(0u32);
    let sub = StoredValue::new_local(None::<Subscription>);
    let observer = StoredValue::new_local(None::<Observer>);
    let scroller = NodeRef::<html::Div>::new();
    let content = NodeRef::<html::Div>::new();

    Effect::new(move |_| {
        let current = generation.get();
        release(sub);
        let id = attempt_id.get_value();
        spawn_local(async move {
            let on_msg = move |msg| {
                if generation.try_get_untracked() == Some(current) {
                    store.apply(msg);
                    loaded.try_set(true);
                }
            };
            match ipc::subscribe_transcript(&id, on_msg).await {
                Ok(handle) => {
                    let mut handle = Some(handle);
                    if generation.try_get_untracked() == Some(current) {
                        sub.try_update_value(|slot| *slot = handle.take());
                    }
                    // The view is gone or resubscribed meanwhile.
                    if let Some(stale) = handle {
                        unsubscribe(stale).await;
                    }
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    });
    on_cleanup(move || {
        release(sub);
        if let Some((observer, _)) = observer.try_update_value(Option::take).flatten() {
            observer.disconnect();
        }
    });

    // Autoscroll: whenever the content grows while the user is at the bottom.
    Effect::new(move |_| {
        let Some(el) = content.get() else {
            return;
        };
        let on_resize = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            if store.at_bottom.try_get_untracked() == Some(true)
                && let Some(Some(scroller)) = scroller.try_get_untracked()
            {
                scroller.set_scroll_top(scroller.scroll_height());
            }
        });
        match web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()) {
            Ok(ro) => {
                ro.observe(&el);
                if let Some((old, _)) = observer
                    .try_update_value(|o| o.replace((ro, on_resize)))
                    .flatten()
                {
                    old.disconnect();
                }
            }
            Err(e) => leptos::logging::warn!("ResizeObserver: {e:?}"),
        }
    });

    // Only the user leaves the bottom (wheel up, keys, scrollbar): scroll events also come from
    // autoscroll and from trimmed rows, possibly after the next batch has grown the content.
    let leave_bottom = move || {
        if store.at_bottom.get_untracked() {
            store.at_bottom.set(false);
        }
    };
    let on_scroll = move |_| {
        if let Some(el) = scroller.get_untracked()
            && el.scroll_height() - el.scroll_top() - el.client_height() < BOTTOM_SLACK_PX
            && !store.at_bottom.get_untracked()
        {
            store.at_bottom.set(true);
        }
    };
    let on_wheel = move |ev: web_sys::WheelEvent| {
        if ev.delta_y() < 0.0 {
            leave_bottom();
        }
    };
    let on_key = move |ev: web_sys::KeyboardEvent| {
        if matches!(ev.key().as_str(), "ArrowUp" | "PageUp" | "Home") && !in_text_field(&ev) {
            leave_bottom();
        }
    };
    // A press on the scroller itself (not on a row) is its scrollbar.
    let on_press = move |ev: web_sys::MouseEvent| {
        let on_scroller = ev
            .target()
            .zip(scroller.get_untracked())
            .is_some_and(|(target, el)| AsRef::<web_sys::EventTarget>::as_ref(&el) == &target);
        if on_scroller {
            leave_bottom();
        }
    };

    let load_older = move |_| {
        let Some(first) = store
            .rows
            .with_untracked(|rows| rows.first().map(|r| r.idx))
        else {
            return;
        };
        loading_older.set(true);
        // The user went up: keep the anchor even if the content was short.
        store.at_bottom.set(false);
        let req = GetEntriesReq {
            attempt_id: attempt_id.get_value(),
            before_idx: first,
            limit: PAGE,
        };
        spawn_local(async move {
            let page = ipc::call::<GetEntries>(&req).await;
            loading_older.try_set(false);
            match page {
                Ok(page) => {
                    let before = scroller
                        .try_get_untracked()
                        .flatten()
                        .and_then(|el| anchor_top(&el, first));
                    store.prepend(page);
                    if let Some(before) = before {
                        pin_anchor(scroller, first, before, ANCHOR_FRAMES);
                    }
                }
                Err(e) => ctx.toasts.app_error(&e),
            }
        });
    };

    let go_latest = move |_| {
        store.at_bottom.set(true);
        if store.newer_hidden.get_untracked() {
            generation.update(|g| *g += 1);
        } else if let Some(el) = scroller.get_untracked() {
            el.set_scroll_top(el.scroll_height());
        }
    };

    view! {
        <div class="relative min-h-0 flex-1" data-view="transcript">
            <div
                node_ref=scroller
                class="h-full overflow-y-auto"
                tabindex="0"
                on:scroll=on_scroll
                on:wheel=on_wheel
                on:keydown=on_key
                on:mousedown=on_press
            >
                <div node_ref=content class="flex flex-col gap-3 px-4 py-3">
                    <Show when=move || store.has_more.get()>
                        <Button
                            variant=ButtonVariant::Ghost
                            size=ButtonSize::Sm
                            class="self-center"
                            attr:disabled=move || loading_older.get()
                            attr:data-action="load-older"
                            on:click=load_older
                        >
                            {move || loading_older.get().then(|| view! { <Spinner /> })}
                            "Carica precedenti"
                        </Button>
                    </Show>
                    <Show when=move || !loaded.get()>
                        <Skeleton class="h-10 w-2/3" />
                        <Skeleton class="h-16 w-full" />
                    </Show>
                    <Show when=move || {
                        loaded.get() && store.rows.with(Vec::is_empty) && store.typing.with(Option::is_none)
                    }>
                        <Empty class="border-none">
                            <EmptyHeader>
                                <EmptyTitle>"Nessun messaggio"</EmptyTitle>
                                <EmptyDescription>"L'agente non ha ancora scritto nulla."</EmptyDescription>
                            </EmptyHeader>
                        </Empty>
                    </Show>
                    <For
                        each=move || store.rows.get()
                        key=|row| row.idx
                        children=move |row| view! { <EntryRow row attempt_id /> }
                    />
                    {move || {
                        store
                            .typing
                            .get()
                            .map(|text| {
                                view! {
                                    <div
                                        class="text-muted-foreground flex items-start gap-2 text-sm"
                                        data-typing=""
                                    >
                                        <Spinner class="mt-0.5 shrink-0" />
                                        <p class="min-w-0 whitespace-pre-wrap italic [overflow-wrap:anywhere]">
                                            {tail(&text, TYPING_TAIL)}
                                        </p>
                                    </div>
                                }
                            })
                    }}
                    <Show when=move || store.newer_hidden.get()>
                        <p class="text-muted-foreground text-center text-xs">
                            "Ci sono messaggi più recenti: usa «Vai all'ultimo»."
                        </p>
                    </Show>
                </div>
            </div>
            <Show when=move || !store.at_bottom.get() || store.newer_hidden.get()>
                <Button
                    variant=ButtonVariant::Secondary
                    size=ButtonSize::Sm
                    class="absolute bottom-3 left-1/2 -translate-x-1/2 rounded-full shadow-md"
                    attr:data-action="go-latest"
                    on:click=go_latest
                >
                    <ArrowDown />
                    "Vai all'ultimo"
                </Button>
            </Show>
        </div>
    }
}

fn release(sub: StoredValue<Option<Subscription>, LocalStorage>) {
    if let Some(handle) = sub.try_update_value(Option::take).flatten() {
        spawn_local(unsubscribe(handle));
    }
}

/// The channel and its closure are dropped only once the backend stopped sending.
async fn unsubscribe((subscription_id, channel, closure): Subscription) {
    let req = UnsubscribeTranscriptReq { subscription_id };
    if let Err(e) = ipc::call::<UnsubscribeTranscript>(&req).await {
        leptos::logging::warn!("unsubscribe_transcript: {e}");
    }
    drop((channel, closure));
}

/// Keeps the row `idx` at `top` (`scrollTop += Δ`) from the frame after the prepend: the new
/// rows start at their placeholder height and `content-visibility` lays out the visible ones a
/// frame later. Measuring first makes it a no-op where the browser anchors natively.
fn pin_anchor(scroller: NodeRef<html::Div>, idx: u32, top: f64, frames: u32) {
    request_animation_frame(move || {
        let Some(Some(el)) = scroller.try_get_untracked() else {
            return;
        };
        if let Some(now) = anchor_top(&el, idx) {
            let delta = (now - top).round() as i32;
            if delta != 0 {
                el.set_scroll_top(el.scroll_top() + delta);
            }
        }
        if frames > 1 {
            pin_anchor(scroller, idx, top, frames - 1);
        }
    });
}

/// Top of the row `idx` relative to the viewport, if it is rendered.
fn anchor_top(scroller: &web_sys::HtmlDivElement, idx: u32) -> Option<f64> {
    let row = scroller
        .query_selector(&format!("[data-entry=\"{idx}\"]"))
        .ok()??;
    Some(row.get_bounding_client_rect().top())
}

/// The key goes to a field inside a row (e.g. the deny message), not to the scroller.
fn in_text_field(ev: &web_sys::KeyboardEvent) -> bool {
    ev.target()
        .and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok())
        .is_some_and(|el| {
            el.is_content_editable()
                || matches!(el.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
        })
}

fn tail(text: &str, max: usize) -> String {
    let skip = text.chars().count().saturating_sub(max);
    let kept: String = text.chars().skip(skip).collect();
    if skip > 0 { format!("…{kept}") } else { kept }
}

#[component]
fn EntryRow(row: Row, attempt_id: StoredValue<Id>) -> impl IntoView {
    // Outside the body closure, so an expanded block stays open across upserts.
    let open = RwSignal::new(false);
    let nested = row.entry.with_untracked(|e| e.parent_tool_use_id.is_some());
    let class = tw_merge!(
        "[content-visibility:auto] [contain-intrinsic-size:auto_3rem]",
        if nested { "ml-5 border-l-2 pl-3" } else { "" }
    );
    view! {
        <div data-entry=row.idx class=class>
            // `try_with`: a row dropped from the window may still be scheduled once.
            {move || row.entry.try_with(|e| entry_view(e, row.entry, attempt_id, open))}
        </div>
    }
}

fn entry_view(
    entry: &Entry,
    signal: RwSignal<Entry>,
    attempt_id: StoredValue<Id>,
    open: RwSignal<bool>,
) -> AnyView {
    match &entry.body {
        EntryBody::UserMessage { text } => {
            let text = text.clone();
            view! {
                <Message align=MessageAlign::End>
                    <MessageContent>
                        <Bubble align=BubbleAlign::End>
                            <BubbleContent class="whitespace-pre-wrap">{text}</BubbleContent>
                        </Bubble>
                    </MessageContent>
                </Message>
            }
            .into_any()
        }
        EntryBody::AssistantText { text } => {
            let text = text.clone();
            view! {
                <Message>
                    <MessageContent>
                        <Bubble variant=BubbleVariant::Ghost>
                            <BubbleContent class="whitespace-pre-wrap">{text}</BubbleContent>
                        </Bubble>
                    </MessageContent>
                </Message>
            }
            .into_any()
        }
        EntryBody::Thinking { text } => {
            let text = text.clone();
            expandable(
                open,
                view! {
                    <Brain class="size-4" />
                    <span class="italic">"Ragionamento"</span>
                }
                .into_any(),
                move || {
                    view! { <p class="whitespace-pre-wrap italic">{text.clone()}</p> }.into_any()
                },
            )
        }
        EntryBody::ToolCall {
            name,
            summary,
            input,
            status,
            output,
            ..
        } => {
            if matches!(status, ToolStatus::AwaitingApproval { .. }) {
                return view! { <ApprovalCard attempt_id=attempt_id.get_value() entry=signal /> }
                    .into_any();
            }
            tool_view(name, summary, input, status, output.as_ref(), open)
        }
        EntryBody::SessionInit {
            model,
            permission_mode,
            mcp_servers,
            warnings,
            ..
        } => {
            let mut parts = vec!["Sessione avviata".to_owned()];
            parts.extend(model.clone());
            parts.extend(permission_mode.as_deref().map(mode_label));
            parts.push(format!("{mcp_servers} server MCP"));
            let warnings = warnings.clone();
            view! {
                <div class="flex flex-col gap-2">
                    <Marker variant=MarkerVariant::Separator class="text-xs">
                        <MarkerContent>{parts.join(" · ")}</MarkerContent>
                    </Marker>
                    {warnings
                        .into_iter()
                        .map(|w| notice_view(Level::Warn, w, None, attempt_id))
                        .collect_view()}
                </div>
            }
            .into_any()
        }
        EntryBody::ApiRetry {
            attempt,
            max_retries,
            delay_ms,
            error,
        } => {
            let text = format!(
                "Nuovo tentativo API {attempt}/{max_retries} tra {}: {error}",
                seconds(*delay_ms)
            );
            view! {
                <Marker class="text-xs">
                    <Hourglass />
                    <MarkerContent>{text}</MarkerContent>
                </Marker>
            }
            .into_any()
        }
        EntryBody::TurnEnd {
            subtype,
            is_error,
            duration_ms,
            num_turns,
            cost_usd_estimate,
            permission_denials,
            text,
            limit,
        } => turn_end_view(
            subtype,
            *is_error,
            *duration_ms,
            *num_turns,
            *cost_usd_estimate,
            *permission_denials,
            text.clone(),
            *limit,
        ),
        EntryBody::Notice {
            level,
            text,
            action,
        } => notice_view(*level, text.clone(), *action, attempt_id),
        EntryBody::Stderr { text } => {
            let text = text.clone();
            expandable(
                open,
                view! {
                    <SquareTerminal class="size-4" />
                    <span>"stderr"</span>
                }
                .into_any(),
                move || view! { <pre class=PRE_CLASS>{text.clone()}</pre> }.into_any(),
            )
        }
    }
}

/// The vendored `Destructive` badge uses `text-destructive-foreground`, which has no token.
pub const ON_DESTRUCTIVE: &str = "text-white";

const PRE_CLASS: &str = "bg-muted max-h-80 overflow-auto rounded-md p-2 font-mono text-xs whitespace-pre-wrap [overflow-wrap:anywhere]";

/// A collapsed row whose body is rendered only while open (tool output, thinking, stderr are
/// collapsed by default, spec §9.4).
fn expandable(
    open: RwSignal<bool>,
    header: AnyView,
    body: impl Fn() -> AnyView + Send + Sync + 'static,
) -> AnyView {
    view! {
        <Collapsible open>
            <CollapsibleTrigger class="text-muted-foreground hover:text-foreground flex w-full min-w-0 items-center gap-2 text-left text-sm">
                <span class=move || {
                    tw_merge!("shrink-0 transition-transform", if open.get() { "rotate-90" } else { "" })
                }>
                    <ChevronRight class="size-4" />
                </span>
                {header}
            </CollapsibleTrigger>
            <CollapsibleContent class="pt-2 pl-6">{move || open.get().then(&body)}</CollapsibleContent>
        </Collapsible>
    }
    .into_any()
}

fn tool_view(
    name: &str,
    summary: &str,
    input: &str,
    status: &ToolStatus,
    output: Option<&ToolOutput>,
    open: RwSignal<bool>,
) -> AnyView {
    let (icon, badge) = match status {
        ToolStatus::Running => (view! { <Spinner /> }.into_any(), None),
        ToolStatus::Succeeded => (
            view! { <Check class="text-success size-4" /> }.into_any(),
            None,
        ),
        ToolStatus::Failed => (
            view! { <X class="text-destructive size-4" /> }.into_any(),
            Some((BadgeVariant::Destructive, "Errore", ON_DESTRUCTIVE)),
        ),
        ToolStatus::Denied { .. } => (
            view! { <Ban class="text-destructive size-4" /> }.into_any(),
            Some((BadgeVariant::Destructive, "Negato", ON_DESTRUCTIVE)),
        ),
        ToolStatus::Cancelled => (
            view! { <CircleStop class="text-muted-foreground size-4" /> }.into_any(),
            Some((BadgeVariant::Muted, "Annullato", "")),
        ),
        ToolStatus::AwaitingApproval { .. } => (
            view! { <Hourglass class="size-4" /> }.into_any(),
            Some((BadgeVariant::Warning, "Richiede approvazione", "")),
        ),
    };
    let header = view! {
        {icon}
        <span class="text-foreground min-w-0 truncate font-mono text-xs" title=summary.to_owned()>
            {summary.to_owned()}
        </span>
        {badge
            .map(|(variant, label, class)| {
                view! { <Badge variant class=tw_merge!("shrink-0", class)>{label}</Badge> }
            })}
    }
    .into_any();
    let name = name.to_owned();
    let input = pretty_json(input);
    let denied = match status {
        ToolStatus::Denied { message } => Some(message.clone()),
        _ => None,
    };
    let output = output.cloned();
    expandable(open, header, move || {
        let output = output.clone().map(|o| {
            let note = (o.truncated_bytes > 0)
                .then(|| format!("Output troncato: {} byte omessi.", o.truncated_bytes));
            let class = if o.is_error {
                tw_merge!(PRE_CLASS, "text-destructive")
            } else {
                PRE_CLASS.to_owned()
            };
            view! {
                <p class="text-muted-foreground text-xs font-medium">"Output"</p>
                <pre class=class>{o.text}</pre>
                {note.map(|n| view! { <p class="text-muted-foreground text-xs">{n}</p> })}
            }
        });
        view! {
            <div class="flex flex-col gap-1.5">
                {denied.clone().map(|m| view! { <p class="text-destructive text-xs">"Negato: " {m}</p> })}
                <p class="text-muted-foreground text-xs font-medium">{format!("Input di {name}")}</p>
                <pre class=PRE_CLASS>{input.clone()}</pre>
                {output}
            </div>
        }
        .into_any()
    })
}

#[allow(clippy::too_many_arguments)] // the fields of `EntryBody::TurnEnd`
fn turn_end_view(
    subtype: &str,
    is_error: bool,
    duration_ms: Option<u64>,
    num_turns: Option<u32>,
    cost: Option<f64>,
    denials: u32,
    text: Option<String>,
    limit: Option<LimitKind>,
) -> AnyView {
    let outcome = if is_error {
        (
            BadgeVariant::Destructive,
            format!("Errore ({subtype})"),
            ON_DESTRUCTIVE,
        )
    } else if subtype == "success" {
        (BadgeVariant::Success, "Completato".to_owned(), "")
    } else {
        (BadgeVariant::Muted, subtype.to_owned(), "")
    };
    let mut details = Vec::new();
    details.extend(duration_ms.map(seconds));
    details.extend(num_turns.map(|n| format!("{n} turni")));
    details.extend(cost.map(|c| format!("≈ ${} stima API", decimal(c, 2))));
    if denials > 0 {
        details.push(format!("{denials} negati"));
    }
    let limit = limit.map(|l| match l {
        LimitKind::UsageLimit => {
            "Limite d'uso raggiunto: i nuovi avvii restano in pausa finché non premi «Riprendi»."
        }
        LimitKind::RateLimit => "Limite di frequenza dell'API: riprova tra poco.",
        LimitKind::AuthFailure => "Accesso a Claude Code scaduto o non valido: accedi di nuovo.",
        LimitKind::Billing => "Problema di fatturazione dell'account Claude.",
    });
    let error = is_error.then_some(()).and(text);
    view! {
        <div class="flex flex-col gap-2" data-turn-end="">
            <Marker variant=MarkerVariant::Separator class="text-xs">
                <MarkerContent class="flex flex-wrap items-center justify-center gap-1.5">
                    <Badge variant=outcome.0 class=outcome.2>{outcome.1}</Badge>
                    {details.into_iter().map(|d| view! { <Badge variant=BadgeVariant::Outline>{d}</Badge> }).collect_view()}
                </MarkerContent>
            </Marker>
            {limit.map(|l| alert(Level::Error, "Turno interrotto".into(), l.into()))}
            {error.map(|t| alert(Level::Error, "Errore del turno".into(), t))}
        </div>
    }
    .into_any()
}

fn notice_view(
    level: Level,
    text: String,
    action: Option<NoticeAction>,
    attempt_id: StoredValue<Id>,
) -> AnyView {
    let ctx = use_app();
    let button = action.map(|NoticeAction::NewSession| {
        let sending = RwSignal::new(false);
        view! {
            <Button
                size=ButtonSize::Sm
                class="mt-2"
                attr:disabled=move || sending.get()
                on:click=move |_| new_session(ctx, attempt_id, sending)
            >
                "Nuova sessione"
            </Button>
        }
    });
    view! {
        <div data-notice="">
            {alert(level, String::new(), text)}
            {button}
        </div>
    }
    .into_any()
}

fn new_session(ctx: AppCtx, attempt_id: StoredValue<Id>, sending: RwSignal<bool>) {
    sending.set(true);
    let req = SendFollowUpReq {
        attempt_id: attempt_id.get_value(),
        prompt: NEW_SESSION_PROMPT.into(),
        permission_mode: None,
        fresh_session: true,
    };
    spawn_local(async move {
        if let Err(e) = ipc::call::<SendFollowUp>(&req).await {
            ctx.toasts.app_error(&e);
        }
        sending.try_set(false);
    });
}

fn alert(level: Level, title: String, text: String) -> AnyView {
    let class = match level {
        Level::Info => "",
        Level::Warn => "border-warning/60 bg-warning-light/40 dark:bg-warning-dark/20",
        Level::Error => "border-destructive/50 text-destructive",
    };
    view! {
        <Alert class=class>
            {(!title.is_empty()).then(|| view! { <AlertTitle>{title}</AlertTitle> })}
            <AlertDescription class="whitespace-pre-wrap">{text}</AlertDescription>
        </Alert>
    }
    .into_any()
}

/// Labels of the permission modes (spec D6).
fn mode_label(mode: &str) -> String {
    match mode {
        "default" => "Supervisionato".into(),
        "acceptEdits" => "Auto-edit".into(),
        "bypassPermissions" => "Autonomo".into(),
        other => other.into(),
    }
}

fn pretty_json(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| text.to_owned())
}

fn seconds(ms: u64) -> String {
    format!("{} s", decimal(ms as f64 / 1000.0, 1))
}

/// Italian decimal comma.
fn decimal(value: f64, digits: usize) -> String {
    format!("{value:.digits$}").replace('.', ",")
}
