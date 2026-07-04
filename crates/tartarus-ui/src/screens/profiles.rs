//! Profiles — compact single-column rows + a thin toolbar.
//!
//! One `.profile-toolbar` carries the GLOBAL auto-switch Switch (+ offline note)
//! and a right-aligned "New profile" button; below it a `.list` of `.profile-row`
//! (status | name | link | kebab), one per profile. The old master-detail
//! settings card is gone — name editing, per-app linking, and profile actions all
//! live inline on the row (name-input / link / kebab only).

use dioxus::prelude::*;

use crate::components::{PageHead, Switch};
use crate::model::{adjust_index_after_remove, unique_name, ProfileSummary};
use crate::use_store;

#[component]
pub fn Profiles() -> Element {
    let mut store = use_store();
    let s = store.read();
    let profiles = s.profiles.clone();
    let active_profile = s.active_profile;
    let offline = s.offline;
    // Auto-switch is a GLOBAL/per-device setting carried in DaemonStatus.
    let auto_switch = s.status.as_ref().is_some_and(|st| st.auto_switch);
    // The configured default profile (compared by NAME); now populated.
    let default_profile = s
        .status
        .as_ref()
        .map(|st| st.default_profile.clone())
        .unwrap_or_default();
    drop(s);

    // Which row's kebab menu / inline link input is open (one at a time).
    let mut menu_open = use_signal::<Option<usize>>(|| None);
    let mut link_editing = use_signal::<Option<usize>>(|| None);
    // Which row is being renamed — drives the debounced-rename FREEZE below and
    // the visual edit state.
    let mut editing = use_signal::<Option<usize>>(|| None);

    // The daemon-known name of the row being renamed, for the debounced rename
    // (review #4). Hooks live at the top level (never conditionally), and the
    // reset is keyed on `editing` (the row IDENTITY) — NOT the profile name,
    // which the Name field edits optimistically on every keystroke. Keying on
    // `editing` keeps `committed` frozen at the pre-edit name while typing, so
    // `onchange` fires rename(original -> new); switching rows resets it.
    let mut committed = use_signal(String::new);
    let editing_row = editing();
    use_effect(use_reactive((&editing_row,), move |(row,)| {
        // `peek()` (NON-subscribing) is essential: a plain `read()` would
        // subscribe the effect to every store mutation, so it would re-run on
        // each keystroke and chase the optimistic edit — re-introducing the
        // from==to bug. With `peek` the only trigger is `editing`, so on a focus
        // change we capture the real (un-edited) name and otherwise stay frozen.
        if let Some(i) = row {
            let real = store
                .peek()
                .profiles
                .get(i)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            committed.set(real);
        }
    }));

    rsx! {
        PageHead {
            title: "Profiles",
            sub: "Per-profile key bindings, calibration, and lighting. A profile activates automatically when its linked application comes into focus.",
        }

        // ── Thin toolbar: global auto-switch + offline note + New profile ─────
        div { class: "profile-toolbar",
            Switch {
                on: auto_switch,
                on_toggle: move |_| {
                    // Optimistic flip of the cached status + push to the daemon.
                    let next = !auto_switch;
                    {
                        let mut w = store.write();
                        if let Some(st) = w.status.as_mut() {
                            st.auto_switch = next;
                        }
                    }
                    crate::ipc::set_auto_switch(store, next);
                },
            }
            div { class: "field-value", "Auto-switch by focused app" }
            if offline {
                span { class: "li-meta", "Daemon offline — last-known state shown." }
            }
            button {
                class: "btn primary",
                style: "margin-left:auto;",
                onclick: move |_| {
                    // Optimistic local push + persist (the daemon makes the profile).
                    // De-dupe the name so every daemon op (which addresses BY NAME)
                    // stays unambiguous and the DEFAULT badge can't double-paint.
                    let mut w = store.write();
                    let name = unique_name("New Profile", |n| w.profiles.iter().any(|p| p.name == n));
                    w.profiles.push(ProfileSummary {
                        name: name.clone(),
                        linked_app: None,
                    });
                    drop(w);
                    crate::ipc::create_profile(store, name);
                },
                "New profile"
            }
        }

        // ── Profile rows ──────────────────────────────────────────────────────
        div { class: "list",
            for (i, profile) in profiles.iter().enumerate() {
                {
                    let is_active  = i == active_profile;
                    let is_default = !default_profile.is_empty() && profile.name == default_profile;
                    let name       = profile.name.clone();
                    let linked     = profile.linked_app.clone();

                    let row_class = match (is_active, is_default) {
                        (true,  true)  => "profile-row active is-default",
                        (true,  false) => "profile-row active",
                        (false, true)  => "profile-row is-default",
                        (false, false) => "profile-row",
                    };

                    // Per-closure String clones (handlers each MOVE a name).
                    let name_default = name.clone();
                    let name_link_set = name.clone();
                    let name_link_clear = name.clone();
                    let name_dup = name.clone();

                    rsx! {
                        div { class: row_class,

                            // ── STATUS ────────────────────────────────────────
                            if is_active {
                                span { class: "li-badge live", "ACTIVE" }
                            } else if is_default {
                                span { class: "li-badge default", "DEFAULT" }
                            } else {
                                // No dedicated idle-dot class in the reuse list;
                                // a small inline-styled dot (keypad/key_picker
                                // inline-style precedent) keeps it self-contained.
                                span {
                                    style: "width:7px;height:7px;border-radius:50%;background:var(--text-mute);opacity:0.6;display:inline-block;",
                                }
                            }

                            // ── NAME (inline-editable) ────────────────────────
                            input {
                                class: "li-name-input",
                                id: "pname-{i}",
                                value: "{name}",
                                onfocus: move |_| editing.set(Some(i)),
                                oninput: move |e| {
                                    let new_name = e.value();
                                    let mut w = store.write();
                                    if let Some(p) = w.profiles.get_mut(i) {
                                        p.name = new_name.clone();
                                    }
                                    if i == w.active_profile {
                                        w.profile.name = new_name;
                                    }
                                },
                                onchange: move |e| {
                                    let new_name = e.value();
                                    let from = committed();
                                    if new_name.trim().is_empty() {
                                        // Empty commit: snap the optimistic edit back
                                        // to the committed name and send nothing.
                                        let mut w = store.write();
                                        if let Some(p) = w.profiles.get_mut(i) {
                                            p.name = from.clone();
                                        }
                                        if i == w.active_profile {
                                            w.profile.name = from;
                                        }
                                    } else if new_name != from {
                                        crate::ipc::rename_profile(store, from, new_name.clone());
                                        // Keep `committed` current so a follow-up
                                        // edit (Enter, keep typing) sends from the
                                        // just-committed name, not the stale one.
                                        committed.set(new_name);
                                    }
                                },
                            }

                            // ── LINK (per-app auto-switch, persisted) ─────────
                            div { style: "min-width:0;",
                                if link_editing() == Some(i) {
                                    // Inline commit: set/clear the linked app.
                                    input {
                                        class: "text-input",
                                        autofocus: true,
                                        placeholder: "app id / window class, e.g. cs2.exe",
                                        value: linked.unwrap_or_default(),
                                        onchange: move |e| {
                                            let v = e.value();
                                            let app = if v.trim().is_empty() { None } else { Some(v.trim().to_string()) };
                                            {
                                                let mut w = store.write();
                                                if let Some(p) = w.profiles.get_mut(i) {
                                                    p.linked_app = app.clone();
                                                }
                                            }
                                            crate::ipc::set_profile_linked_app(store, name_link_set.clone(), app);
                                            link_editing.set(None);
                                        },
                                        onfocusout: move |_| link_editing.set(None),
                                    }
                                } else if let Some(app) = linked {
                                    // A linked app: show a chip with a clear (x).
                                    // Dimmed + click-to-enable when auto-switch is off.
                                    if auto_switch {
                                        span { class: "link-chip",
                                            span { class: "glyph", "◆" }
                                            span { "{app}" }
                                            span {
                                                class: "link-x",
                                                onclick: move |_| {
                                                    {
                                                        let mut w = store.write();
                                                        if let Some(p) = w.profiles.get_mut(i) {
                                                            p.linked_app = None;
                                                        }
                                                    }
                                                    crate::ipc::set_profile_linked_app(store, name_link_clear.clone(), None);
                                                },
                                                "×"
                                            }
                                        }
                                    } else {
                                        span {
                                            class: "link-chip disabled",
                                            title: "Auto-switch is off",
                                            onclick: move |_| enable_auto_switch(store),
                                            span { class: "glyph", "◆" }
                                            span { "{app}" }
                                        }
                                    }
                                } else {
                                    // No link: a ghost "+ Link app…" button.
                                    if auto_switch {
                                        button {
                                            class: "link-add",
                                            onclick: move |_| link_editing.set(Some(i)),
                                            "+ Link app…"
                                        }
                                    } else {
                                        button {
                                            class: "link-add disabled",
                                            title: "Auto-switch is off",
                                            onclick: move |_| enable_auto_switch(store),
                                            "+ Link app…"
                                        }
                                    }
                                }
                            }

                            // ── KEBAB ─────────────────────────────────────────
                            div { style: "position:relative;",
                                button {
                                    class: "kebab",
                                    onclick: move |_| {
                                        let next = if menu_open() == Some(i) { None } else { Some(i) };
                                        menu_open.set(next);
                                    },
                                    "⋮"
                                }

                                if menu_open() == Some(i) {
                                    // Backdrop catcher BELOW the .menu (z-index:30,
                                    // hard-coded in style.rs) so menu-item clicks
                                    // land on the items, not this catcher.
                                    div {
                                        style: "position:fixed;inset:0;z-index:20;",
                                        onclick: move |_| menu_open.set(None),
                                    }
                                    div {
                                        class: "menu",
                                        style: "top:calc(100% + 6px); right:0;",
                                        // Set active (hidden if already active).
                                        if !is_active {
                                            button {
                                                class: "menu-item",
                                                onclick: move |_| {
                                                    menu_open.set(None);
                                                    // Switch + reload the body so editor
                                                    // screens follow the new active profile.
                                                    let n = store.read().profiles.get(i).map(|p| p.name.clone());
                                                    if let Some(n) = n {
                                                        crate::ipc::switch_profile_and_reload(store, n);
                                                    }
                                                },
                                                "Set active"
                                            }
                                        }
                                        // Set as default (disabled/marked if already).
                                        button {
                                            class: if is_default { "menu-item disabled" } else { "menu-item" },
                                            onclick: move |_| {
                                                menu_open.set(None);
                                                if !is_default {
                                                    crate::ipc::set_default_profile(store, name_default.clone());
                                                }
                                            },
                                            if is_default { "Default ✓" } else { "Set as default" }
                                        }
                                        // Duplicate: the daemon has no deep-copy, so this
                                        // creates a NEW EMPTY profile named "<n> (copy)"
                                        // and persists it. Binds/lighting are NOT copied.
                                        button {
                                            class: "menu-item",
                                            onclick: move |_| {
                                                menu_open.set(None);
                                                let mut w = store.write();
                                                let copy = unique_name(&format!("{name_dup} (copy)"), |n| w.profiles.iter().any(|p| p.name == n));
                                                w.profiles.push(ProfileSummary {
                                                    name: copy.clone(),
                                                    linked_app: None,
                                                });
                                                drop(w);
                                                crate::ipc::create_profile(store, copy);
                                            },
                                            "Duplicate"
                                        }
                                        // Rename: focus the inline name input. Setting
                                        // `editing` is the load-bearing part (freeze +
                                        // visual state); the DOM focus is a nicety.
                                        button {
                                            class: "menu-item",
                                            onclick: move |_| {
                                                menu_open.set(None);
                                                editing.set(Some(i));
                                                let id = format!("pname-{i}");
                                                spawn(async move {
                                                    let _ = document::eval(&format!(
                                                        "document.getElementById('{id}')?.focus();"
                                                    )).await;
                                                });
                                            },
                                            "Rename"
                                        }
                                        div { class: "menu-sep" }
                                        // Delete (guards against removing the last profile).
                                        button {
                                            class: "menu-item danger",
                                            onclick: move |_| {
                                                menu_open.set(None);
                                                let mut w = store.write();
                                                if w.profiles.len() <= 1 {
                                                    return;
                                                }
                                                let n = w.profiles.get(i)
                                                    .map(|p| p.name.clone())
                                                    .unwrap_or_default();
                                                w.profiles.remove(i);
                                                let new_len = w.profiles.len();
                                                adjust_index_after_remove(&mut w.active_profile, i, new_len);
                                                drop(w);
                                                crate::ipc::delete_profile(store, n);
                                            },
                                            "Delete"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Optimistically flip auto-switch ON and push to the daemon. Called when a
/// dimmed (disabled) link control is clicked while auto-switch is off — instead
/// of silently doing nothing, enabling auto-switch makes the link meaningful.
fn enable_auto_switch(mut store: Signal<crate::model::AppState>) {
    {
        let mut w = store.write();
        if let Some(st) = w.status.as_mut() {
            st.auto_switch = true;
        }
    }
    crate::ipc::set_auto_switch(store, true);
}
