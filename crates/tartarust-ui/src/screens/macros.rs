//! Macros screen — name rail + compact chip-flow step editor for named macros.
//!
//! The Dioxus build split this into three components whose state lived in
//! per-instance signals: the screen, a `MacroEditor` holding the rename draft,
//! and one `StepChip` per step holding its own `editing` flag and its own
//! `KeyPicker`. GPUI has ONE view. The key picker, the rename draft, and the
//! delay field are entities this view owns, so "which chip is open" and "what
//! will the next pick do" are fields here instead of state scattered across the
//! chips — which also means only one chip edits at a time.
//!
//! Editing is optimistic everywhere: the store is written first, then the whole
//! macro is pushed with `SaveMacro` (a blind by-name upsert daemon-side, which
//! is why names are de-duped on create and collisions are refused on rename).

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, FontWeight, MouseButton, Rgba, SharedString,
    Window, deferred, div, px, rgba,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::components::widgets::{btn_danger, btn_ghost, btn_primary, card, page_head};
use crate::components::{KeyPicker, KeyPickerEvent, key_picker_trigger};
use crate::ipc;
use crate::model::{
    AppState, MAX_MACRO_DELAY_MS, Macro, MacroStep, adjust_index_after_remove, key_label,
    unique_name,
};
use crate::store;
use crate::theme;

// ---------------------------------------------------------------------------
// Local helpers
// ---------------------------------------------------------------------------

/// The visual voice of one step kind — the old `.step-chip.press/.release/
/// .delay` CSS rules, as values.
struct ChipSkin {
    border: Rgba,
    bg: Rgba,
    ink: Rgba,
    num_bg: Rgba,
    num_ink: Rgba,
    pad_x: f32,
    pad_y: f32,
    text: f32,
}

fn chip_skin(step: &MacroStep) -> ChipSkin {
    match step {
        MacroStep::Press { .. } => ChipSkin {
            border: theme::line(),
            bg: theme::panel_2(),
            ink: theme::ink(),
            num_bg: theme::rust_wash(),
            num_ink: theme::rust(),
            pad_x: 10.,
            pad_y: 6.,
            text: 12.,
        },
        MacroStep::Release { .. } => ChipSkin {
            border: theme::line(),
            bg: theme::panel_1(),
            ink: theme::dim(),
            num_bg: theme::panel_2(),
            num_ink: theme::mute(),
            pad_x: 10.,
            pad_y: 6.,
            text: 12.,
        },
        // A delay wears drift amber: it is a value you dialed in, not a key.
        MacroStep::Delay { .. } => ChipSkin {
            border: rgba(0xD9A44A66),
            bg: theme::amber_wash(),
            ink: theme::amber(),
            num_bg: theme::panel_2(),
            num_ink: theme::mute(),
            pad_x: 9.,
            pad_y: 5.,
            text: 11.5,
        },
    }
}

fn step_label(step: &MacroStep) -> String {
    match step {
        MacroStep::Press { key } | MacroStep::Release { key } => key_label(key),
        MacroStep::Delay { ms } => format!("{ms} ms"),
    }
}

/// Mutate the selected macro's steps in the store, then persist. `f` runs only
/// when `sel` indexes a real macro, and its return value gates the save (return
/// `false` for a no-op edit). The whole macro is cloned out inside the update
/// and saved after it returns.
fn mutate_and_save(
    store: &Entity<AppState>,
    sel: usize,
    cx: &mut App,
    f: impl FnOnce(&mut Macro) -> bool,
) {
    let to_save = store.update(cx, |s, cx| {
        let saved = match s.macros.get_mut(sel) {
            Some(m) => f(m).then(|| m.clone()),
            None => None,
        };
        if saved.is_some() {
            cx.notify();
        }
        saved
    });
    if let Some(m) = to_save {
        store::fire(store, ipc::req_save_macro(m), cx);
    }
}

/// The `.card-title` voice: a small monospace all-caps rule across the top of a
/// card, with room for a value on the right.
fn card_head(mono: &SharedString) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.))
        .font_family(mono.clone())
        .text_size(px(10.))
        .text_color(theme::mute())
}

