//! Profiles — compact single-column rows + a thin toolbar.
//!
//! One `.profile-toolbar` carries the GLOBAL auto-switch Switch (+ offline note)
//! and a right-aligned "New profile" button; below it a `.list` of `.profile-row`
//! (status | name | link | kebab), one per profile. The old master-detail
//! settings card is gone — name editing, per-app linking, and profile actions all
//! live inline on the row (name-input / link / kebab only).

use dioxus::prelude::*;

use crate::components::{PageHead, Switch};
use crate::model::{ProfileSummary, adjust_index_after_remove, unique_name};
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

    // Export/import dialog state. `export_for` holds the profile being exported
    // (`None` = closed); the import dialog is a plain open flag. One shared
    // busy/error pair — the two dialogs are mutually exclusive — plus a
    // toolbar success note that survives the dialog closing.
    let mut export_for = use_signal::<Option<String>>(|| None);
    let mut export_path = use_signal(String::new);
    let mut import_open = use_signal(|| false);
    let mut import_path = use_signal(String::new);
    let mut import_overwrite = use_signal(|| false);
    let mut io_busy = use_signal(|| false);
    let mut io_error = use_signal::<Option<String>>(|| None);
    let mut io_note = use_signal::<Option<String>>(|| None);
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
        PageHead { sub: "Per-profile key bindings, calibration, and lighting. A profile activates automatically when its linked application comes into focus." }

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
            if let Some(note) = io_note() {
                span { class: "li-meta", "{note}" }
            }
            // Import needs a reachable daemon; the button hides offline (same
            // rule as the shell's Save affordance).
            if !offline {
                button {
                    class: "btn",
                    style: "margin-left:auto;",
                    onclick: move |_| {
                        io_note.set(None);
                        io_error.set(None);
                        import_path.set(String::new());
                        import_overwrite.set(false);
                        import_open.set(true);
                    },
                    "Import…"
                }
            }
            button {
                class: "btn primary",
                style: if offline { "margin-left:auto;" } else { "" },
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
                    let name_export = name.clone();

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
                                    style: "width:7px;height:7px;border-radius:50%;background:var(--mute);opacity:0.6;display:inline-block;",
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
                                        // Duplicate: a daemon-side deep copy named
                                        // "<n> (copy)" — binds, lighting, and thumb mode
                                        // come along; the app link deliberately doesn't
                                        // (two profiles linked to one app would make
                                        // auto-switch ambiguous).
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
                                                crate::ipc::duplicate_profile(store, name_dup.clone(), copy);
                                            },
                                            "Duplicate"
                                        }
                                        // Export: opens the destination dialog
                                        // (the bundle itself comes from the
                                        // daemon, so the item hides offline).
                                        if !offline {
                                            button {
                                                class: "menu-item",
                                                onclick: move |_| {
                                                    menu_open.set(None);
                                                    io_note.set(None);
                                                    io_error.set(None);
                                                    export_path.set(crate::model::default_export_path(&name_export));
                                                    export_for.set(Some(name_export.clone()));
                                                },
                                                "Export…"
                                            }
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

        // ── Export dialog ─────────────────────────────────────────────────────
        if let Some(exp_name) = export_for() {
            {
                let exp_confirm = exp_name.clone();
                rsx! {
                    div {
                        class: "overlay",
                        onclick: move |_| {
                            if !io_busy() {
                                export_for.set(None);
                                io_error.set(None);
                            }
                        },
                        div {
                            class: "dialog",
                            onclick: move |e| e.stop_propagation(),
                            div { class: "dialog-title", "Export profile" }
                            div { class: "dialog-sub",
                                "Writes “{exp_name}” — with the macros it references — as a portable TOML bundle. Import it on any machine."
                            }
                            input {
                                class: "text-input",
                                autofocus: true,
                                value: "{export_path}",
                                oninput: move |e| export_path.set(e.value()),
                            }
                            if let Some(err) = io_error() {
                                div { class: "dialog-error", "{err}" }
                            }
                            div { class: "dialog-actions",
                                button {
                                    class: "btn",
                                    onclick: move |_| {
                                        export_for.set(None);
                                        io_error.set(None);
                                    },
                                    "Cancel"
                                }
                                button {
                                    class: "btn primary",
                                    onclick: move |_| {
                                        if io_busy() {
                                            return;
                                        }
                                        io_busy.set(true);
                                        io_error.set(None);
                                        let name = exp_confirm.clone();
                                        let path = export_path();
                                        spawn(async move {
                                            match do_export(store, name, path).await {
                                                Ok(msg) => {
                                                    io_note.set(Some(msg));
                                                    export_for.set(None);
                                                }
                                                Err(e) => io_error.set(Some(e)),
                                            }
                                            io_busy.set(false);
                                        });
                                    },
                                    if io_busy() { "Exporting…" } else { "Export" }
                                }
                            }
                        }
                    }
                }
            }
        }

        // ── Import dialog ─────────────────────────────────────────────────────
        if import_open() {
            div {
                class: "overlay",
                onclick: move |_| {
                    if !io_busy() {
                        import_open.set(false);
                        io_error.set(None);
                    }
                },
                div {
                    class: "dialog",
                    onclick: move |e| e.stop_propagation(),
                    div { class: "dialog-title", "Import profiles" }
                    div { class: "dialog-sub",
                        "Path to an exported profile bundle — or a full config.toml (e.g. ~/.config/rusty-tartarus/config.toml), which imports every profile in it. Imports apply live and persist when you press Save."
                    }
                    input {
                        class: "text-input",
                        autofocus: true,
                        placeholder: "~/profile.tartarust.toml",
                        value: "{import_path}",
                        oninput: move |e| import_path.set(e.value()),
                    }
                    crate::components::Toggle {
                        label: "Overwrite existing profiles/macros",
                        on: import_overwrite(),
                        on_toggle: move |_| {
                            let next = !import_overwrite();
                            import_overwrite.set(next);
                        },
                    }
                    if let Some(err) = io_error() {
                        div { class: "dialog-error", "{err}" }
                    }
                    div { class: "dialog-actions",
                        button {
                            class: "btn",
                            onclick: move |_| {
                                import_open.set(false);
                                io_error.set(None);
                            },
                            "Cancel"
                        }
                        button {
                            class: "btn primary",
                            onclick: move |_| {
                                if io_busy() {
                                    return;
                                }
                                io_busy.set(true);
                                io_error.set(None);
                                let path = import_path();
                                let overwrite = import_overwrite();
                                spawn(async move {
                                    match do_import(store, path, overwrite).await {
                                        Ok(msg) => {
                                            io_note.set(Some(msg));
                                            import_open.set(false);
                                        }
                                        Err(e) => io_error.set(Some(e)),
                                    }
                                    io_busy.set(false);
                                });
                            },
                            if io_busy() { "Importing…" } else { "Import" }
                        }
                    }
                }
            }
        }
    }
}

