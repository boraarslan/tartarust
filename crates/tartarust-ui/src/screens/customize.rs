//! Customize — the keypad layout + per-key bind editor. Hypershift is a SECOND
//! keyboard layout, not a toggle: the stage stacks two keypads (Base above,
//! Hypershift below), each editing its OWN layer's binds. Clicking a cap in
//! either keypad selects that (key, layer) for the right-hand editor.
//!
//! Binds are the canonical `tartarust_ipc::BindAction`, stored in the active
//! `Profile` keyed by evdev name. Every edit updates the local store
//! (optimistic) and fires `SetBind` to the daemon (no-op when offline).
//!
//! The right-hand bind editor is the advanced, low-error binding panel: every
//! key choice goes through the searchable [`KeyPicker`] (no free-text evdev
//! typing), chords are editable as removable pills, and Macro / Profile targets
//! are dropdowns of what actually exists. Type switching is NON-DESTRUCTIVE —
//! picking a chip changes the *pending* editor type locally and only commits
//! (`SetBind`) once a real value is chosen (value-less Hypershift / Disable
//! commit immediately).
//!
//! Under Dioxus this was five components, each reading the global store and
//! holding its own signals. GPUI wants one view: [`Customize`] owns every piece
//! of editor state — the pending type, the one shared picker, the launch field —
//! while the keypads take their binds and selection as plain values and hand
//! interactions back through callbacks.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, FontWeight, MouseButton, SharedString,
    Window, deferred, div, linear_color_stop, linear_gradient,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::components::widgets::{btn_ghost, card, page_head};
use crate::components::{KeyPicker, KeyPickerEvent, Keypad, KeypadMode, key_picker_trigger};
use crate::ipc;
use crate::model::{
    AppState, BindAction, KeyId, Layer, ThumbAxisMode, bind_value_label, key_label,
};
use crate::store;
use crate::theme;

// ---------------------------------------------------------------------------
// Geometry. The stylesheet sized the work area in viewport units
// (`clamp(290px, 27vw, 380px)` for the inspector column); the window is fixed
// at 1280x800, so that clamp is resolved once, here.
// ---------------------------------------------------------------------------

/// Inspector column width (CSS `clamp(290px, 27vw, 380px)` at 1280px).
const INSPECTOR_W: f32 = 345.0;
/// Gap between the stage and the inspector (CSS `.work`).
const WORK_GAP: f32 = 22.0;
/// Vertical gap between the two stacked keypads (CSS `.keypad-stack`).
const STACK_GAP: f32 = 26.0;

/// The editor's bind "kind" — what value UI to show. Decoupled from the stored
/// `BindAction` so a chip click can switch the editor type WITHOUT overwriting
/// the bind until the user actually picks a value (non-destructive switching).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Keyboard,
    Mouse,
    Macro,
    Launch,
    Profile,
    Hypershift,
    Disable,
}

impl Kind {
    /// Editor chips, in display order.
    const ALL: [Kind; 7] = [
        Kind::Keyboard,
        Kind::Mouse,
        Kind::Macro,
        Kind::Launch,
        Kind::Profile,
        Kind::Hypershift,
        Kind::Disable,
    ];

    fn label(self) -> &'static str {
        match self {
            Kind::Keyboard => "Keyboard",
            Kind::Mouse => "Mouse",
            Kind::Macro => "Macro",
            Kind::Launch => "Launch",
            Kind::Profile => "Switch Profile",
            Kind::Hypershift => "Hypershift",
            Kind::Disable => "Disable",
        }
    }

    /// Field label for this kind's value control.
    fn value_label(self) -> &'static str {
        match self {
            Kind::Keyboard => "Keys (chord)",
            Kind::Mouse => "Mouse button",
            Kind::Macro => "Macro",
            Kind::Launch => "Command",
            Kind::Profile => "Profile",
            Kind::Hypershift => "Hypershift",
            Kind::Disable => "Disabled",
        }
    }

    /// The editor kind that matches an existing bind (so the panel seeds from
    /// the current value). `None`/`Passthrough`/`Key` all map sensibly; a
    /// Keyboard bind covers both plain keys and multimedia keys (media keys are
    /// pickable via the KeyPicker's Media category).
    fn of(action: Option<&BindAction>) -> Kind {
        match action {
            Some(BindAction::MouseButton { .. }) => Kind::Mouse,
            Some(BindAction::Macro { .. }) => Kind::Macro,
            Some(BindAction::LaunchProgram { .. }) => Kind::Launch,
            Some(BindAction::SwitchProfile { .. }) => Kind::Profile,
            Some(BindAction::Hypershift) => Kind::Hypershift,
            Some(BindAction::Disable) => Kind::Disable,
            // Key, Passthrough, None (unbound) all default to the Keyboard editor.
            _ => Kind::Keyboard,
        }
    }

    /// True for value-less binds that commit the instant their chip is clicked.
    fn is_valueless(self) -> bool {
        matches!(self, Kind::Hypershift | Kind::Disable)
    }
}

