//! Customize — the keypad layout + per-key bind editor. Hypershift is a SECOND
//! keyboard layout, not a toggle: the stage stacks two keypads (Base above,
//! Hypershift below), each editing its OWN layer's binds. Clicking a cap in
//! either keypad selects that (key, layer) for the right-hand editor.
//!
//! Binds are the canonical `tartarus_ipc::BindAction`, stored in the active
//! `Profile` keyed by evdev name. Every edit updates the local store
//! (optimistic) and fires `SetBind` to the daemon (no-op when offline).
//!
//! The right-hand `BindEditor` is the advanced, low-error binding panel:
//! every key choice goes through the searchable [`KeyPicker`] (no free-text
//! evdev typing), chords are editable as removable pills, and Macro / Profile
//! targets are dropdowns of what actually exists. Type switching is
//! NON-DESTRUCTIVE — picking a chip changes the *pending* editor type locally
//! and only commits (`set_bind`) once a real value is chosen (value-less
//! Hypershift / Disable commit immediately).

use dioxus::prelude::*;

use crate::components::{KeyPicker, PageHead};
use crate::model::{bind_value_label, key_label, BindAction, KeyId, Layer};
use crate::use_store;

#[component]
pub fn Customize() -> Element {
    let store = use_store();
    let selected = store.read().selected;

    rsx! {
        PageHead {
            title: "Customize",
            sub: "Click any input to remap it. The Hypershift keypad below edits the secondary layer held under your thumb.",
        }

        div { class: "work",
            // Stage: two stacked keypads — Base above, Hypershift below. Each
            // keypad shows/edits ITS OWN layer's binds and, on cap click, sets
            // store.selected + store.layer (via its `layer` prop). The keypad
            // renders its own 4/8-way thumb toggle.
            div { class: "stage",
                div { class: "keypad-stack",
                    div { class: "keypad-layer",
                        div { class: "layer-label",
                            span { class: "dot" }
                            "Base"
                        }
                        Keypad { mode: KeypadMode::Bind, layer: Layer::Base }
                    }
                    div { class: "keypad-layer",
                        div { class: "layer-label shift",
                            span { class: "dot" }
                            "Hypershift"
                        }
                        Keypad { mode: KeypadMode::Bind, layer: Layer::Hypershift }
                    }
                }
            }

            // Inspector: bind editor for the selected key. Keyed on the key +
            // layer so switching selection / layer remounts the editor and
            // re-seeds its pending-type state cleanly.
            div { class: "inspector",
                match selected {
                    Some(_) => rsx! { BindEditor { } },
                    None => rsx! {
                        div { class: "empty-inspector",
                            div { class: "big", "⌖" }
                            div { "Select a key to edit its binding" }
                        }
                    },
                }
            }
        }
    }
}

use crate::components::{Keypad, KeypadMode};

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