/// The quiet in-card note the empty and loading states use (`.li-meta`).
fn hint(text: impl Into<SharedString>, mono: &SharedString) -> gpui::Div {
    div()
        .py(px(8.))
        .px(px(2.))
        .font_family(mono.clone())
        .text_size(px(11.))
        .text_color(theme::mute())
        .child(text.into())
}

fn divider() -> gpui::Div {
    div().flex_none().h(px(1.)).bg(theme::line_soft())
}

/// The ✕ affordance shared by rail rows and step chips. Its mouse-down stops
/// there, so the row or chip underneath does not also act on the press.
fn chip_x(
    id: (&'static str, usize),
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .size(px(15.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .text_size(px(12.))
        .text_color(theme::mute())
        .hover(|s| s.bg(theme::red_wash()).text_color(theme::red()))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .child("✕")
}

// ---------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------

/// What the next pick from the shared [`KeyPicker`] will do. Held as an
/// `Option` on the view: `None` means the panel is closed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PickTarget {
    /// Append a Press then a Release of the picked key ("+ Tap").
    Tap,
    /// Append a Press ("+ Key down").
    KeyDown,
    /// Append a Release ("+ Key up").
    KeyUp,
    /// Re-key the press/release step at this index.
    Step(usize),
}

pub struct Macros {
    store: Entity<AppState>,
    /// The one key picker panel, shared by the three add controls and by every
    /// step chip — only one can be open at a time, so one panel is enough.
    picker: Entity<KeyPicker>,
    /// What an incoming pick applies to; `None` = the panel is closed.
    picking: Option<PickTarget>,
    /// The rename draft. Bound to a LOCAL input, not the store, so typing does
    /// not churn the store — which online would flip the selected macro back to
    /// "not loaded" and blank the editor mid-type.
    name_input: Entity<InputState>,
    /// The name `name_input` was seeded from. Re-seeding keys on the NAME
    /// (identity), not the index: keying on the index would leave a deleted
    /// macro's name over the survivor that slides into its slot.
    seeded_name: String,
    /// Which step chip is in edit mode, if any.
    editing: Option<usize>,
    /// The delay chip's ms field, and the step it currently holds a draft for.
    /// Tracked apart from `editing` so a commit can never land on a chip the
    /// user has since moved to.
    delay_input: Entity<InputState>,
    delay_target: Option<usize>,
}

impl Macros {
    pub fn new(store: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Everything on screen (the rail, the step flow, the loaded-set) is
        // daemon-owned, so one observer covers the view. It also carries the two
        // jobs the Dioxus build did in `use_effect`s: the body prefetch and the
        // rename-draft re-seed.
        cx.observe_in(&store, window, |this, _, window, cx| {
            this.prefetch_selected(cx);
            this.reseed_name(window, cx);
            cx.notify();
        })
        .detach();

        let picker = cx.new(|cx| KeyPicker::new(window, cx));
        cx.subscribe(&picker, |this, _, ev: &KeyPickerEvent, cx| match ev {
            KeyPickerEvent::Picked(key) => this.apply_pick(key, cx),
            // Escape closes the panel only; a chip in edit mode stays in edit
            // mode, as it did when each chip owned its own picker.
            KeyPickerEvent::Dismissed => {
                this.picking = None;
                cx.notify();
            }
        })
        .detach();

        let seeded_name = {
            let s = store.read(cx);
            s.macros
                .get(s.selected_macro)
                .map(|m| m.name.clone())
                .unwrap_or_default()
        };
        let name_input =
            cx.new(|cx| InputState::new(window, cx).default_value(seeded_name.clone()));
        // Enter and blur both commit — the old title field's `onchange`.
        cx.subscribe_in(
            &name_input,
            window,
            |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_rename(window, cx);
                    cx.notify();
                }
            },
        )
        .detach();

        let delay_input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe(&delay_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_delay(cx);
            }
        })
        .detach();

        let this = Self {
            store,
            picker,
            picking: None,
            name_input,
            seeded_name,
            editing: None,
            delay_input,
            delay_target: None,
        };
        // Prefetch on mount as well as on every store write: after a live load
        // the selected macro is a name-only stub that no click ever fetched.
        this.prefetch_selected(cx);
        this
    }

    // --- Store-driven housekeeping -----------------------------------------

    /// Fetch the selected macro's body while it is still a name-only stub.
    /// Gated on `macros_loaded` (not on the step count) so a genuinely-empty
    /// loaded macro is not re-fetched forever; `store::fetch_macro` marks the
    /// name loaded even when the daemon rejects it, so this always terminates.
    /// No-op offline, where every macro is local.
    fn prefetch_selected(&self, cx: &mut App) {
        let pending = {
            let s = self.store.read(cx);
            s.macros.get(s.selected_macro).and_then(|m| {
                (!s.offline && !s.macros_loaded.contains(&m.name)).then(|| m.name.clone())
            })
        };
        if let Some(name) = pending {
            store::fetch_macro(&self.store, name, cx);
        }
    }

    /// Re-point the rename draft when the selected macro's identity changes.
    /// Typing never comes through here — the draft is local — so this fires only
    /// when the selection moves or the store renames underneath the editor.
    fn reseed_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = {
            let s = self.store.read(cx);
            s.macros
                .get(s.selected_macro)
                .map(|m| m.name.clone())
                .unwrap_or_default()
        };
        if name == self.seeded_name {
            return;
        }
        self.set_name_draft(name, window, cx);
        // A different macro is in the editor now, so whatever chip or picker was
        // open belonged to the old one.
        self.editing = None;
        self.delay_target = None;
        self.picking = None;
    }

    fn set_name_draft(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.seeded_name = name.clone();
        self.name_input
            .update(cx, |state, cx| state.set_value(name, window, cx));
    }

    // --- Inline editors -----------------------------------------------------

    /// Commit and close whichever inline editor is open. Called before anything
    /// that opens another editor or moves the selection: GPUI delivers the
    /// outgoing field's `Blur` AFTER the new state is installed, so the blur
    /// handler alone would commit against the wrong macro — or drop the edit
    /// entirely, once the draft has been re-seeded.
    fn end_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_rename(window, cx);
        self.commit_delay(cx);
    }

    /// Commit the rename draft. Refuses an empty name, a no-op, and a collision
    /// with another macro; all three snap the draft back to the real name.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_name = self.name_input.read(cx).value().trim().to_string();
        let sel = self.store.read(cx).selected_macro;
        // ONE store write: rename the macro AND rekey the loaded-set, else the
        // loaded test recomputes against the stale name and blanks the editor.
        let renamed = self.store.update(cx, |s, cx| {
            // Renaming ONTO another macro's name must be refused: daemon-side
            // SaveMacro is a blind by-name upsert, so a collision would
            // overwrite that macro's steps and leave two same-named rows here,
            // breaking every by-name operation (delete, the loaded-set,
            // GetMacro, Macro binds). The create path de-dupes with
            // `unique_name` for exactly this reason. Computed before the
            // `get_mut` below — an iter in the match guard would overlap the
            // mutable borrow.
            let collides = s
                .macros
                .iter()
                .enumerate()
                .any(|(j, x)| j != sel && x.name == new_name);
            match s.macros.get_mut(sel) {
                Some(m) if !new_name.is_empty() && m.name != new_name && !collides => {
                    let old = std::mem::replace(&mut m.name, new_name.clone());
                    let m = m.clone();
                    s.macros_loaded.remove(&old);
                    s.macros_loaded.insert(new_name.clone());
                    cx.notify();
                    Some((old, m))
                }
                _ => None,
            }
        });
        let Some((old, m)) = renamed else {
            // Empty, unchanged, or colliding: snap the draft back to the store's
            // name, which this path left untouched.
            let real = self
                .store
                .read(cx)
                .macros
                .get(sel)
                .map(|m| m.name.clone())
                .unwrap_or_default();
            self.set_name_draft(real, window, cx);
            return;
        };
        // The draft already matches the store; keep the seed in step so the
        // observer that follows this write does not reset the caret.
        self.seeded_name = m.name.clone();
        // SaveMacro inserts BY NAME (there is no RenameMacro), so the old name
        // would orphan on the daemon and reappear as a duplicate on the next
        // load — delete it after saving the renamed body.
        store::fire(&self.store, ipc::req_save_macro(m), cx);
        store::fire(&self.store, ipc::req_delete_macro(old), cx);
    }

    /// Commit the ms draft (Enter or blur) and close the chip. Parse failures
    /// clamp to 0 and the value caps at [`MAX_MACRO_DELAY_MS`]: the daemon
    /// rejects over-cap delays, so the editor must never produce one.
    fn commit_delay(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.delay_target.take() else {
            return;
        };
        if self.editing == Some(index) {
            self.editing = None;
        }
        let ms = self
            .delay_input
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .unwrap_or(0)
            .min(MAX_MACRO_DELAY_MS);
        let sel = self.store.read(cx).selected_macro;
        mutate_and_save(&self.store, sel, cx, move |m| {
            match m.steps.get_mut(index) {
                Some(MacroStep::Delay { ms: current }) if *current != ms => {
                    *current = ms;
                    true
                }
                _ => false,
            }
        });
        cx.notify();
    }

    /// Click a chip into edit mode. A delay chip seeds and focuses the ms field;
    /// a key chip reveals its picker trigger.
    fn edit_step(
        &mut self,
        index: usize,
        delay_ms: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_edits(window, cx);
        self.editing = Some(index);
        if let Some(ms) = delay_ms {
            self.delay_target = Some(index);
            self.delay_input.update(cx, |state, cx| {
                state.set_value(ms.to_string(), window, cx);
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    // --- Key picker ---------------------------------------------------------

    /// Open the shared picker for `target`. The picker's search box takes focus,
    /// which blurs whatever field was open, so commit that edit here rather than
    /// leave it to the late blur.
    fn open_picker(&mut self, target: PickTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(window, cx);
        self.picking = Some(target);
        if let PickTarget::Step(i) = target {
            self.editing = Some(i);
        }
        self.picker
            .update(cx, |picker, cx| picker.reset(window, cx));
        cx.notify();
    }

    /// Route an incoming pick to whatever opened the picker, then close it.
    fn apply_pick(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let Some(target) = self.picking.take() else {
            return;
        };
        let sel = self.store.read(cx).selected_macro;
        let key = key.to_string();
        match target {
            PickTarget::Tap => mutate_and_save(&self.store, sel, cx, move |m| {
                m.steps.push(MacroStep::Press { key: key.clone() });
                m.steps.push(MacroStep::Release { key });
                true
            }),
            PickTarget::KeyDown => mutate_and_save(&self.store, sel, cx, move |m| {
                m.steps.push(MacroStep::Press { key });
                true
            }),
            PickTarget::KeyUp => mutate_and_save(&self.store, sel, cx, move |m| {
                m.steps.push(MacroStep::Release { key });
                true
            }),
            PickTarget::Step(index) => {
                self.editing = None;
                mutate_and_save(&self.store, sel, cx, move |m| {
                    match m.steps.get_mut(index) {
                        Some(MacroStep::Press { key: slot } | MacroStep::Release { key: slot }) => {
                            *slot = key;
                            true
                        }
                        _ => false,
                    }
                });
            }
        }
        cx.notify();
    }

    // --- Mutations ----------------------------------------------------------

    /// Select a rail row. Live-loaded macros arrive name-only (ListMacros
    /// carries no bodies), so the first selection of one with an empty body
    /// fetches its steps.
    fn select_macro(
        &mut self,
        i: usize,
        needs_body: bool,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_edits(window, cx);
        self.store.update(cx, |s, cx| {
            s.selected_macro = i;
            cx.notify();
        });
        if needs_body {
            store::fetch_macro(&self.store, name, cx);
        }
    }

    /// Delete a macro from the rail. Refuses to empty the list — there is always
    /// at least one macro.
    fn delete_macro(
        &mut self,
        i: usize,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.end_edits(window, cx);
        if self.store.read(cx).macros.len() <= 1 {
            return;
        }
        self.store.update(cx, |s, cx| {
            // Delete by the RENDERED INDEX, not by name: duplicate names (two
            // "New Macro" clicks) make a by-name lookup remove the wrong (first)
            // row. A stale index — a double event, or a resync that shrank the
            // list — is skipped, since Vec::remove would panic past the end.
            if i < s.macros.len() {
                s.macros.remove(i);
                // Drop the loaded flag only if no other macro still shares this
                // name (the set is keyed by name, so a same-named survivor must
                // keep it).
                if !s.macros.iter().any(|x| x.name == name) {
                    s.macros_loaded.remove(&name);
                }
            }
            let len = s.macros.len();
            adjust_index_after_remove(&mut s.selected_macro, i, len);
            cx.notify();
        });
        store::fire(&self.store, ipc::req_delete_macro(name), cx);
    }

    fn new_macro(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(window, cx);
        let created = self.store.update(cx, |s, cx| {
            // De-dupe the name: identity-by-name (delete, SaveMacro, the
            // loaded-set) breaks when two macros share one.
            let name = unique_name("New Macro", |n| s.macros.iter().any(|m| m.name == n));
            let created = Macro {
                name: name.clone(),
                steps: Vec::new(),
            };
            s.macros.push(created.clone());
            // A brand-new local macro has its (empty) body in hand; mark it
            // loaded so the editor does not block on a fetch.
            s.macros_loaded.insert(name);
            s.selected_macro = s.macros.len() - 1;
            cx.notify();
            created
        });
        store::fire(&self.store, ipc::req_save_macro(created), cx);
    }

    fn add_delay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(window, cx);
        let sel = self.store.read(cx).selected_macro;
        mutate_and_save(&self.store, sel, cx, |m| {
            m.steps.push(MacroStep::Delay { ms: 50 });
            true
        });
    }

    fn clear_steps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(window, cx);
        let sel = self.store.read(cx).selected_macro;
        mutate_and_save(&self.store, sel, cx, |m| {
            if m.steps.is_empty() {
                return false;
            }
            m.steps.clear();
            true
        });
    }

    fn remove_step(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(window, cx);
        let sel = self.store.read(cx).selected_macro;
        mutate_and_save(&self.store, sel, cx, move |m| {
            // `index` is a render snapshot; a double event could make it stale,
            // so guard before Vec::remove (which would otherwise panic).
            if index < m.steps.len() {
                m.steps.remove(index);
                true
            } else {
                false
            }
        });
        cx.notify();
    }

    // --- Pieces -------------------------------------------------------------

    /// The picker panel, anchored under whatever opened it. The panel sets no
    /// position of its own (see [`KeyPicker`]), so the anchor lives at the call
    /// site's `relative()` wrapper. Deferred at the priority the Profiles screen
    /// uses for its menus, so it paints over the cards and over the click-away
    /// catcher.
    fn picker_panel(&self) -> impl IntoElement {
        deferred(
            div()
                .absolute()
                .top(px(38.))
                .left(px(0.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(self.picker.clone()),
        )
        .with_priority(2)
    }

    /// One rail row: name, step count, and the delete ✕.
    fn macro_row(
        &self,
        i: usize,
        m: &Macro,
        sel: usize,
        mono: &SharedString,
        cx: &Context<Self>,
    ) -> AnyElement {
        let is_active = i == sel;
        let name = m.name.clone();
        let steps = m.steps.len();
        // Live-loaded macros arrive name-only; fetch the full steps the first
        // time one with an empty body is selected.
        let needs_body = m.steps.is_empty();
        let fetch_name = name.clone();
        let del_name = name.clone();

        div()
            .id(("macro-row", i))
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(13.))
            .py(px(12.))
            .rounded(px(theme::RADIUS_SM))
            .bg(if is_active {
                theme::rust_wash()
            } else {
                theme::panel_0()
            })
            .border_1()
            .border_color(if is_active {
                theme::rust()
            } else {
                theme::line_soft()
            })
            .when(!is_active, |el| {
                el.hover(|s| s.bg(theme::panel_1()).border_color(theme::line()))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.select_macro(i, needs_body, fetch_name.clone(), window, cx)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::ink())
                            .truncate()
                            .child(name),
                    )
                    .child(
                        div()
                            .pt(px(2.))
                            .font_family(mono.clone())
                            .text_size(px(11.))
                            .text_color(theme::mute())
                            .child(format!("{steps} steps")),
                    ),
            )
            .child(chip_x(
                ("macro-row-x", i),
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.delete_macro(i, del_name.clone(), window, cx)
                }),
            ))
            .into_any_element()
    }

    /// The editor's title row. It doubles as the rename field, exactly as the
    /// CSS build had it: borderless until hovered, committing once on blur or
    /// Enter.
    fn title_row(&self, m: &Macro, mono: &SharedString, cx: &Context<Self>) -> impl IntoElement {
        card_head(mono)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .rounded(px(theme::RADIUS_SM))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .hover(|s| s.border_color(theme::line_soft()))
                    .font_family(theme::sans(cx))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::ink())
                    .child(
                        Input::new(&self.name_input)
                            .appearance(false)
                            .text_size(px(13.5)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme::rust())
                    .child(format!("{} steps", m.steps.len())),
            )
    }

    /// The add-step toolbar. The three key controls are pickers whose trigger
    /// reads the action ("+ Tap") instead of a generic "Choose key…", so they
    /// stay distinguishable.
    fn toolbar(&self, mono: &SharedString, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(9.))
            .rounded(px(theme::RADIUS_SM))
            .bg(theme::panel_0())
            .border_1()
            .border_color(theme::line_soft())
            .child(self.add_button(PickTarget::Tap, 0, "+ Tap", mono, cx))
            .child(self.add_button(PickTarget::KeyDown, 1, "+ Key down", mono, cx))
            .child(self.add_button(PickTarget::KeyUp, 2, "+ Key up", mono, cx))
            .child(btn_ghost(
                "macro-add-delay",
                "+ Delay",
                cx.listener(|this, _: &ClickEvent, window, cx| this.add_delay(window, cx)),
            ))
            .child(btn_danger(
                "macro-clear",
                "Clear",
                cx.listener(|this, _: &ClickEvent, window, cx| this.clear_steps(window, cx)),
            ))
    }

    fn add_button(
        &self,
        target: PickTarget,
        slot: usize,
        label: &'static str,
        mono: &SharedString,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .relative()
            .flex_none()
            .w(px(124.))
            .child(key_picker_trigger(
                ("macro-add", slot),
                "",
                Some(label),
                mono.clone(),
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_picker(target, window, cx)
                }),
            ))
            .when(self.picking == Some(target), |el| {
                el.child(self.picker_panel())
            })
    }

    /// One step chip. Numbering is earned here — steps are a real sequence.
    fn step_chip(
        &self,
        index: usize,
        step: &MacroStep,
        mono: &SharedString,
        cx: &Context<Self>,
    ) -> AnyElement {
        let skin = chip_skin(step);
        let editing = self.editing == Some(index);
        let delay_ms = match step {
            MacroStep::Delay { ms } => Some(*ms),
            _ => None,
        };

        let chip = div()
            .id(("macro-step", index))
            .relative()
            .flex()
            .items_center()
            .gap(px(7.))
            .px(px(skin.pad_x))
            .py(px(skin.pad_y))
            .rounded(px(theme::RADIUS_SM))
            .bg(skin.bg)
            .border_1()
            // Edit mode takes the rust border. The CSS also painted a 1px ring
            // outside it, which has no GPUI equivalent.
            .border_color(if editing { theme::rust() } else { skin.border })
            .font_family(mono.clone())
            .text_size(px(skin.text))
            .text_color(skin.ink)
            .cursor_pointer()
            .child(
                div()
                    .flex_none()
                    .min_w(px(16.))
                    .h(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(3.))
                    .bg(skin.num_bg)
                    .text_size(px(10.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(skin.num_ink)
                    .child((index + 1).to_string()),
            );

        let remove = chip_x(
            ("macro-step-x", index),
            cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.remove_step(index, window, cx)
            }),
        );

        match (editing, step) {
            // DELAY in edit mode — the ms field, committed on Enter or blur.
            (true, MacroStep::Delay { .. }) => chip
                .child(
                    div().w(px(64.)).child(
                        Input::new(&self.delay_input)
                            .appearance(false)
                            .h(px(16.))
                            .px(px(0.))
                            .py(px(0.))
                            .text_size(px(skin.text)),
                    ),
                )
                .child(div().child("ms"))
                .child(remove)
                .into_any_element(),
            // PRESS / RELEASE in edit mode — re-pick the key.
            (true, MacroStep::Press { key } | MacroStep::Release { key }) => chip
                .child(div().flex_none().w(px(150.)).child(key_picker_trigger(
                    ("macro-step-key", index),
                    key,
                    None,
                    mono.clone(),
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_picker(PickTarget::Step(index), window, cx)
                    }),
                )))
                .child(remove)
                .when(self.picking == Some(PickTarget::Step(index)), |el| {
                    el.child(self.picker_panel())
                })
                .into_any_element(),
            // Every kind renders the same chip until it is clicked into edit
            // mode; only the inline editors above differ.
            (false, _) => chip
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.edit_step(index, delay_ms, window, cx)
                }))
                .child(div().child(step_label(step)))
                .child(remove)
                .into_any_element(),
        }
    }
}