/// What the one shared [`KeyPicker`] is currently picking for. The Dioxus
/// picker owned its trigger and its open flag, so the chord's `adding` bool and
/// the mouse editor's picker were separate instances; the ported picker is a
/// panel the parent shows and anchors, so a single flag says both whether it is
/// open and where the pick goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// Append a key to the selected key's chord.
    Chord,
    /// Replace the selected key's mouse button.
    Mouse,
}

/// Everything the inspector renders from, snapshotted once per frame so the
/// editor pieces don't each re-read the store.
struct Editor {
    key: KeyId,
    layer: Layer,
    /// The selected key's bind on the edit layer.
    bind: Option<BindAction>,
    /// The OTHER layer's bind, already rendered, for the read-only footer.
    other_value: String,
    macros: Vec<String>,
    profiles: Vec<String>,
    mono: SharedString,
}

/// The Customize screen.
pub struct Customize {
    store: Entity<AppState>,
    /// The one searchable key picker, shown for whichever control opened it.
    picker: Entity<KeyPicker>,
    /// What the picker is open for; `None` = closed.
    picking: Option<Pick>,
    /// Whether the Macro / Profile name dropdown is open.
    names_open: bool,
    /// The Launch editor's text field. Commits on Enter/blur, never per
    /// keystroke, so a `SetBind` can't fire on every character.
    launch_input: Entity<InputState>,
    /// Which (key, layer) [`Customize::launch_input`] holds the command for.
    /// Also the commit's guard: a blur delivered after the selection moved must
    /// not write the old text onto the new key.
    launch_seed: Option<(KeyId, Layer)>,
    /// Pending editor kind — `None` means "follow the stored bind". A chip
    /// click sets it locally (non-destructive); a value pick commits and clears
    /// it.
    pending: Option<Kind>,
    /// The (key, layer) `pending` belongs to. The editor is not rebuilt from
    /// scratch when the selection moves, so a half-switched type would
    /// otherwise leak across selections and overwrite a different key's bind.
    prev_sel: Option<(KeyId, Layer)>,
}