#[component]
fn BindEditor() -> Element {
    let mut store = use_store();
    let s = store.read();
    let key_id = s.selected.unwrap();
    let layer = s.layer;

    // The scroll wheel emits a relative axis, not a key, so it isn't remappable
    // as a bind in M1. Show a note instead of the editor.
    if !key_id.is_bindable() {
        let cap = key_id.cap();
        drop(s);
        return rsx! {
            div { class: "card",
                div { class: "card-title",
                    "Binding"
                    span { class: "val", "{cap}" }
                }
                div { class: "empty-inspector",
                    div { class: "big", "⟳" }
                    div { "The scroll wheel reports a relative axis, not a key —" }
                    div { style: "color:var(--text-mute);", "remapping it is pending analog/axis support." }
                }
            }
        };
    }

    let bind = s.bind_for(key_id, layer);
    let cap = key_id.cap();
    let current_kind = Kind::of(bind.as_ref());

    // The *other* layer's bind, read-only, for at-a-glance reference.
    let other_layer = match layer {
        Layer::Base => Layer::Hypershift,
        Layer::Hypershift => Layer::Base,
    };
    let other_bind = s.bind_for(key_id, other_layer);
    let other_value = bind_value_label(other_bind.as_ref());
    drop(s);

    // Pending editor kind — `None` means "follow the stored bind". A chip click
    // sets it locally (non-destructive); a value pick commits and clears it.
    //
    // BindEditor is NOT remounted on selection change (it sits in a `match`
    // arm, not an iterator — Dioxus `key:` only reconciles list children, so it
    // wouldn't reset `use_signal`). Instead, reset `pending` explicitly when the
    // selected key OR layer changes, so a half-switched type can't leak across
    // selections and overwrite a different key's bind. `peek()` reads without
    // subscribing, so this comparison doesn't itself trigger re-renders.
    let mut pending = use_signal(|| None::<Kind>);
    let mut prev_sel = use_signal(|| (key_id, layer));
    if *prev_sel.peek() != (key_id, layer) {
        prev_sel.set((key_id, layer));
        pending.set(None);
    }
    let active_kind = pending().unwrap_or(current_kind);

    let current_value = bind_value_label(bind.as_ref());

    rsx! {
        div { class: "card",
            // Readable binding header: the physical key as a chip, then what it
            // currently does, at a legible size (replaces the small red value tag).
            div { class: "bind-head",
                span { class: "bind-key", "{cap}" }
                span { class: "bind-sep", "→" }
                span { class: "bind-now", "{current_value}" }
            }

            // ── Type chips — selecting one switches the editor locally. Only
            // value-less kinds (Hypershift / Disable) commit on click.
            div { class: "field",
                label { class: "field-label", "Bind type" }
                div { class: "select-grid",
                    for kind in Kind::ALL {
                        div {
                            class: if active_kind == kind { "chip active" } else { "chip" },
                            onclick: move |_| {
                                if kind.is_valueless() {
                                    let action = match kind {
                                        Kind::Hypershift => BindAction::Hypershift,
                                        _ => BindAction::Disable,
                                    };
                                    commit_bind(&mut store, key_id, Some(action));
                                    pending.set(None);
                                } else {
                                    // Switching to a value-requiring type from a
                                    // committed value-less bind (Disable/Hypershift):
                                    // clear it now so the cap drops its disabled/
                                    // shift state immediately, instead of lingering
                                    // until the user picks a value (bug fix).
                                    if matches!(current_kind, Kind::Disable | Kind::Hypershift) {
                                        commit_bind(&mut store, key_id, None);
                                    }
                                    pending.set(Some(kind));
                                }
                            },
                            "{kind.label()}"
                        }
                    }
                }
            }

            // ── Value editor for the active kind.
            div { class: "field",
                label { class: "field-label", "{value_field_label(active_kind)}" }
                {value_editor(store, key_id, active_kind, bind, pending)}
            }

            div { class: "divider" }

            // ── Reset / clear actions.
            div { class: "btn-row",
                button {
                    class: "btn",
                    onclick: move |_| {
                        commit_bind(&mut store, key_id, None);
                        pending.set(None);
                    },
                    "Reset to default"
                }
            }

            div { class: "divider" }

            // ── The other layer's bind, read-only.
            div { class: "field",
                label { class: "field-label",
                    if layer == Layer::Base { "On Hypershift" } else { "On Base layer" }
                }
                div { class: "field-value", "{other_value}" }
            }
        }
    }
}

