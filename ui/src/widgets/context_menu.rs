//! Small context menu, hand-written and signal-driven (spec F1): the registry's
//! `dropdown_menu` ships an inline script, which §9.1 and the CSP forbid. Owner: UI-SHELL.
//! Content `[role=menu][data-name=ContextMenuContent]` (DOM contract, spec §9.2).
//!
//! A [`ContextMenu`] opens at a point through its [`MenuState`] and is mounted fresh at each
//! opening. It closes on Esc, on a pointerdown outside it, when an item is chosen, and when
//! the window loses the focus or is resized; Esc and a chosen item put the focus back on the
//! trigger. ↑/↓/Home/End move the focus between the items; Tab closes the menu and moves on
//! from the trigger.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{HtmlElement, KeyboardEvent, Node};

/// Space kept between the menu and the edges of the window, in pixels.
const MARGIN: f64 = 4.0;

/// Open state of one [`ContextMenu`]: where it shows and which element opened it.
#[derive(Clone, Copy)]
pub struct MenuState {
    /// Requested top-left corner, viewport pixels; `None` while closed.
    at: RwSignal<Option<(f64, f64)>>,
    /// Gets the focus back on close; a pointerdown on it is not "outside".
    trigger: StoredValue<Option<HtmlElement>, LocalStorage>,
}

impl Default for MenuState {
    fn default() -> Self {
        Self::new()
    }
}

impl MenuState {
    pub fn new() -> Self {
        Self {
            at: RwSignal::new(None),
            trigger: StoredValue::new_local(None),
        }
    }

    /// Opens (or moves) the menu with its top-left corner at `(x, y)`, viewport pixels.
    pub fn open_at(self, x: f64, y: f64, trigger: Option<HtmlElement>) {
        self.trigger.set_value(trigger);
        self.at.set(Some((x, y)));
    }

    /// Opens the menu under `trigger`, aligned with its left edge.
    pub fn open_below(self, trigger: HtmlElement) {
        let rect = trigger.get_bounding_client_rect();
        self.open_at(rect.left(), rect.bottom() + MARGIN, Some(trigger));
    }

    /// Tracked.
    pub fn is_open(self) -> bool {
        self.at.with(Option::is_some)
    }

    /// `el` opened the menu, which is open (untracked).
    pub fn opened_by(self, el: &HtmlElement) -> bool {
        self.at.with_untracked(Option::is_some)
            && self
                .trigger
                .with_value(|t| t.as_ref().is_some_and(|t| t == el))
    }

    /// Closes the menu; `refocus` puts the focus back on its trigger.
    pub fn close(self, refocus: bool) {
        if self.at.try_with_untracked(Option::is_none).unwrap_or(true) {
            return;
        }
        self.at.set(None);
        let trigger = self.trigger.try_update_value(Option::take).flatten();
        if let Some(trigger) = trigger.filter(|_| refocus) {
            let _ = trigger.focus();
        }
    }

    fn trigger_contains(self, node: &Node) -> bool {
        self.trigger
            .try_with_value(|t| t.as_ref().is_some_and(|t| t.contains(Some(node))))
            .unwrap_or(false)
    }
}

/// The menu while open; `label` names it for assistive technologies. The children are
/// [`ContextMenuItem`]s.
#[component]
pub fn ContextMenu(
    state: MenuState,
    #[prop(into)] label: Signal<String>,
    children: ChildrenFn,
) -> impl IntoView {
    let open = Memo::new(move |_| state.is_open());
    view! {
        <Show when=move || open.get()>
            <MenuPanel state label children=children.clone() />
        </Show>
    }
}

