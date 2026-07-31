//! A reusable, searchable, category-grouped single-key picker.
//!
//! A pinned search box over a scrollable, grouped list of every bindable key. It
//! emits a single evdev name; chords are assembled by the consumer from several
//! picks. Reused for Customize (each chord key + mouse/multimedia) and Macros
//! (per step).
//!
//! The catalog is `&'static`, so nothing is cloned in the hot path: the filter
//! produces an ordered `Vec<usize>` of indices into the static [`search_index`]
//! slice. Render and keyboard navigation both walk that single list, which keeps
//! the highlighted row and the rendered row in lockstep.
//!
//! Unlike the Dioxus original this view is the PANEL only — it no longer owns
//! the trigger button, the open flag, or the click-away backdrop. The parent
//! creates it once with [`KeyPicker::new`], shows or hides it, and anchors it
//! (the panel sets no position of its own); [`key_picker_trigger`] keeps every
//! call site's trigger button identical. Picks come back as
//! [`KeyPickerEvent`]s over a `cx.subscribe`, so a parent view handles them with
//! `&mut Self` in hand instead of through a callback.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, EventEmitter, KeyDownEvent, SharedString, Window,
    div,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::model::{KeyEntry, key_label, search_index};
use crate::theme;

/// What the picker tells its parent.
pub enum KeyPickerEvent {
    /// A key was chosen; carries its canonical evdev name.
    Picked(&'static str),
    /// The picker wants to close without a pick (Escape). The parent owns the
    /// open flag, so only it can actually close the panel.
    Dismissed,
}

pub struct KeyPicker {
    /// The search box. Its `Change` events drive [`KeyPicker::refilter`].
    search: Entity<InputState>,
    /// Ordered indices into [`search_index`] that match the current query — one
    /// source of truth shared by the renderer and the keyboard handler, so the
    /// highlight never drifts from the rows on screen.
    filtered: Vec<usize>,
    /// Position within `filtered`, not within the catalog.
    highlighted: usize,
}

impl EventEmitter<KeyPickerEvent> for KeyPicker {}

impl KeyPicker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search keys…"));
        cx.subscribe(&search, |this, state, ev: &InputEvent, cx| match ev {
            InputEvent::Change => {
                let query = state.read(cx).value().to_string();
                this.refilter(&query);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.pick_highlighted(cx),
            _ => {}
        })
        .detach();

        Self {
            search,
            filtered: (0..search_index().len()).collect(),
            highlighted: 0,
        }
    }

    /// Re-arm the picker for a fresh open: clear the query, reset the highlight,
    /// and put the caret in the search box. The parent calls this when it opens
    /// the panel — the Dioxus version did the same work in its trigger's click
    /// handler, which the parent now owns.
    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        self.refilter("");
        cx.notify();
    }

    /// Recompute the match list for `query`. Was a `use_memo`; a plain method
    /// now, called from the search box's `Change` event.
    fn refilter(&mut self, query: &str) {
        let rows = search_index();
        let q = query.to_lowercase();
        self.filtered = if q.is_empty() {
            (0..rows.len()).collect()
        } else {
            rows.iter()
                .enumerate()
                .filter(|(_, r)| r.0.contains(&q))
                .map(|(i, _)| i)
                .collect()
        };
        // A fresh filter invalidates the old highlight position; reset to the top.
        self.highlighted = 0;
    }

    /// Emit the highlighted row, if the filter left anything to pick.
    fn pick_highlighted(&mut self, cx: &mut Context<Self>) {
        if let Some(&row) = self.filtered.get(self.highlighted) {
            cx.emit(KeyPickerEvent::Picked(search_index()[row].1.evdev));
        }
    }

    /// Move the highlight by one row, clamped at both ends.
    fn move_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        self.highlighted = if down {
            (self.highlighted + 1).min(self.filtered.len() - 1)
        } else {
            self.highlighted.saturating_sub(1)
        };
        cx.notify();
    }
}

