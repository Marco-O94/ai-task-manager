// ATM-PORT: script removed; signal-driven
//
// Ported from rust-ui/leptos-ui `app_crates/registry/src/ui/dialog.rs` (commit in VENDORED.toml).
// Same markup and classes, minus the inline script element (our CSP blocks it). `open: RwSignal<bool>`
// on <Dialog> drives `data-state`; Esc and backdrop clicks close; scroll lock and focus are
// handled from Rust. Pointer events follow `data-state` via classes instead of inline styles.
use icons::X;
use leptos::context::Provider;
use leptos::ev;
use leptos::prelude::*;
use leptos_ui::clx;
use tw_merge::*;
use wasm_bindgen::JsCast;

use crate::hooks::use_random::use_random_id_for;
use crate::hooks::use_scroll_lock;
use crate::ui::button::{Button, ButtonSize, ButtonVariant};

mod components {
    use super::*;
    clx! {DialogBody, div, "flex flex-col gap-4"}
    clx! {DialogHeader, div, "flex flex-col gap-2 text-center sm:text-left"}
    clx! {DialogTitle, h3, "text-lg leading-none font-semibold"}
    clx! {DialogDescription, p, "text-muted-foreground text-sm"}
    clx! {DialogFooter, footer, "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end"}
}

pub use components::*;

const FOCUSABLE: &str = "button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])";
const CLOSE_ANIMATION_MS: u32 = 200;

/* ========================================================== */
/*                     ✨ FUNCTIONS ✨                        */
/* ========================================================== */

#[derive(Clone)]
struct DialogContext {
    target_id: String,
    open: RwSignal<bool>,
}

/// Returns the dialog trigger ID when called inside a `<Dialog>` subtree.
/// Use this to wire custom elements (e.g. AttachmentTrigger) as dialog triggers.
pub fn use_dialog_trigger_id() -> Option<String> {
    use_context::<DialogContext>().map(|c| c.target_id.clone())
}

#[component]
pub fn Dialog(open: RwSignal<bool>, children: Children, #[prop(optional, into)] class: String) -> impl IntoView {
    let dialog_target_id = use_random_id_for("dialog");

    let ctx = DialogContext { target_id: dialog_target_id, open };

    let merged_class = tw_merge!("w-fit", class);

    view! {
        <Provider value=ctx>
            <div class=merged_class data-name="__Dialog">
                {children()}
            </div>
        </Provider>
    }
}

#[component]
pub fn DialogTrigger(
    children: Children,
    #[prop(optional, into)] class: String,
    #[prop(default = ButtonVariant::Outline)] variant: ButtonVariant,
    #[prop(default = ButtonSize::Default)] size: ButtonSize,
) -> impl IntoView {
    let ctx = expect_context::<DialogContext>();
    let trigger_id = format!("trigger_{}", ctx.target_id);
    let open = ctx.open;

    view! {
        <Button
            class=class
            attr:id=trigger_id
            attr:tabindex="0"
            attr:data-dialog-trigger=ctx.target_id
            variant=variant
            size=size
            on:click=move |_| open.set(true)
        >
            {children()}
        </Button>
    }
}