impl Customize {
    pub fn new(store: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Every store write repaints the screen: the keypads render from the
        // profile's binds, the inspector from the selection.
        cx.observe(&store, |_, _, cx| cx.notify()).detach();

        let picker = cx.new(|cx| KeyPicker::new(window, cx));
        cx.subscribe(&picker, |this, _, ev: &KeyPickerEvent, cx| match ev {
            KeyPickerEvent::Picked(evdev) => this.on_pick(evdev, cx),
            KeyPickerEvent::Dismissed => {
                this.picking = None;
                cx.notify();
            }
        })
        .detach();

        let launch_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("e.g. firefox --new-window"));
        // Enter and blur both commit — the old editor's debounced `onchange`.
        cx.subscribe(&launch_input, |this, state, ev: &InputEvent, cx| {
            if !matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                return;
            }
            let text = state.read(cx).value().to_string();
            this.commit_launch(&text, cx);
        })
        .detach();

        Self {
            store,
            picker,
            picking: None,
            names_open: false,
            launch_input,
            launch_seed: None,
            pending: None,
            prev_sel: None,
        }
    }

    // --- commit ------------------------------------------------------------

    /// Update the local store + push the change to the daemon. No-op for inputs
    /// that aren't bindable keys (the scroll wheel emits a relative axis, not
    /// `KEY_*`), and while a profile switch is in flight — the body is about to
    /// be replaced, so a `SetBind` here could target the stale active-profile
    /// name.
    fn commit_bind(&mut self, key: KeyId, action: Option<BindAction>, cx: &mut Context<Self>) {
        let Some(evdev) = key.evdev() else { return };
        if self.store.read(cx).switching {
            return;
        }
        let (profile, layer) = self.store.update(cx, |s, cx| {
            s.set_bind(key, action.clone());
            cx.notify();
            (s.active_profile_name(), s.layer)
        });
        store::fire(
            &self.store,
            ipc::req_set_bind(profile, layer, evdev.to_string(), action),
            cx,
        );
    }

    /// Commit a chord — or clear the bind when the last key was removed.
    fn commit_chord(&mut self, key: KeyId, keys: Vec<String>, cx: &mut Context<Self>) {
        let action = (!keys.is_empty()).then_some(BindAction::Key { keys });
        self.commit_bind(key, action, cx);
        self.pending = None;
    }

    /// Commit the Launch field's text as `argv`. It targets the key the field
    /// was SEEDED from, and only while that is still the live selection: a blur
    /// arriving after the user clicked another cap must not retarget.
    fn commit_launch(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some((key, layer)) = self.launch_seed else {
            return;
        };
        let (selected, edit_layer) = {
            let s = self.store.read(cx);
            (s.selected, s.layer)
        };
        if selected != Some(key) || edit_layer != layer {
            return;
        }
        let argv: Vec<String> = text.split_whitespace().map(|s| s.to_string()).collect();
        let action = (!argv.is_empty()).then_some(BindAction::LaunchProgram { command: argv });
        self.commit_bind(key, action, cx);
        self.pending = None;
        cx.notify();
    }

    /// A chip click. Value-less kinds commit immediately; the rest only switch
    /// the editor locally — except that leaving a COMMITTED value-less bind
    /// clears it now, so the cap drops its disabled/shift state immediately
    /// instead of lingering until a value is picked.
    fn set_kind(&mut self, kind: Kind, key: KeyId, current: Kind, cx: &mut Context<Self>) {
        if kind.is_valueless() {
            let action = match kind {
                Kind::Hypershift => BindAction::Hypershift,
                _ => BindAction::Disable,
            };
            self.commit_bind(key, Some(action), cx);
            self.pending = None;
        } else {
            if matches!(current, Kind::Disable | Kind::Hypershift) {
                self.commit_bind(key, None, cx);
            }
            self.pending = Some(kind);
        }
        // A different value editor is on screen now; whatever the old one had
        // open belongs to it.
        self.picking = None;
        self.names_open = false;
        cx.notify();
    }

    /// A key came back from the picker. Where it goes depends on which control
    /// opened it.
    fn on_pick(&mut self, evdev: &'static str, cx: &mut Context<Self>) {
        let Some(target) = self.picking.take() else {
            return;
        };
        let (selected, layer) = {
            let s = self.store.read(cx);
            (s.selected, s.layer)
        };
        let Some(key) = selected else {
            cx.notify();
            return;
        };
        match target {
            Pick::Chord => {
                let mut keys = match self.store.read(cx).bind_for(key, layer) {
                    Some(BindAction::Key { keys }) => keys,
                    _ => Vec::new(),
                };
                keys.push(evdev.to_string());
                self.commit_chord(key, keys, cx);
            }
            Pick::Mouse => {
                self.commit_bind(
                    key,
                    Some(BindAction::MouseButton {
                        button: evdev.to_string(),
                    }),
                    cx,
                );
                self.pending = None;
            }
        }
        cx.notify();
    }

    /// Open (or close) the picker for `target`, re-arming its search box.
    fn toggle_picker(&mut self, target: Pick, window: &mut Window, cx: &mut Context<Self>) {
        self.names_open = false;
        if self.picking == Some(target) {
            self.picking = None;
        } else {
            self.picking = Some(target);
            let picker = self.picker.clone();
            picker.update(cx, |p, cx| p.reset(window, cx));
        }
        cx.notify();
    }

    // --- inspector ---------------------------------------------------------

    /// The bind editor card for the selected key.
    fn bind_editor(&self, e: &Editor, cx: &Context<Self>) -> AnyElement {
        let current_kind = Kind::of(e.bind.as_ref());
        let active_kind = self.pending.unwrap_or(current_kind);
        let key = e.key;

        // Readable binding header: the physical key as a chip, then what it
        // currently does, at a legible size.
        let head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(theme::sp(10.))
            .child(
                div()
                    .px(theme::sp(12.))
                    .py(theme::sp(4.))
                    .rounded(theme::sp(theme::RADIUS_SM))
                    .bg(machined_face())
                    .border_1()
                    .border_color(theme::line())
                    .font_family(e.mono.clone())
                    .text_size(theme::sp(15.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::ink())
                    .child(key.cap()),
            )
            .child(
                div()
                    .text_size(theme::sp(14.))
                    .text_color(theme::mute())
                    .child("→"),
            )
            .child(
                div()
                    .font_family(e.mono.clone())
                    .text_size(theme::sp(13.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::rust())
                    .child(bind_value_label(e.bind.as_ref())),
            );

        // Type chips, two to a row — the stylesheet's `1fr 1fr` grid.
        let mut chip_rows: Vec<AnyElement> = Vec::new();
        for (r, pair) in Kind::ALL.chunks(2).enumerate() {
            let mut row = div().flex().gap(theme::sp(6.));
            for (c, &kind) in pair.iter().enumerate() {
                let active = active_kind == kind;
                row = row.child(
                    div()
                        .id(("cust-chip", r * 2 + c))
                        .flex_1()
                        .px(theme::sp(10.))
                        .py(theme::sp(8.))
                        .rounded(theme::sp(theme::RADIUS_SM))
                        .border_1()
                        .text_size(theme::sp(12.))
                        // Layout centering, NOT `.text_center()`: the text-align
                        // refinement gets lost under the hover style pass, which
                        // left-aligned hovered chips (user review).
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .when(active, |el| {
                            el.border_color(theme::rust())
                                .bg(theme::rust_wash())
                                .text_color(theme::ink())
                        })
                        .when(!active, |el| {
                            el.border_color(theme::line())
                                .bg(theme::panel_1())
                                .text_color(theme::dim())
                                .hover(|s| s.border_color(theme::mute()).text_color(theme::ink()))
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_kind(kind, key, current_kind, cx)
                        }))
                        .child(kind.label()),
                );
            }
            // A lone trailing chip keeps its half-width column. The spacer
            // mirrors the chip's padding + border (invisibly): flex bases are
            // content-box, so a BARE flex_1 spacer concedes its padding/border
            // width to the chip and the lone chip renders wider than a
            // gridded one (user review: Disable overflowed the grid).
            if pair.len() == 1 {
                row = row.child(
                    div()
                        .flex_1()
                        .px(theme::sp(10.))
                        .border_1()
                        .border_color(gpui::transparent_black()),
                );
            }
            chip_rows.push(row.into_any_element());
        }

        let other_label = if e.layer == Layer::Base {
            "On Hypershift"
        } else {
            "On Base layer"
        };

        card()
            .child(head)
            .child(field(
                "Bind type",
                &e.mono,
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::sp(6.))
                    .children(chip_rows),
            ))
            .child(field(
                active_kind.value_label(),
                &e.mono,
                self.value_editor(e, active_kind, cx),
            ))
            .child(divider())
            .child(btn_ghost(
                "cust-reset",
                "Reset to default",
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.commit_bind(key, None, cx);
                    this.pending = None;
                    cx.notify();
                }),
            ))
            .child(divider())
            // The other layer's bind, read-only, for at-a-glance reference.
            .child(field(
                other_label,
                &e.mono,
                field_value(e.other_value.clone(), &e.mono),
            ))
            .into_any_element()
    }

    /// The value-editor body for the active kind. Each variant builds the right
    /// low-error control — pills + picker, dropdown, or commit-on-blur field.
    fn value_editor(&self, e: &Editor, kind: Kind, cx: &Context<Self>) -> AnyElement {
        match kind {
            Kind::Keyboard => {
                // Seed the chord from a Key bind; otherwise start empty.
                let keys = match &e.bind {
                    Some(BindAction::Key { keys }) => keys.clone(),
                    _ => Vec::new(),
                };
                self.chord_editor(e, keys, cx)
            }
            Kind::Mouse => {
                let current = match &e.bind {
                    Some(BindAction::MouseButton { button }) => button.clone(),
                    _ => String::new(),
                };
                div()
                    .relative()
                    .child(key_picker_trigger(
                        "cust-mouse",
                        &current,
                        None,
                        e.mono.clone(),
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_picker(Pick::Mouse, window, cx)
                        }),
                    ))
                    .when(self.picking == Some(Pick::Mouse), |el| {
                        el.child(self.picker_panel(42.))
                    })
                    .into_any_element()
            }
            Kind::Macro => {
                let current = match &e.bind {
                    Some(BindAction::Macro { name }) => name.clone(),
                    _ => String::new(),
                };
                self.name_picker(e, Kind::Macro, current, cx)
            }
            Kind::Profile => {
                let current = match &e.bind {
                    Some(BindAction::SwitchProfile { name }) => name.clone(),
                    _ => String::new(),
                };
                self.name_picker(e, Kind::Profile, current, cx)
            }
            // Multi-arg launch command: one space-split field -> `command:
            // Vec<String>`. The field is seeded (and re-seeded on a new
            // selection) by `Customize::render`.
            Kind::Launch => div()
                .flex()
                .flex_col()
                .child(div().w_full().child(Input::new(&self.launch_input)))
                .child(
                    div()
                        .mt(theme::sp(7.))
                        .font_family(e.mono.clone())
                        .text_size(theme::sp(13.))
                        .text_color(theme::mute())
                        .child(
                            "Space-separated: first token is the program, the rest are arguments.",
                        ),
                )
                .into_any_element(),
            // Value-less kinds: just confirm the active state (the chip committed it).
            Kind::Hypershift => field_note("Holds the secondary layer while pressed.", &e.mono),
            Kind::Disable => field_note("Key is consumed and emits nothing.", &e.mono),
        }
    }

    /// Editable key chord: each key is a removable pill; `+ add key` appends
    /// another through the [`KeyPicker`]. Commits a `Key{keys}` on every
    /// mutation (or clears the bind when the last key goes). Multimedia keys are
    /// pickable here too (the picker's Media category), so there is no separate
    /// multimedia editor.
    fn chord_editor(&self, e: &Editor, keys: Vec<String>, cx: &Context<Self>) -> AnyElement {
        let key = e.key;
        let mut row = div().flex().flex_wrap().items_center().gap(theme::sp(7.));

        for (i, name) in keys.iter().enumerate() {
            let chord = keys.clone();
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap(theme::sp(7.))
                    .pl(theme::sp(11.))
                    .pr(theme::sp(8.))
                    .py(theme::sp(6.))
                    .rounded(theme::sp(theme::RADIUS_SM))
                    .bg(machined_face())
                    .border_1()
                    .border_color(theme::line())
                    .font_family(e.mono.clone())
                    .text_size(theme::sp(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::ink())
                    .child(key_label(name))
                    .child(
                        div()
                            .id(("cust-pill-x", i))
                            .flex()
                            .flex_none()
                            .size(theme::sp(16.))
                            .items_center()
                            .justify_center()
                            .rounded(theme::sp(3.))
                            .text_size(theme::sp(13.))
                            .text_color(theme::mute())
                            .hover(|s| s.bg(theme::red_wash()).text_color(theme::red()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                let mut next = chord.clone();
                                // A stale index (a resync shortened the chord
                                // between render and click) is a no-op, not a
                                // panic.
                                if i >= next.len() {
                                    return;
                                }
                                next.remove(i);
                                this.commit_chord(key, next, cx);
                                cx.notify();
                            }))
                            .child("×"),
                    ),
            );
        }

        let open = self.picking == Some(Pick::Chord);
        let add_label = if keys.is_empty() {
            "+ pick a key"
        } else {
            "+ add key"
        };
        div()
            .relative()
            .child(
                row.child(
                    div()
                        .id("cust-chord-add")
                        .flex()
                        .items_center()
                        .gap(theme::sp(5.))
                        .px(theme::sp(11.))
                        .py(theme::sp(6.))
                        .rounded(theme::sp(theme::RADIUS_SM))
                        .border_1()
                        .border_dashed()
                        .border_color(if open { theme::rust() } else { theme::line() })
                        .text_size(theme::sp(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if open { theme::ink() } else { theme::dim() })
                        .hover(|s| s.border_color(theme::rust()).text_color(theme::ink()))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.toggle_picker(Pick::Chord, window, cx)
                        }))
                        .child(add_label),
                ),
            )
            .when(open, |el| el.child(self.picker_panel(38.)))
            .into_any_element()
    }

    /// Dropdown over the names that actually exist (macros, profiles) → one
    /// picked name. `kind` decides both which list is offered and which bind the
    /// pick commits.
    fn name_picker(
        &self,
        e: &Editor,
        kind: Kind,
        current: String,
        cx: &Context<Self>,
    ) -> AnyElement {
        let key = e.key;
        let (names, placeholder, empty_msg) = if kind == Kind::Macro {
            (
                &e.macros,
                "Choose a macro…",
                "No macros yet — create one on the Macros screen.",
            )
        } else {
            (&e.profiles, "Choose a profile…", "No profiles available.")
        };
        if names.is_empty() {
            return field_note(empty_msg, &e.mono);
        }

        let chosen = !current.is_empty();
        let trigger = div()
            .id("cust-name-trigger")
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
            .font_family(e.mono.clone())
            .text_size(theme::sp(12.5))
            .text_color(if chosen { theme::ink() } else { theme::dim() })
            .hover(|s| s.bg(theme::panel_2()).border_color(theme::mute()))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.names_open = !this.names_open;
                this.picking = None;
                cx.notify();
            }))
            .child(if chosen {
                current.clone()
            } else {
                placeholder.to_string()
            })
            .child(
                div()
                    .text_size(theme::sp(9.))
                    .text_color(theme::dim())
                    .child("▾"),
            );

        let mono = e.mono.clone();
        let list = div()
            .id("cust-name-list")
            .absolute()
            .top(theme::sp(42.))
            .left_0()
            .right_0()
            .max_h(theme::sp(240.))
            .flex()
            .flex_col()
            .p(theme::sp(4.))
            .rounded(theme::sp(theme::RADIUS))
            .bg(theme::panel_1())
            .border_1()
            .border_color(theme::line())
            .shadow_lg()
            .overflow_y_scroll()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(names.iter().enumerate().map(|(i, name)| {
                let picked = name.clone();
                let is_current = *name == current;
                div()
                    .id(("cust-name-row", i))
                    .px(theme::sp(8.))
                    .py(theme::sp(5.))
                    .rounded(theme::sp(theme::RADIUS_SM))
                    .font_family(mono.clone())
                    .text_size(theme::sp(12.))
                    .text_color(if is_current {
                        theme::rust()
                    } else {
                        theme::ink()
                    })
                    .hover(|s| s.bg(theme::panel_2()))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.names_open = false;
                        let action = if kind == Kind::Macro {
                            BindAction::Macro {
                                name: picked.clone(),
                            }
                        } else {
                            BindAction::SwitchProfile {
                                name: picked.clone(),
                            }
                        };
                        this.commit_bind(key, Some(action), cx);
                        this.pending = None;
                        cx.notify();
                    }))
                    .child(name.clone())
            }));

        div()
            .relative()
            .child(trigger)
            .when(self.names_open, |el| {
                el.child(deferred(list).with_priority(2))
            })
            .into_any_element()
    }

    /// The picker panel, anchored `top` below whichever trigger opened it.
    /// Deferred so it paints above the rest of the inspector — priority 2, one
    /// above the click-away catcher (the old z-index 20/30 pair).
    fn picker_panel(&self, top: f32) -> AnyElement {
        deferred(
            div()
                .absolute()
                .top(theme::sp(top))
                .left_0()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(self.picker.clone()),
        )
        .with_priority(2)
        .into_any_element()
    }
}