/// The value-editor body for a given editor kind. Each variant builds the right
/// low-error control (pills + picker, dropdown, or debounced multi-arg field)
/// and commits through [`commit_bind`].
fn value_editor(
    mut store: Signal<crate::model::AppState>,
    key_id: KeyId,
    kind: Kind,
    bind: Option<BindAction>,
    mut pending: Signal<Option<Kind>>,
) -> Element {
    match kind {
        Kind::Keyboard => {
            // Seed the chord from a Key bind; otherwise start empty.
            let keys = match &bind {
                Some(BindAction::Key { keys }) => keys.clone(),
                _ => Vec::new(),
            };
            rsx! { ChordEditor { store, key_id, keys, pending } }
        }
        Kind::Mouse => {
            let current = match &bind {
                Some(BindAction::MouseButton { button }) => button.clone(),
                _ => String::new(),
            };
            rsx! {
                MousePicker { store, key_id, current, pending }
            }
        }
        Kind::Macro => {
            let names: Vec<String> = store.read().macros.iter().map(|m| m.name.clone()).collect();
            let current = match &bind {
                Some(BindAction::Macro { name }) => name.clone(),
                _ => String::new(),
            };
            rsx! {
                NamePicker {
                    names,
                    current,
                    placeholder: "Choose a macro…",
                    empty_msg: "No macros yet — create one on the Macros screen.",
                    on_pick: move |name| {
                        commit_bind(&mut store, key_id, Some(BindAction::Macro { name }));
                        pending.set(None);
                    },
                }
            }
        }
        Kind::Profile => {
            let names: Vec<String> = store.read().profiles.iter().map(|p| p.name.clone()).collect();
            let current = match &bind {
                Some(BindAction::SwitchProfile { name }) => name.clone(),
                _ => String::new(),
            };
            rsx! {
                NamePicker {
                    names,
                    current,
                    placeholder: "Choose a profile…",
                    empty_msg: "No profiles available.",
                    on_pick: move |name| {
                        commit_bind(&mut store, key_id, Some(BindAction::SwitchProfile { name }));
                        pending.set(None);
                    },
                }
            }
        }
        Kind::Launch => {
            let command = match &bind {
                Some(BindAction::LaunchProgram { command }) => command.clone(),
                _ => Vec::new(),
            };
            rsx! { LaunchEditor { store, key_id, command, pending } }
        }
        // Value-less kinds: just confirm the active state (the chip committed it).
        Kind::Hypershift => rsx! {
            div { class: "field-value", style: "color:var(--text-mute);", "Holds the secondary layer while pressed." }
        },
        Kind::Disable => rsx! {
            div { class: "field-value", style: "color:var(--text-mute);", "Key is consumed and emits nothing." }
        },
    }
}

/// Field label for the active kind's value control.
fn value_field_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Keyboard => "Keys (chord)",
        Kind::Mouse => "Mouse button",
        Kind::Macro => "Macro",
        Kind::Launch => "Command",
        Kind::Profile => "Profile",
        Kind::Hypershift => "Hypershift",
        Kind::Disable => "Disabled",
    }
}

// --- value controls ---------------------------------------------------------

/// Commit a chord (or clear the bind when empty). A free helper so each pill
/// handler can call it independently — signals are `Copy`, so there's no shared
/// `FnMut` closure to borrow.
fn commit_chord(
    mut store: Signal<crate::model::AppState>,
    key_id: KeyId,
    mut pending: Signal<Option<Kind>>,
    new_keys: Vec<String>,
) {
    let action = if new_keys.is_empty() {
        None
    } else {
        Some(BindAction::Key { keys: new_keys })
    };
    commit_bind(&mut store, key_id, action);
    pending.set(None);
}

/// Editable key chord: each key is a removable pill; `+ add key` appends another
/// via a KeyPicker. Commits a `Key{keys}` on every mutation (or clears the bind
/// when the last key is removed). Multimedia keys are pickable here too (the
/// KeyPicker's Media category), so there's no separate multimedia editor.
#[component]
fn ChordEditor(
    store: Signal<crate::model::AppState>,
    key_id: KeyId,
    keys: Vec<String>,
    pending: Signal<Option<Kind>>,
) -> Element {
    let mut adding = use_signal(|| false);

    rsx! {
        div { class: "chord-pills",
            for (i, name) in keys.iter().enumerate() {
                div { class: "pill",
                    "{key_label(name)}"
                    span {
                        class: "pill-x",
                        onclick: {
                            let keys = keys.clone();
                            move |_| {
                                let mut next = keys.clone();
                                next.remove(i);
                                commit_chord(store, key_id, pending, next);
                            }
                        },
                        "×"
                    }
                }
            }

            if adding() {
                KeyPicker {
                    value: "".to_string(),
                    on_pick: {
                        let keys = keys;
                        move |picked: String| {
                            let mut next = keys.clone();
                            next.push(picked);
                            adding.set(false);
                            commit_chord(store, key_id, pending, next);
                        }
                    },
                }
            } else {
                button {
                    class: "pill-add",
                    onclick: move |_| adding.set(true),
                    if keys.is_empty() { "+ pick a key" } else { "+ add key" }
                }
            }
        }
    }
}