impl Render for KeyPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = theme::mono(cx);
        let rows = search_index();
        // Snapshot both so the row builders below can take `cx` without holding
        // a borrow on `self`.
        let filtered = self.filtered.clone();
        let highlighted = self.highlighted;

        let list = if filtered.is_empty() {
            div()
                .px(theme::sp(12.))
                .py(theme::sp(22.))
                .text_center()
                .text_size(theme::sp(12.))
                .text_color(theme::mute())
                .child("No keys match")
                .into_any_element()
        } else {
            // Walk matched indices (catalog order). Open a new group whenever the
            // category changes; `flat` is the running position used to match the
            // highlight. Comparing the head STRING is equivalent to comparing
            // categories (distinct categories have distinct heads and rows are
            // contiguous) and needs no enum derives.
            let mut groups: Vec<AnyElement> = Vec::new();
            let mut group: Vec<AnyElement> = Vec::new();
            let mut group_head = "";

            for (flat, &row) in filtered.iter().enumerate() {
                let r = &rows[row];
                if group_head != r.2 {
                    flush_group(&mut groups, group_head, &mut group, &mono);
                    group_head = r.2;
                }
                group.push(entry_row(flat, r.1, flat == highlighted, cx));
            }
            flush_group(&mut groups, group_head, &mut group, &mono);

            div().children(groups).into_any_element()
        };

        div()
            .w(theme::sp(280.))
            .max_h(theme::sp(340.))
            .flex()
            .flex_col()
            .rounded(theme::sp(theme::RADIUS))
            .bg(theme::panel_1())
            .border_1()
            .border_color(theme::line())
            .shadow_lg()
            .overflow_hidden()
            // Escape and the arrows ride the focused search box and bubble to
            // here; Enter arrives as the input's own `PressEnter` event instead.
            .on_key_down(cx.listener(
                |this, e: &KeyDownEvent, _, cx| match e.keystroke.key.as_str() {
                    "escape" => cx.emit(KeyPickerEvent::Dismissed),
                    "down" => this.move_highlight(true, cx),
                    "up" => this.move_highlight(false, cx),
                    _ => {}
                },
            ))
            .child(
                div()
                    .flex_none()
                    .m(theme::sp(8.))
                    .child(Input::new(&self.search)),
            )
            .child(
                div()
                    .id("kp-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(theme::sp(4.))
                    .pb(theme::sp(6.))
                    .child(list),
            )
    }
}

/// Close the running group into `groups`, if it has any rows. A no-op on an
/// empty group, so the first category and the final flush share one path.
fn flush_group(
    groups: &mut Vec<AnyElement>,
    head: &str,
    group: &mut Vec<AnyElement>,
    mono: &SharedString,
) {
    if group.is_empty() {
        return;
    }
    let rows = std::mem::take(group);
    groups.push(
        div()
            .pt(theme::sp(2.))
            .child(
                div()
                    .px(theme::sp(9.))
                    .pt(theme::sp(7.))
                    .pb(theme::sp(5.))
                    .bg(theme::panel_1())
                    .font_family(mono.clone())
                    .text_size(theme::sp(9.5))
                    .text_color(theme::mute())
                    .child(head.to_uppercase()),
            )
            .children(rows)
            .into_any_element(),
    );
}

/// One catalog row: the readable label, and the raw evdev name alongside it.
/// `flat` is the row's position in the filtered list — the highlight's unit.
fn entry_row(
    flat: usize,
    entry: &'static KeyEntry,
    highlighted: bool,
    cx: &mut Context<KeyPicker>,
) -> AnyElement {
    let evdev = entry.evdev;
    div()
        .id(("kp-entry", flat))
        .flex()
        .items_center()
        .justify_between()
        .gap(theme::sp(10.))
        .px(theme::sp(9.))
        .py(theme::sp(7.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .text_size(theme::sp(12.5))
        .text_color(if highlighted {
            theme::ink()
        } else {
            theme::dim()
        })
        .when(highlighted, |el| el.bg(theme::rust_wash()))
        .hover(|s| s.bg(theme::panel_2()).text_color(theme::ink()))
        .cursor_pointer()
        // Pointing at a row moves the highlight, so mouse and keyboard never
        // disagree about which row Enter would take.
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered && this.highlighted != flat {
                this.highlighted = flat;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
            cx.emit(KeyPickerEvent::Picked(evdev));
        }))
        .child(div().child(entry.label))
        .child(
            div()
                .flex_none()
                .text_size(theme::sp(10.5))
                .text_color(theme::mute())
                .child(evdev),
        )
        .into_any_element()
}

/// The picker's trigger button. The parent owns the open flag now, so it owns
/// the trigger too — this keeps every call site's button identical. `value` is
/// the current evdev name (may be empty); `placeholder` relabels the button
/// while `value` is empty (default "Choose key…"), so a caller can make the
/// trigger read e.g. "+ Tap" directly.
pub fn key_picker_trigger(
    id: impl Into<gpui::ElementId>,
    value: &str,
    placeholder: Option<&str>,
    mono: SharedString,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = if value.is_empty() {
        placeholder.unwrap_or("Choose key…").to_string()
    } else {
        key_label(value)
    };
    div()
        .id(id)
        .flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap(theme::sp(8.))
        .px(theme::sp(11.))
        .py(theme::sp(9.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .bg(theme::panel_1())
        .border_1()
        .border_color(theme::line())
        .font_family(mono)
        .text_size(theme::sp(12.5))
        .text_color(theme::ink())
        .hover(|s| s.bg(theme::panel_2()).border_color(theme::mute()))
        .cursor_pointer()
        .on_click(on_click)
        .child(label)
}