impl Render for Macros {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot what the screen needs, then drop the borrow before building
        // listeners (the shell's convention).
        let (macros, sel, selected_loaded) = {
            let s = self.store.read(cx);
            // A macro's body is "loaded" when its name is in `macros_loaded`
            // (local and mock macros, plus any GetMacro'd ones). Offline
            // everything is local, so treat all as loaded. This is what tells a
            // name-only stub from a genuinely-empty macro.
            let loaded = s.offline
                || s.macros
                    .get(s.selected_macro)
                    .is_none_or(|m| s.macros_loaded.contains(&m.name));
            (s.macros.clone(), s.selected_macro, loaded)
        };
        let mono = theme::mono(cx);

        // --- LEFT: the narrow macro name rail -------------------------------
        let rail = card()
            .flex_none()
            .w(px(230.))
            .child(card_head(&mono).child("MACROS"))
            .child(if macros.is_empty() {
                hint("No macros yet.", &mono).into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children(
                        macros
                            .iter()
                            .enumerate()
                            .map(|(i, m)| self.macro_row(i, m, sel, &mono, cx)),
                    )
                    .into_any_element()
            })
            .child(divider())
            .child(btn_primary(
                "macro-new",
                "New macro",
                cx.listener(|this, _: &ClickEvent, window, cx| this.new_macro(window, cx)),
            ));