/// Single mouse button via a KeyPicker (Mouse category lists the BTN_*). Commits
/// `MouseButton{button}` on pick.
#[component]
fn MousePicker(
    store: Signal<crate::model::AppState>,
    key_id: KeyId,
    current: String,
    pending: Signal<Option<Kind>>,
) -> Element {
    let mut store = store;
    let mut pending = pending;
    rsx! {
        KeyPicker {
            value: current,
            on_pick: move |picked: String| {
                commit_bind(&mut store, key_id, Some(BindAction::MouseButton { button: picked }));
                pending.set(None);
            },
        }
    }
}

/// Dropdown over a list of names (macros, profiles) → one picked name. The
/// empty state text and the committed bind stay caller-side, so this holds no
/// bind-kind knowledge.
#[component]
fn NamePicker(
    names: Vec<String>,
    current: String,
    placeholder: &'static str,
    empty_msg: &'static str,
    on_pick: EventHandler<String>,
) -> Element {
    if names.is_empty() {
        return rsx! {
            div { class: "field-value", style: "color:var(--text-mute);", "{empty_msg}" }
        };
    }

    rsx! {
        div { class: "profile-select",
            select {
                onchange: move |e| {
                    let name = e.value();
                    if !name.is_empty() {
                        on_pick.call(name);
                    }
                },
                option {
                    value: "",
                    selected: current.is_empty(),
                    disabled: true,
                    "{placeholder}"
                }
                for name in names {
                    option {
                        value: "{name}",
                        selected: name == current,
                        "{name}"
                    }
                }
            }
        }
    }
}

/// Multi-arg launch command editor: a single space-split field → `command:
/// Vec<String>` (today only argv[0] was editable). Debounced via `onchange` so
/// it commits on blur/Enter, not per keystroke.
#[component]
fn LaunchEditor(
    store: Signal<crate::model::AppState>,
    key_id: KeyId,
    command: Vec<String>,
    pending: Signal<Option<Kind>>,
) -> Element {
    let mut store = store;
    let mut pending = pending;
    let text = command.join(" ");

    rsx! {
        input {
            class: "text-input",
            value: "{text}",
            placeholder: "e.g. firefox --new-window",
            // Debounced: commit on blur / Enter (onchange), not per keystroke,
            // so a SetBind doesn't fire on every character (review #5).
            onchange: move |e| {
                let argv: Vec<String> = e.value()
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                let action = if argv.is_empty() {
                    None
                } else {
                    Some(BindAction::LaunchProgram { command: argv })
                };
                commit_bind(&mut store, key_id, action);
                pending.set(None);
            },
        }
        div { class: "field-value", style: "color:var(--text-mute);margin-top:7px;",
            "Space-separated: first token is the program, the rest are arguments."
        }
    }
}

// --- commit ----------------------------------------------------------------

/// Update the local store + push the change to the daemon. No-op for inputs
/// that aren't bindable keys (the scroll wheel emits a relative axis, not KEY_*),
/// and while a profile switch is in flight (the body is about to be replaced, so
/// a SetBind here could target the stale active-profile name — review #1).
fn commit_bind(store: &mut Signal<crate::model::AppState>, key: KeyId, action: Option<BindAction>) {
    let Some(evdev) = key.evdev() else { return };
    if store.read().switching {
        return;
    }
    let (profile, layer) = {
        let mut w = store.write();
        w.set_bind(key, action.clone());
        (w.active_profile_name(), w.layer)
    };
    crate::ipc::set_bind(*store, profile, layer, evdev, action);
}