#[component]
fn MenuPanel(state: MenuState, label: Signal<String>, children: ChildrenFn) -> impl IntoView {
    provide_context(state);
    let panel = NodeRef::<html::Div>::new();
    // Measured once mounted, to keep the whole menu inside the window.
    let size = RwSignal::new((0.0, 0.0));
    Effect::new(move |_| {
        let Some(el) = panel.get() else {
            return;
        };
        let rect = el.get_bounding_client_rect();
        size.set((rect.width(), rect.height()));
        focus_item(&el, |_, _| 0);
    });
    let corner = move || {
        let (x, y) = state.at.get().unwrap_or_default();
        let (w, h) = size.get();
        let (vw, vh) = viewport();
        (clamp(x, w, vw), clamp(y, h, vh))
    };

    let esc = window_event_listener(ev::keydown, move |e| {
        if e.key() == "Escape" {
            e.prevent_default();
            state.close(true);
        }
    });
    let outside = window_event_listener(ev::pointerdown, move |e| {
        let Some(node) = e.target().and_then(|t| t.dyn_into::<Node>().ok()) else {
            return;
        };
        let inside = panel
            .get_untracked()
            .is_some_and(|p| p.contains(Some(&node)));
        // A pointerdown on the trigger is left to its click, which toggles the menu.
        if !inside && !state.trigger_contains(&node) {
            state.close(false);
        }
    });
    let blur = window_event_listener(ev::blur, move |_| state.close(false));
    let resize = window_event_listener(ev::resize, move |_| state.close(false));
    on_cleanup(move || {
        esc.remove();
        outside.remove();
        blur.remove();
        resize.remove();
    });

    let keys = move |e: KeyboardEvent| {
        let key = e.key();
        if key == "Tab" {
            state.close(true);
            return;
        }
        let pick: fn(Option<usize>, usize) -> usize = match key.as_str() {
            "ArrowDown" => |i, n| i.map_or(0, |i| (i + 1) % n),
            "ArrowUp" => |i, n| i.map_or(n - 1, |i| (i + n - 1) % n),
            "Home" => |_, _| 0,
            "End" => |_, n| n - 1,
            _ => return,
        };
        e.prevent_default();
        if let Some(el) = panel.get_untracked() {
            focus_item(&el, pick);
        }
    };
    view! {
        <div
            node_ref=panel
            role="menu"
            aria-label=move || label.get()
            aria-orientation="vertical"
            data-name="ContextMenuContent"
            class="bg-popover text-popover-foreground fixed z-50 flex min-w-48 flex-col rounded-md border p-1 shadow-md"
            style:left=move || format!("{}px", corner().0)
            style:top=move || format!("{}px", corner().1)
            on:keydown=keys
        >
            {children()}
        </div>
    }
}

/// One entry of the enclosing [`ContextMenu`], activated by a click (Enter and Space click a
/// button): it closes the menu, puts the focus back on the trigger, then runs `on_select`.
#[component]
pub fn ContextMenuItem(
    #[prop(into)] on_select: Callback<()>,
    #[prop(optional)] destructive: bool,
    children: Children,
) -> impl IntoView {
    let state = expect_context::<MenuState>();
    let tone = if destructive { "text-destructive" } else { "" };
    view! {
        <button
            type="button"
            role="menuitem"
            tabindex="-1"
            class=format!(
                "hover:bg-accent focus:bg-accent flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left text-sm outline-none [&_svg]:size-4 [&_svg]:shrink-0 {tone}",
            )
            on:click=move |_| {
                state.close(true);
                on_select.run(());
            }
        >
            {children()}
        </button>
    }
}

/// Focuses item `pick(index of the focused item, count)` of `menu`.
fn focus_item(menu: &HtmlElement, pick: impl Fn(Option<usize>, usize) -> usize) {
    let Ok(list) = menu.query_selector_all("[role=menuitem]") else {
        return;
    };
    let items: Vec<HtmlElement> = (0..list.length())
        .filter_map(|i| list.item(i)?.dyn_into().ok())
        .collect();
    if items.is_empty() {
        return;
    }
    let active = document().active_element();
    let current = items
        .iter()
        .position(|item| active.as_deref() == Some(item.unchecked_ref()));
    if let Some(item) = items.get(pick(current, items.len())) {
        let _ = item.focus();
    }
}

fn viewport() -> (f64, f64) {
    let px = |v: Result<JsValue, JsValue>| v.ok().and_then(|v| v.as_f64()).unwrap_or(f64::MAX);
    let w = window();
    (px(w.inner_width()), px(w.inner_height()))
}

/// Start of a box `len` long requested at `start`, moved back so that it ends inside the
/// viewport (minus [`MARGIN`]); a box larger than the viewport starts at the margin.
fn clamp(start: f64, len: f64, viewport: f64) -> f64 {
    start.min(viewport - len - MARGIN).max(MARGIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_stays_inside_the_window() {
        assert_eq!(clamp(100.0, 200.0, 1000.0), 100.0);
        assert_eq!(clamp(900.0, 200.0, 1000.0), 796.0);
        assert_eq!(clamp(-5.0, 200.0, 1000.0), MARGIN);
        assert_eq!(clamp(50.0, 2000.0, 1000.0), MARGIN);
    }
}