        // --- RIGHT: the chip-flow editor ------------------------------------
        let editor = card().flex_1().min_w(px(0.));
        let editor = match macros.get(sel) {
            None => editor
                .child(card_head(&mono).child("EDITOR"))
                .child(hint(
                    "Select a macro from the list, or create a new one.",
                    &mono,
                )),
            // A live stub whose body is still in flight. Editing stays blocked
            // until it lands, so a SaveMacro cannot persist a truncated (empty)
            // body over the daemon's real steps.
            Some(m) if !selected_loaded => editor
                .child(card_head(&mono).child(m.name.clone()))
                .child(hint("Loading macro steps from the daemon…", &mono)),
            Some(m) => editor
                .child(self.title_row(m, &mono, cx))
                .child(self.toolbar(&mono, cx))
                .child(divider())
                .child(if m.steps.is_empty() {
                    hint(
                        "No steps yet — use + Tap, + Key down/up, or + Delay to build the sequence.",
                        &mono,
                    )
                    .into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .children(
                            m.steps
                                .iter()
                                .enumerate()
                                .map(|(i, step)| self.step_chip(i, step, &mono, cx)),
                        )
                        .into_any_element()
                }),
        };

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .p(px(22.))
            .child(page_head(
                "Create and edit keystroke sequences that can be bound to any key.",
            ))
            .child(
                // The two cards hug their content (the old grid's
                // `align-items: start`), so the pair scrolls as one when either
                // the rail or the step flow outgrows the window.
                div()
                    .id("macros-body")
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .gap(px(22.))
                    .overflow_y_scroll()
                    .child(rail)
                    .child(editor),
            )
            // The picker's click-away catcher, deferred so it sits above the
            // cards but below the panel itself (priority 2).
            .when(self.picking.is_some(), |el| {
                el.child(deferred(
                    div()
                        .id("macro-pick-dismiss")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.picking = None;
                                cx.notify();
                            }),
                        ),
                ))
            })
    }
}