#[component]
pub fn DialogContent(
    children: Children,
    #[prop(optional, into)] class: String,
    #[prop(default = true)] show_close_button: bool,
    #[prop(default = true)] close_on_backdrop_click: bool,
    #[prop(default = "Dialog")] data_name_prefix: &'static str,
) -> impl IntoView {
    let ctx = expect_context::<DialogContext>();
    let open = ctx.open;
    let merged_class = tw_merge!(
        // "flex flex-col gap-4", // TODO 🐛 Bug when I try to have this.. Using DialogBody instead.
        "relative bg-background border rounded-2xl shadow-lg p-6 w-full max-w-[calc(100%-2rem)] max-h-[85vh] fixed top-[50%] left-[50%] translate-x-[-50%] translate-y-[-50%] z-100 transition-all duration-200 data-[state=closed]:opacity-0 data-[state=closed]:scale-95 data-[state=open]:opacity-100 data-[state=open]:scale-100 data-[state=closed]:pointer-events-none data-[state=closed]:invisible",
        class
    );

    let backdrop_data_name = format!("{}Backdrop", data_name_prefix);
    let content_data_name = format!("{}Content", data_name_prefix);
    let backdrop_id = format!("{}_backdrop", ctx.target_id);
    let state = move || if open.get() { "open" } else { "closed" };

    let content_ref = NodeRef::<leptos::html::Div>::new();
    let restore_focus = StoredValue::new_local(None::<web_sys::HtmlElement>);

    // Scroll lock and focus follow the open/closed transitions of the signal.
    Effect::new(move |was_open: Option<bool>| {
        let is_open = open.get();
        if is_open && was_open != Some(true) {
            restore_focus.set_value(active_element());
            use_scroll_lock::lock();
            if let Some(first) = content_ref
                .get_untracked()
                .and_then(|el| el.query_selector(FOCUSABLE).ok().flatten())
                .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
            {
                let _ = first.focus();
            }
        } else if !is_open && was_open == Some(true) {
            use_scroll_lock::unlock(CLOSE_ANIMATION_MS);
            if let Some(el) = restore_focus.try_update_value(Option::take).flatten() {
                let _ = el.focus();
            }
        }
        is_open
    });

    let esc = window_event_listener(ev::keydown, move |e| {
        if e.key() == "Escape" && open.try_get_untracked() == Some(true) {
            e.prevent_default();
            open.set(false);
        }
    });
    on_cleanup(move || {
        esc.remove();
        if open.try_get_untracked() == Some(true) {
            use_scroll_lock::unlock(0);
        }
    });

    view! {
        <div
            data-name=backdrop_data_name
            id=backdrop_id
            class="fixed inset-0 transition-opacity duration-200 data-[state=closed]:pointer-events-none z-60 bg-black/50 data-[state=closed]:opacity-0 data-[state=open]:opacity-100"
            data-state=state
            on:click=move |_| {
                if close_on_backdrop_click {
                    open.set(false);
                }
            }
        />

        <div
            node_ref=content_ref
            data-name=content_data_name
            class=merged_class
            id=ctx.target_id
            data-target="target__dialog"
            data-state=state
            data-backdrop=if close_on_backdrop_click { "auto" } else { "manual" }
            role="dialog"
            aria-modal="true"
        >
            <button
                type="button"
                class=format!(
                    "absolute top-4 right-4 p-1 rounded-sm focus:ring-2 focus:ring-offset-2 focus:outline-none [&_svg:not([class*='size-'])]:size-4 focus:ring-ring{}",
                    if show_close_button { "" } else { " hidden" },
                )
                aria-label="Close dialog"
                on:click=move |_| open.set(false)
            >
                <span class="hidden">"Close Dialog"</span>
                <X />
            </button>

            {children()}
        </div>
    }
}

#[component]
pub fn DialogClose(
    children: Children,
    #[prop(optional, into)] class: String,
    #[prop(default = ButtonVariant::Outline)] variant: ButtonVariant,
    #[prop(default = ButtonSize::Default)] size: ButtonSize,
) -> impl IntoView {
    let open = expect_context::<DialogContext>().open;

    view! {
        <Button class=class attr:aria-label="Close dialog" variant=variant size=size on:click=move |_| open.set(false)>
            {children()}
        </Button>
    }
}

#[component]
pub fn DialogAction(
    children: Children,
    #[prop(optional, into)] class: String,
    #[prop(default = ButtonVariant::Default)] variant: ButtonVariant,
    #[prop(default = ButtonSize::Default)] size: ButtonSize,
) -> impl IntoView {
    let open = expect_context::<DialogContext>().open;

    view! {
        <Button class=class attr:aria-label="Close dialog" variant=variant size=size on:click=move |_| open.set(false)>
            {children()}
        </Button>
    }
}

fn active_element() -> Option<web_sys::HtmlElement> {
    document().active_element()?.dyn_into().ok()
}