impl Render for Customize {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = theme::mono(cx);

        // Snapshot what the screen needs, then drop the borrow before building
        // listeners (the shell's convention).
        let (selected, layer, base_binds, shift_binds, axis_mode, bind, other_bind) = {
            let s = self.store.read(cx);
            let other = match s.layer {
                Layer::Base => Layer::Hypershift,
                Layer::Hypershift => Layer::Base,
            };
            (
                s.selected,
                s.layer,
                s.profile.layer(Layer::Base).clone(),
                s.profile.layer(Layer::Hypershift).clone(),
                s.profile.thumb_axis_mode,
                s.selected.and_then(|k| s.bind_for(k, s.layer)),
                s.selected.and_then(|k| s.bind_for(k, other)),
            )
        };

        // The editor is not rebuilt when the selection moves, so reset the
        // pending type — and anything the previous editor had open — whenever
        // the (key, layer) target changes. Without this a half-switched type
        // could leak across selections and overwrite a different key's bind.
        if let Some(key) = selected
            && self.prev_sel != Some((key, layer))
        {
            self.prev_sel = Some((key, layer));
            self.pending = None;
            self.picking = None;
            self.names_open = false;
            self.launch_seed = None;
        }

        // Seed the Launch field when its editor first shows for a target. Only
        // on a real change, so what the user typed survives the re-render every
        // commit causes.
        let launch_target = selected.filter(|k| k.is_bindable()).and_then(|k| {
            let kind = self.pending.unwrap_or_else(|| Kind::of(bind.as_ref()));
            (kind == Kind::Launch).then_some((k, layer))
        });
        match launch_target {
            Some(target) if self.launch_seed != Some(target) => {
                let text = match &bind {
                    Some(BindAction::LaunchProgram { command }) => command.join(" "),
                    _ => String::new(),
                };
                let input = self.launch_input.clone();
                input.update(cx, |s, cx| s.set_value(text, window, cx));
                self.launch_seed = Some(target);
            }
            None => self.launch_seed = None,
            _ => {}
        }

