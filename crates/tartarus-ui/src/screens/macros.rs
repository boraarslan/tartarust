//! Macros screen — name rail + compact chip-flow step editor for named macros.

use dioxus::prelude::*;

use crate::components::KeyPicker;
use crate::model::{key_label, Macro, MacroStep};
use crate::use_store;

// ---------------------------------------------------------------------------
// Local helpers
// ---------------------------------------------------------------------------

/// Pick a macro name that doesn't collide with an existing one. Returns `base`
/// if free, else appends " 2", " 3", … until unique. Macros are identified BY
/// NAME (delete, SaveMacro, the loaded-set), so colliding names corrupt those.
fn unique_macro_name(base: &str, macros: &[Macro]) -> String {
    let taken = |n: &str| macros.iter().any(|m| m.name == n);
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| base.to_string())
}

fn step_kind(s: &MacroStep) -> &'static str {
    match s {
        MacroStep::Press { .. } => "press",
        MacroStep::Release { .. } => "release",
        MacroStep::Delay { .. } => "delay",
    }
}

fn step_label(s: &MacroStep) -> String {
    match s {
        MacroStep::Press { key } | MacroStep::Release { key } => key_label(key),
        MacroStep::Delay { ms } => format!("{ms} ms"),
    }
}

/// Mutate the selected macro's steps in the store, then persist. The closure is
/// only invoked when `sel` indexes a real macro; its return value gates the save
/// (return `false` to skip persistence, e.g. a no-op edit). The whole macro is
/// cloned out under the lock and saved after it's dropped.
fn mutate_and_save(
    mut store: Signal<crate::model::AppState>,
    sel: usize,
    f: impl FnOnce(&mut Macro) -> bool,
) {
    let to_save = {
        let mut w = store.write();
        match w.macros.get_mut(sel) {
            Some(m) => f(m).then(|| m.clone()),
            None => None,
        }
    };
    if let Some(m) = to_save {
        crate::ipc::save_macro(store, m);
    }
}

// ---------------------------------------------------------------------------
// Top-level screen component
// ---------------------------------------------------------------------------