/// Fetch the bundle from the daemon and write it where the dialog says. The
/// small sync file write is fine on this task (same as desktop.rs's installs).
async fn do_export(
    store: Signal<crate::model::AppState>,
    name: String,
    path_raw: String,
) -> Result<String, String> {
    let trimmed = path_raw.trim();
    if trimmed.is_empty() {
        return Err("enter a destination path".into());
    }
    let bundle = crate::ipc::export_profile_bundle(store, name.clone()).await?;
    let toml = tartarust_ipc::bundle::to_toml(&bundle)?;
    let path = crate::model::expand_tilde(trimmed);
    std::fs::write(&path, &toml).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(format!(
        "Exported “{name}” ({} macro(s)) to {}",
        bundle.macros.len(),
        path.display()
    ))
}

/// Read + classify the file (export bundle or full config), push the profile(s)
/// to the daemon, and resync. Session-live until the user presses Save —
/// the same defer-all convention as every other edit in the UI.
async fn do_import(
    store: Signal<crate::model::AppState>,
    path_raw: String,
    overwrite: bool,
) -> Result<String, String> {
    let trimmed = path_raw.trim();
    if trimmed.is_empty() {
        return Err("enter a file path".into());
    }
    let path = crate::model::expand_tilde(trimmed);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let entries = match tartarust_ipc::bundle::parse(&text)? {
        tartarust_ipc::bundle::ImportSource::Bundle(b) => vec![(b.profile, b.macros)],
        tartarust_ipc::bundle::ImportSource::Config(entries) => entries,
    };
    let n = crate::ipc::import_profile_entries(store, entries, overwrite).await?;
    Ok(format!("Imported {n} profile(s) — press Save to keep"))
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