        // ── Cap clicks: select (key, layer), or deselect when the same cap on
        // the same board is clicked again. The keypad hands back its own layer,
        // so a click also says WHICH board it came from.
        let store = self.store.clone();
        // Both handlers are `Rc<dyn Fn(..)>` to the keypad; the closures carry
        // explicit parameter types so they stay higher-ranked over the `&mut
        // Window` / `&mut App` lifetimes, and unsize at the builder call.
        let on_select = Rc::new(
            move |key: KeyId, board: Layer, _window: &mut Window, cx: &mut App| {
                store.update(cx, |s, cx| {
                    if s.selected == Some(key) && s.layer == board {
                        s.selected = None;
                    } else {
                        s.selected = Some(key);
                        s.layer = board;
                    }
                    cx.notify();
                });
            },
        );

        // ── The thumbpad hub's 4-way/8-way toggle: an optimistic local write
        // (clearing a now-hidden diagonal selection) plus a persist.
        let store = self.store.clone();
        let on_axis_mode = Rc::new(
            move |mode: ThumbAxisMode, _window: &mut Window, cx: &mut App| {
                let profile = store.read(cx).active_profile_name();
                store.update(cx, |s, cx| {
                    s.profile.thumb_axis_mode = mode;
                    // Dropping to Four hides the diagonals; clear a selection
                    // that just vanished so the inspector doesn't dangle on an
                    // invisible cap.
                    if mode == ThumbAxisMode::Four
                        && let Some(KeyId::Thumb(d)) = s.selected
                        && d.is_diagonal()
                    {
                        s.selected = None;
                    }
                    cx.notify();
                });
                store::fire(&store, ipc::req_set_thumb_axis_mode(profile, mode), cx);
            },
        );