#[component]
pub fn Macros() -> Element {
    let mut store = use_store();
    let s = store.read();
    let macros: Vec<Macro> = s.macros.clone();
    let sel = s.selected_macro;
    let offline = s.offline;
    // A macro's body is "loaded" when its name is in macros_loaded (local/mock
    // macros and any GetMacro'd ones). Offline, everything is local so treat all
    // as loaded. Distinguishes a name-only stub from a genuinely-empty macro.
    let selected_loaded = offline
        || macros
            .get(sel)
            .map(|m| s.macros_loaded.contains(&m.name))
            .unwrap_or(true);
    drop(s);

    // Prefetch the initially-selected macro's body once on mount: after a live
    // load the selected macro is a name-only stub (no click fired to fetch it).
    // Gate on macros_loaded (not step-count) so a genuinely-empty loaded macro
    // isn't re-fetched forever. No-op offline.
    use_effect(move || {
        let s = store.read();
        let pending = s.macros.get(s.selected_macro).and_then(|m| {
            (!s.offline && !s.macros_loaded.contains(&m.name)).then(|| m.name.clone())
        });
        drop(s);
        if let Some(name) = pending {
            crate::ipc::fetch_macro(store, name);
        }
    });

    let selected_macro = macros.get(sel).cloned();
    let macro_count = macros.len();

    rsx! {
        div { class: "page-head",
            h1 { class: "page-title", "Macros" }
            p { class: "page-sub", "Create and edit keystroke sequences that can be bound to any key." }
        }

        div { class: "work macros",
            // LEFT — narrow macro name rail
            div { class: "card",
                div { class: "card-title", "Macros" }

                if macros.is_empty() {
                    div { class: "li-meta", style: "padding: 8px 2px;", "No macros yet." }
                } else {
                    div { class: "list",
                        for (i , m) in macros.iter().enumerate() {
                            {
                                let name = m.name.clone();
                                let step_count = m.steps.len();
                                let is_active = i == sel;
                                // Live-loaded macros arrive name-only (ListMacros has
                                // no bodies); fetch the full steps the first time one
                                // with an empty body is selected. No-op offline.
                                let needs_body = m.steps.is_empty();
                                let fetch_name = name.clone();
                                let del_name = name.clone();
                                rsx! {
                                    div {
                                        class: if is_active { "list-item active" } else { "list-item" },
                                        onclick: move |_| {
                                            store.write().selected_macro = i;
                                            if needs_body {
                                                crate::ipc::fetch_macro(store, fetch_name.clone());
                                            }
                                        },
                                        div { class: "li-main",
                                            div { class: "li-name", "{name}" }
                                            div { class: "li-meta", "{step_count} steps" }
                                        }
                                        // Hover delete — refuse if it would empty the
                                        // list (always keep at least one macro).
                                        span {
                                            class: "chip-x",
                                            title: "Delete macro",
                                            onclick: move |e| {
                                                e.stop_propagation();
                                                if macro_count <= 1 {
                                                    return;
                                                }
                                                {
                                                    let mut w = store.write();
                                                    // Delete by the RENDERED INDEX, not by name:
                                                    // duplicate names (two "New Macro" clicks) make
                                                    // a by-name lookup remove the wrong (first) row.
                                                    if i < w.macros.len() {
                                                        w.macros.remove(i);
                                                        // Drop the loaded flag only if no other macro
                                                        // still shares this name (the set is keyed by
                                                        // name, so a same-named survivor must keep it).
                                                        if !w.macros.iter().any(|x| x.name == del_name) {
                                                            w.macros_loaded.remove(&del_name);
                                                        }
                                                    }
                                                    // Three-case index fix-up so a delete BELOW the
                                                    // selection doesn't mis-point selected_macro.
                                                    let len = w.macros.len();
                                                    if i < w.selected_macro {
                                                        w.selected_macro -= 1;
                                                    } else if w.selected_macro >= len {
                                                        w.selected_macro = len.saturating_sub(1);
                                                    }
                                                }
                                                crate::ipc::delete_macro(store, del_name.clone());
                                            },
                                            "✕"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "divider" }

                button {
                    class: "btn primary",
                    onclick: move |_| {
                        let new_macro = {
                            let mut w = store.write();
                            // De-dupe the name: identity-by-name (delete, SaveMacro,
                            // the loaded-set) breaks when two macros share a name.
                            let name = unique_macro_name("New Macro", &w.macros);
                            let new_macro = Macro { name: name.clone(), steps: vec![] };
                            w.macros.push(new_macro.clone());
                            // A brand-new local macro has its (empty) body in hand;
                            // mark it loaded so the editor doesn't block on a fetch.
                            w.macros_loaded.insert(name);
                            w.selected_macro = w.macros.len() - 1;
                            new_macro
                        };
                        crate::ipc::save_macro(store, new_macro);
                    },
                    "New macro"
                }
            }

            // RIGHT — chip-flow editor
            div { class: "card",
                match selected_macro {
                    None => rsx! {
                        div { class: "card-title", "Editor" }
                        div { class: "li-meta", style: "padding: 8px 2px;",
                            "Select a macro from the list, or create a new one."
                        }
                    },
                    Some(m) => {
                        let macro_name = m.name.clone();
                        let steps = m.steps.clone();
                        rsx! {
                            MacroEditor {
                                macro_name,
                                steps,
                                sel,
                                loaded: selected_loaded,
                            }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Step editor sub-component
// ---------------------------------------------------------------------------

#[component]
fn MacroEditor(
    macro_name: String,
    steps: Vec<MacroStep>,
    sel: usize,
    /// False while this macro is a live name-only stub whose body is still
    /// loading. Editing is blocked until the body arrives so a SaveMacro can't
    /// persist a truncated (empty) body over the daemon's real steps.
    loaded: bool,
) -> Element {
    let mut store = use_store();

    // Rename draft: the name input is bound to a LOCAL signal, not the store, so
    // typing doesn't churn the store (which would flip `selected_loaded` false
    // online — unmounting this editor mid-type — and pre-commit the name so the
    // onchange rename guard always short-circuits). Re-seeded whenever the
    // selected macro's identity (its name) changes — keying on `sel` would leave
    // a deleted macro's name over the survivor that slides into the same index.
    // Hooks live above the early return below so the hook order is stable when
    // `loaded` flips false->true.
    let mut name_draft = use_signal(|| macro_name.clone());
    let mut prev_name = use_signal(|| macro_name.clone());
    if *prev_name.peek() != macro_name {
        prev_name.set(macro_name.clone());
        name_draft.set(macro_name.clone());
    }

    // Until the body loads, show a slim placeholder instead of the editor.
    if !loaded {
        return rsx! {
            div { class: "card-title", "{macro_name}" }
            div { class: "li-meta", style: "padding: 8px 2px;",
                "Loading macro steps from the daemon…"
            }
        };
    }

    let draft_v = name_draft();
    let step_count = steps.len();

    rsx! {
        // Inline-editable title doubles as the rename field. Bound to a LOCAL
        // draft (not the store) so typing doesn't churn the store / unmount the
        // editor; the rename commits once on blur/Enter.
        div { class: "card-title",
            input {
                class: "li-name-input",
                value: "{draft_v}",
                oninput: move |e| name_draft.set(e.value()),
                onchange: move |e| {
                    let new_name = e.value().trim().to_string();
                    // Commit the rename in ONE store write: rename the macro, rekey
                    // the loaded-set (else selected_loaded recomputes against the
                    // stale name and blanks the editor), then persist. The
                    // macros_loaded borrow can't overlap the macros.get_mut borrow,
                    // so do the rename first and capture the old name, then rekey.
                    let saved = {
                        let mut w = store.write();
                        match w.macros.get_mut(sel) {
                            Some(m) if !new_name.is_empty() && m.name != new_name => {
                                let old = std::mem::replace(&mut m.name, new_name.clone());
                                let m = m.clone();
                                w.macros_loaded.remove(&old);
                                w.macros_loaded.insert(new_name.clone());
                                Some((old, m))
                            }
                            // Empty or unchanged: snap the draft back to the real name.
                            _ => {
                                name_draft.set(macro_name.clone());
                                None
                            }
                        }
                    };
                    if let Some((old, m)) = saved {
                        // SaveMacro inserts by name (no RenameMacro), so the old name
                        // would orphan on the daemon and reappear as a duplicate on
                        // the next load — delete it after saving the renamed body.
                        crate::ipc::save_macro(store, m);
                        crate::ipc::delete_macro(store, old);
                    }
                },
            }
            span { class: "val", "{step_count} steps" }
        }

        // Toolbar: rename lives in the title; here are the add-step controls.
        div { class: "macro-toolbar",
            // "+ Tap" — append a Press then a Release of the picked key.
            AddKeyButton {
                label: "+ Tap",
                on_pick: move |key: String| {
                    mutate_and_save(store, sel, move |m| {
                        m.steps.push(MacroStep::Press { key: key.clone() });
                        m.steps.push(MacroStep::Release { key });
                        true
                    });
                },
            }
            // "+ Key down" — append a Press.
            AddKeyButton {
                label: "+ Key down",
                on_pick: move |key: String| {
                    mutate_and_save(store, sel, move |m| {
                        m.steps.push(MacroStep::Press { key });
                        true
                    });
                },
            }
            // "+ Key up" — append a Release.
            AddKeyButton {
                label: "+ Key up",
                on_pick: move |key: String| {
                    mutate_and_save(store, sel, move |m| {
                        m.steps.push(MacroStep::Release { key });
                        true
                    });
                },
            }
            button {
                class: "btn",
                onclick: move |_| {
                    mutate_and_save(store, sel, |m| {
                        m.steps.push(MacroStep::Delay { ms: 50 });
                        true
                    });
                },
                "+ Delay"
            }
            button {
                class: "btn danger",
                onclick: move |_| {
                    mutate_and_save(store, sel, |m| {
                        if m.steps.is_empty() {
                            return false;
                        }
                        m.steps.clear();
                        true
                    });
                },
                "Clear"
            }
        }

        div { class: "divider" }

        if steps.is_empty() {
            div { class: "li-meta", style: "padding: 8px 2px;",
                "No steps yet — use + Tap, + Key down/up, or + Delay to build the sequence."
            }
        } else {
            div { class: "step-flow",
                for (i , step) in steps.iter().enumerate() {
                    StepChip { key: "{i}", index: i, step: step.clone(), sel }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Add-step button that opens a KeyPicker and emits the chosen key
// ---------------------------------------------------------------------------

/// A labelled add-step control: a [`KeyPicker`] whose trigger reads the action
/// label (e.g. "+ Tap") directly via the picker's `placeholder` prop, so the
/// three add buttons are distinguishable instead of three identical "Choose
/// key…" triggers. `on_pick` forwards the chosen evdev name to the caller.
#[component]
fn AddKeyButton(label: String, on_pick: EventHandler<String>) -> Element {
    rsx! {
        KeyPicker {
            value: String::new(),
            placeholder: Some(label),
            on_pick: move |k: String| on_pick.call(k),
        }
    }
}

// ---------------------------------------------------------------------------
// One step chip (press / release / delay) with inline edit
// ---------------------------------------------------------------------------

#[component]
fn StepChip(index: usize, step: MacroStep, sel: usize) -> Element {
    let store = use_store();
    let mut editing = use_signal(|| false);

    let kind = step_kind(&step);
    let label = step_label(&step);
    let class = format!("step-chip {kind}{}", if editing() { " editing" } else { "" });

    // The remove handler is shared by every chip kind.
    let remove = move |e: Event<MouseData>| {
        e.stop_propagation();
        mutate_and_save(store, sel, move |m| {
            // `index` is a render snapshot; a double event could make it stale,
            // so guard before Vec::remove (which would otherwise panic).
            if index < m.steps.len() {
                m.steps.remove(index);
                true
            } else {
                false
            }
        });
    };

    match &step {
        // DELAY — click to edit the ms inline.
        MacroStep::Delay { ms } => {
            let ms = *ms;
            if editing() {
                rsx! {
                    span { class: "{class}",
                        span { class: "num", "{index + 1}" }
                        input {
                            // No dedicated CSS class in the API surface for the
                            // inline ms field; style it small + mono inline to sit
                            // inside the .step-chip.editing wrapper.
                            style: "width:52px; background:transparent; border:none; outline:none; color:var(--amber); font-family:var(--mono); font-size:11.5px;",
                            r#type: "number",
                            min: "0",
                            autofocus: true,
                            value: "{ms}",
                            // Commit on change/blur; clamp parse failures to 0.
                            onchange: move |e| {
                                let new_ms = e.value().parse::<u64>().unwrap_or(0);
                                editing.set(false);
                                mutate_and_save(store, sel, move |m| match m.steps.get_mut(index) {
                                    Some(MacroStep::Delay { ms }) if *ms != new_ms => {
                                        *ms = new_ms;
                                        true
                                    }
                                    _ => false,
                                });
                            },
                            onkeydown: move |e| {
                                if e.key() == Key::Enter {
                                    editing.set(false);
                                }
                            },
                        }
                        span { "ms" }
                        span { class: "chip-x", onclick: remove, "✕" }
                    }
                }
            } else {
                rsx! {
                    span { class: "{class}", onclick: move |_| editing.set(true),
                        span { class: "num", "{index + 1}" }
                        span { "{label}" }
                        span { class: "chip-x", onclick: remove, "✕" }
                    }
                }
            }
        }
        // PRESS / RELEASE — click to re-pick the key via KeyPicker.
        MacroStep::Press { key } | MacroStep::Release { key } => {
            let cur = key.clone();
            if editing() {
                rsx! {
                    span { class: "{class}",
                        span { class: "num", "{index + 1}" }
                        KeyPicker {
                            value: cur,
                            on_pick: move |new_key: String| {
                                editing.set(false);
                                mutate_and_save(store, sel, move |m| match m.steps.get_mut(index) {
                                    Some(MacroStep::Press { key } | MacroStep::Release { key }) => {
                                        *key = new_key;
                                        true
                                    }
                                    _ => false,
                                });
                            },
                        }
                        span { class: "chip-x", onclick: remove, "✕" }
                    }
                }
            } else {
                rsx! {
                    span { class: "{class}", onclick: move |_| editing.set(true),
                        span { class: "num", "{index + 1}" }
                        span { "{label}" }
                        span { class: "chip-x", onclick: remove, "✕" }
                    }
                }
            }
        }
    }
}