        // One labelled board. Selection is layer-scoped: only the board holding
        // the edit layer highlights its cap.
        let board = |board_layer: Layer, binds: BTreeMap<String, BindAction>| -> Div {
            let shift = board_layer == Layer::Hypershift;
            let accent = if shift { theme::amber() } else { theme::rust() };
            div()
                .flex()
                .flex_col()
                .gap(theme::sp(12.))
                // The stylesheet ruled the second board off from the first.
                .when(shift, |el| {
                    el.pt(theme::sp(22.))
                        .border_t_1()
                        .border_color(theme::line_soft())
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(theme::sp(8.))
                        .font_family(mono.clone())
                        .text_size(theme::sp(10.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(if shift { theme::amber() } else { theme::mute() })
                        .child(div().size(theme::sp(7.)).rounded_full().bg(accent))
                        .child(if shift { "HYPERSHIFT" } else { "BASE" }),
                )
                .child(
                    Keypad::new(KeypadMode::Bind, board_layer)
                        .binds(binds)
                        .thumb_axis_mode(axis_mode)
                        .selected(selected.filter(|_| board_layer == layer))
                        .on_select(on_select.clone())
                        .on_axis_mode(on_axis_mode.clone()),
                )
        };

        // ── Stage: two stacked keypads — Base above, Hypershift below.
        let stage = div()
            .relative()
            .flex()
            .flex_col()
            .pt(theme::sp(28.))
            .px(theme::sp(26.))
            .pb(theme::sp(24.))
            .rounded(theme::sp(8.))
            .bg(theme::panel_0())
            .border_1()
            .border_color(theme::line_soft())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::sp(STACK_GAP))
                    .child(board(Layer::Base, base_binds))
                    .child(board(Layer::Hypershift, shift_binds)),
            )
            // Registration marks: two L-shaped corner ticks, like a bench-mat
            // print (the stylesheet's `.stage::before` / `::after`).
            .child(
                div()
                    .absolute()
                    .top(theme::sp(9.))
                    .left(theme::sp(9.))
                    .size(theme::sp(11.))
                    .border_t_1()
                    .border_l_1()
                    .border_color(theme::line()),
            )
            .child(
                div()
                    .absolute()
                    .bottom(theme::sp(9.))
                    .right(theme::sp(9.))
                    .size(theme::sp(11.))
                    .border_b_1()
                    .border_r_1()
                    .border_color(theme::line()),
            );

        // ── Inspector: the bind editor for the selected key.
        let inspector = match selected {
            // The scroll wheel emits a relative axis, not a key, so it isn't
            // remappable as a bind in M1. Show a note instead of the editor.
            Some(key) if !key.is_bindable() => card()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .font_family(mono.clone())
                        .text_size(theme::sp(10.))
                        .text_color(theme::mute())
                        .child("BINDING")
                        .child(div().text_color(theme::rust()).child(key.cap())),
                )
                .child(
                    empty_panel()
                        .child(big_glyph("⟳", &mono))
                        .child("The scroll wheel reports a relative axis, not a key —")
                        .child("remapping it is pending analog/axis support."),
                )
                .into_any_element(),
            Some(key) => {
                let editor = Editor {
                    key,
                    layer,
                    bind,
                    other_value: bind_value_label(other_bind.as_ref()),
                    macros: self
                        .store
                        .read(cx)
                        .macros
                        .iter()
                        .map(|m| m.name.clone())
                        .collect(),
                    profiles: self
                        .store
                        .read(cx)
                        .profiles
                        .iter()
                        .map(|p| p.name.clone())
                        .collect(),
                    mono: mono.clone(),
                };
                self.bind_editor(&editor, cx)
            }
            None => empty_panel()
                .child(big_glyph("⌖", &mono))
                .child("Select a key to edit its binding")
                .into_any_element(),
        };

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .p(theme::sp(22.))
            .child(page_head(
                "Click any input to remap it. The Hypershift keypad below edits the secondary \
                 layer held under your thumb.",
            ))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(theme::sp(WORK_GAP))
                    // The two stacked boards are taller than the window, so the
                    // stage column scrolls instead of the whole screen.
                    .child(
                        div()
                            .id("cust-stage")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .child(stage),
                    )
                    .child(div().flex_none().w(theme::sp(INSPECTOR_W)).child(inspector)),
            )
            // Click-away for the picker / name dropdown, deferred so it sits
            // above the page but below the panel itself (priority 2).
            .when(self.names_open || self.picking.is_some(), |el| {
                el.child(deferred(
                    div().id("cust-dismiss").absolute().inset_0().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.names_open = false;
                            this.picking = None;
                            cx.notify();
                        }),
                    ),
                ))
            })
    }
}

// --- small pieces ------------------------------------------------------------

/// The machined plate the key chip and the chord pills sit on (CSS
/// `linear-gradient(180deg, var(--panel-2), var(--panel-1))`).
fn machined_face() -> gpui::Background {
    linear_gradient(
        180.,
        linear_color_stop(theme::panel_2(), 0.),
        linear_color_stop(theme::panel_1(), 1.),
    )
}

/// A labelled editor field: the mono caption, then the control.
fn field(label: &str, mono: &SharedString, body: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .mb(theme::sp(7.))
                .font_family(mono.clone())
                .text_size(theme::sp(10.))
                .text_color(theme::mute())
                .child(label.to_uppercase()),
        )
        .child(body)
}

/// A read-only field value (`.field-value`).
fn field_value(text: String, mono: &SharedString) -> impl IntoElement {
    div()
        .font_family(mono.clone())
        .text_size(theme::sp(13.))
        .text_color(theme::ink())
        .child(text)
}

/// A quiet explanatory value — the value-less kinds and the empty lists.
fn field_note(text: &'static str, mono: &SharedString) -> AnyElement {
    div()
        .font_family(mono.clone())
        .text_size(theme::sp(13.))
        .text_color(theme::mute())
        .child(text)
        .into_any_element()
}

/// The dashed empty-state panel (`.empty-inspector`).
fn empty_panel() -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(theme::sp(8.))
        .h(theme::sp(220.))
        .rounded(theme::sp(theme::RADIUS))
        .border_1()
        .border_dashed()
        .border_color(theme::line())
        .text_center()
        .text_size(theme::sp(12.5))
        .text_color(theme::mute())
}

/// The oversized glyph an empty panel leads with.
fn big_glyph(glyph: &'static str, mono: &SharedString) -> impl IntoElement {
    div()
        .font_family(mono.clone())
        .text_size(theme::sp(22.))
        .opacity(0.5)
        .child(glyph)
}

/// A hairline rule between editor sections (`.divider`).
fn divider() -> impl IntoElement {
    div().h(theme::sp(1.)).bg(theme::line_soft())
}
