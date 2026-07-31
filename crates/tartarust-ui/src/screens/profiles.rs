//! Profiles — compact single-column rows + a thin toolbar.
//!
//! One toolbar carries the GLOBAL auto-switch Switch (+ offline note) and a
//! right-aligned "New profile" button; below it a list of rows (status | name |
//! link | kebab), one per profile. There is no master-detail settings card —
//! name editing, per-app linking, and profile actions all live inline on the
//! row (name / link / kebab only).
//!
//! The export/import dialogs are the one place in the UI that talks to the
//! daemon OUTSIDE [`crate::store`]: their payload is a file on disk, not store
//! state, so the flows live here as private helpers. Every other mutation goes
//! through `store::fire` and inherits its optimistic-edit + resync policy.

use std::path::Path;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, AsyncApp, ClickEvent, Context, Entity, FontWeight, MouseButton, Rgba,
    SharedString, Subscription, Window, deferred, div, rgba,
};
use gpui_component::input::{Input, InputEvent, InputState};
use tartarust_ipc::bundle::ImportSource;
use tartarust_ipc::{ProfileExport, Response};

use crate::components::widgets::{btn_ghost, btn_primary, card, page_head, switch_row};
use crate::ipc;
use crate::model::{
    AppState, Macro, Profile, ProfileSummary, adjust_index_after_remove, default_export_path,
    expand_tilde, unique_name,
};
use crate::store;
use crate::theme;

/// Which file dialog is up. The two are mutually exclusive, which is why they
/// share one path field, one overwrite switch, and one busy/error pair.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Dialog {
    /// Export the named profile.
    Export(String),
    Import,
}

/// The inline rename editor. `from` is the daemon-known name — `RenameProfile`'s
/// `from` argument — captured when the edit opens and advanced only on a
/// successful commit, so a second Enter renames from the just-committed name
/// rather than the stale original (the old screen's `committed` freeze).
struct NameEdit {
    row: usize,
    from: String,
    input: Entity<InputState>,
    /// Held, not detached: the commit listener must die with the editor.
    _sub: Subscription,
}

/// The inline per-app link editor. Addressed by NAME like every daemon op, so
/// the row index alone is not enough.
struct LinkEdit {
    row: usize,
    name: String,
    input: Entity<InputState>,
    _sub: Subscription,
}

/// The per-render facts every row needs, snapshotted from the store once.
struct RowCtx {
    active: usize,
    default_profile: String,
    offline: bool,
    auto_switch: bool,
    count: usize,
    mono: SharedString,
}

pub struct Profiles {
    store: Entity<AppState>,
    /// Which row's kebab menu is open (one at a time).
    menu_open: Option<usize>,
    /// The inline rename editor, live only while a row is being renamed.
    name_edit: Option<NameEdit>,
    /// The per-row app-link editor (same one-at-a-time rule).
    link_edit: Option<LinkEdit>,
    /// The open file dialog, if any.
    dialog: Option<Dialog>,
    /// Destination/source path, shared by both dialogs.
    path_input: Entity<InputState>,
    /// Import-only "under a different name" field.
    rename_input: Entity<InputState>,
    /// The dialogs' overwrite switch.
    overwrite: bool,
    /// One shared busy/error pair for the in-flight file operation, plus a
    /// toolbar note that survives the dialog closing.
    io_busy: bool,
    io_error: Option<String>,
    io_note: Option<String>,
}

impl Profiles {
    pub fn new(store: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Re-render on every store write: the row list, the DEFAULT badge, and
        // the offline gating all read daemon-owned state.
        cx.observe(&store, |_, _, cx| cx.notify()).detach();

        let path_input = cx.new(|cx| InputState::new(window, cx));
        let rename_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("import under a different name (optional)")
        });

        Self {
            store,
            menu_open: None,
            name_edit: None,
            link_edit: None,
            dialog: None,
            path_input,
            rename_input,
            overwrite: false,
            io_busy: false,
            io_error: None,
            io_note: None,
        }
    }

    // --- Mutations ---------------------------------------------------------

    /// Optimistically flip auto-switch and push to the daemon. Also the
    /// handler behind a dimmed (disabled) link control: instead of silently
    /// doing nothing, enabling auto-switch makes the link meaningful.
    fn set_auto_switch(&mut self, on: bool, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            if let Some(st) = s.status.as_mut() {
                st.auto_switch = on;
            }
            cx.notify();
        });
        store::fire(&self.store, ipc::req_set_auto_switch(on), cx);
    }

    /// Optimistic local push + persist (the daemon makes the profile). The name
    /// is de-duped so every daemon op (which addresses BY NAME) stays
    /// unambiguous and the DEFAULT badge can't double-paint.
    fn create_profile(&mut self, cx: &mut Context<Self>) {
        self.end_edits(cx);
        self.menu_open = None;
        let mut name = String::new();
        self.store.update(cx, |s, cx| {
            name = unique_name("New Profile", |n| s.profiles.iter().any(|p| p.name == n));
            s.profiles.push(ProfileSummary {
                name: name.clone(),
                linked_app: None,
            });
            cx.notify();
        });
        store::fire(&self.store, ipc::req_create_profile(name), cx);
    }

    /// A daemon-side deep copy named "<n> (copy)" — binds, lighting, and thumb
    /// mode come along; the app link deliberately doesn't (two profiles linked
    /// to one app would make auto-switch ambiguous).
    fn duplicate_profile(&mut self, from: String, cx: &mut Context<Self>) {
        let mut copy = String::new();
        self.store.update(cx, |s, cx| {
            copy = unique_name(&format!("{from} (copy)"), |n| {
                s.profiles.iter().any(|p| p.name == n)
            });
            s.profiles.push(ProfileSummary {
                name: copy.clone(),
                linked_app: None,
            });
            cx.notify();
        });
        store::fire(&self.store, ipc::req_duplicate_profile(from, copy), cx);
    }

    /// Delete, behind the same guards the render-time menu applies.
    fn delete_profile(&mut self, row: usize, cx: &mut Context<Self>) {
        let mut name = None;
        self.store.update(cx, |s, cx| {
            // Defense in depth behind the render-time guard.
            if s.profiles.len() <= 1 {
                return;
            }
            // `row` is a render-snapshot index; a double event (or a resync
            // shrinking the list between render and click) makes it stale — a
            // stale index is a no-op, since Vec::remove would panic past the
            // end (cf. the macros screen's delete guard).
            let Some(n) = s.profiles.get(row).map(|p| p.name.clone()) else {
                return;
            };
            s.profiles.remove(row);
            let new_len = s.profiles.len();
            adjust_index_after_remove(&mut s.active_profile, row, new_len);
            name = Some(n);
            cx.notify();
        });
        if let Some(n) = name {
            store::fire(&self.store, ipc::req_delete_profile(n), cx);
        }
    }

    /// Switch + reload the body so the editor screens follow the new active
    /// profile.
    fn set_active(&mut self, row: usize, cx: &mut Context<Self>) {
        self.end_edits(cx);
        self.menu_open = None;
        let name = self
            .store
            .read(cx)
            .profiles
            .get(row)
            .map(|p| p.name.clone());
        if let Some(name) = name {
            store::switch_profile_and_reload(&self.store, name, cx);
        }
    }

    fn set_default(&mut self, name: String, cx: &mut Context<Self>) {
        self.menu_open = None;
        store::fire(&self.store, ipc::req_set_default_profile(name), cx);
    }

    fn set_link(&mut self, row: usize, name: String, app: Option<String>, cx: &mut Context<Self>) {
        self.store.update(cx, |s, cx| {
            if let Some(p) = s.profiles.get_mut(row) {
                p.linked_app = app.clone();
            }
            cx.notify();
        });
        store::fire(&self.store, ipc::req_set_profile_linked_app(name, app), cx);
    }

    // --- Inline editors ----------------------------------------------------

    /// Commit and close whichever inline editor is open. Called before anything
    /// that opens another editor, a menu, or a dialog: GPUI delivers the
    /// outgoing input's `Blur` AFTER the new state is installed, so the blur
    /// handler alone (which refuses to act for a superseded editor) would drop
    /// the pending commit.
    fn end_edits(&mut self, cx: &mut Context<Self>) {
        self.commit_name(cx);
        self.name_edit = None;
        self.commit_link(cx);
        self.link_edit = None;
    }

    fn open_name_edit(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(cx);
        self.menu_open = None;
        let Some(from) = self
            .store
            .read(cx)
            .profiles
            .get(row)
            .map(|p| p.name.clone())
        else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(from.clone()));
        // Enter and blur both commit — the old screen's `onchange`.
        let sub = cx.subscribe(&input, |this, emitter, ev: &InputEvent, cx| {
            if !matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                return;
            }
            // Only the CURRENT editor may act: opening another row's editor
            // blurs this one after the new one is installed, and an unguarded
            // handler would tear the new editor down.
            let current = this
                .name_edit
                .as_ref()
                .is_some_and(|e| e.input.entity_id() == emitter.entity_id());
            if current {
                this.commit_name(cx);
                this.name_edit = None;
                cx.notify();
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.name_edit = Some(NameEdit {
            row,
            from,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    /// Send the pending rename, if there is one. An empty commit sends nothing
    /// and leaves the row on its committed name (the editor closes back onto
    /// the store's value, so there is nothing to snap back).
    fn commit_name(&mut self, cx: &mut Context<Self>) {
        let Some((row, from, to)) = self.name_edit.as_ref().map(|e| {
            (
                e.row,
                e.from.clone(),
                e.input.read(cx).value().trim().to_string(),
            )
        }) else {
            return;
        };
        if to.is_empty() || to == from {
            return;
        }
        // Mirror the daemon: rename_profile also renames a matching configured
        // default / active name, so update the cached status alongside — a
        // stale name would detach the DEFAULT badge (or pin it to a wrong row
        // renamed onto the old name) until an unrelated resync. `store::fire`
        // corrects this authoritatively once the daemon acks; this is the
        // immediate optimistic mirror.
        self.store.update(cx, |s, cx| {
            if let Some(p) = s.profiles.get_mut(row) {
                p.name = to.clone();
            }
            if row == s.active_profile {
                s.profile.name = to.clone();
            }
            if let Some(st) = s.status.as_mut() {
                if st.default_profile == from {
                    st.default_profile = to.clone();
                }
                if st.active_profile == from {
                    st.active_profile = to.clone();
                }
            }
            cx.notify();
        });
        store::fire(&self.store, ipc::req_rename_profile(from, to.clone()), cx);
        // Keep `from` current so a follow-up edit (Enter, keep typing) sends
        // from the just-committed name, not the stale one.
        if let Some(edit) = self.name_edit.as_mut() {
            edit.from = to;
        }
    }

    fn open_link_edit(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(cx);
        self.menu_open = None;
        let Some(profile) = self.store.read(cx).profiles.get(row).cloned() else {
            return;
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("app id / window class, e.g. cs2.exe")
                .default_value(profile.linked_app.clone().unwrap_or_default())
        });
        let sub = cx.subscribe(&input, |this, emitter, ev: &InputEvent, cx| {
            if !matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                return;
            }
            let current = this
                .link_edit
                .as_ref()
                .is_some_and(|e| e.input.entity_id() == emitter.entity_id());
            if current {
                this.commit_link(cx);
                this.link_edit = None;
                cx.notify();
            }
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.link_edit = Some(LinkEdit {
            row,
            name: profile.name,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    /// Set or clear the linked app. A commit that changes nothing sends
    /// nothing, so opening the editor and blurring it can't dirty the profile.
    fn commit_link(&mut self, cx: &mut Context<Self>) {
        let Some((row, name, typed)) = self.link_edit.as_ref().map(|e| {
            (
                e.row,
                e.name.clone(),
                e.input.read(cx).value().trim().to_string(),
            )
        }) else {
            return;
        };
        let app = (!typed.is_empty()).then_some(typed);
        let known = self
            .store
            .read(cx)
            .profiles
            .get(row)
            .map(|p| p.linked_app.clone());
        if known == Some(app.clone()) {
            return;
        }
        self.set_link(row, name, app, cx);
    }

    // --- File dialogs ------------------------------------------------------

    fn open_export(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(cx);
        self.menu_open = None;
        self.io_note = None;
        self.io_error = None;
        self.overwrite = false;
        let suggested = default_export_path(&name);
        self.path_input.update(cx, |s, cx| {
            s.set_placeholder("", window, cx);
            s.set_value(suggested, window, cx);
            s.focus(window, cx);
        });
        self.dialog = Some(Dialog::Export(name));
        cx.notify();
    }

    fn open_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_edits(cx);
        self.menu_open = None;
        self.io_note = None;
        self.io_error = None;
        self.overwrite = false;
        self.path_input.update(cx, |s, cx| {
            s.set_placeholder("~/profile.tartarust.toml", window, cx);
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
        self.rename_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.dialog = Some(Dialog::Import);
        cx.notify();
    }

    /// Dismissal — backdrop click AND Cancel — is refused while busy, so an
    /// in-flight operation can never lose its error to a closed dialog.
    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        if self.io_busy {
            return;
        }
        self.io_error = None;
        self.dialog = None;
        cx.notify();
    }

    /// Run the open dialog's operation. The busy flag is set here and cleared
    /// when the spawned work finishes, whichever way it went.
    fn confirm_dialog(&mut self, cx: &mut Context<Self>) {
        if self.io_busy {
            return;
        }
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        let path = self.path_input.read(cx).value().to_string();
        let rename = self.rename_input.read(cx).value().to_string();
        let overwrite = self.overwrite;
        let store = self.store.clone();
        self.io_busy = true;
        self.io_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = match dialog {
                Dialog::Export(name) => do_export(&store, name, path, overwrite, cx).await,
                Dialog::Import => do_import(&store, path, rename, overwrite, cx).await,
            };
            let _ = this.update(cx, |this, cx| {
                this.io_busy = false;
                match outcome {
                    Ok(note) => {
                        this.io_note = Some(note);
                        this.dialog = None;
                    }
                    Err(e) => this.io_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // --- Rendering ---------------------------------------------------------

    fn profile_row(
        &self,
        i: usize,
        profile: &ProfileSummary,
        ctx: &RowCtx,
        cx: &Context<Self>,
    ) -> AnyElement {
        let is_active = i == ctx.active;
        let is_default = !ctx.default_profile.is_empty() && profile.name == ctx.default_profile;
        let name = profile.name.clone();
        let linked = profile.linked_app.clone();

        // ── STATUS ─────────────────────────────────────────────────────────
        let status = if is_active {
            badge("ACTIVE", theme::live(), theme::live_wash(), &ctx.mono).into_any_element()
        } else if is_default {
            badge("DEFAULT", theme::dim(), theme::panel_2(), &ctx.mono).into_any_element()
        } else {
            // No dedicated idle-dot widget: a small muted dot keeps the status
            // column aligned without pretending to be an annunciator.
            div()
                .size(theme::sp(7.))
                .rounded_full()
                .bg(theme::mute())
                .opacity(0.6)
                .into_any_element()
        };

        // ── NAME (inline-editable) ─────────────────────────────────────────
        let editing = self.name_edit.as_ref().filter(|e| e.row == i);
        let name_cell = match editing {
            Some(edit) => div()
                .flex_1()
                .min_w(theme::sp(0.))
                .child(Input::new(&edit.input))
                .into_any_element(),
            None => div()
                .id(("prof-name", i))
                .flex_1()
                .min_w(theme::sp(0.))
                .px(theme::sp(8.))
                .py(theme::sp(6.))
                .rounded(theme::sp(theme::RADIUS_SM))
                .border_1()
                .border_color(gpui::transparent_black())
                .text_size(theme::sp(13.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::ink())
                .truncate()
                .hover(|s| s.border_color(theme::line_soft()))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_name_edit(i, window, cx)
                }))
                .child(name.clone())
                .into_any_element(),
        };

        // ── LINK (per-app auto-switch, persisted) ──────────────────────────
        let link_editing = self.link_edit.as_ref().filter(|e| e.row == i);
        let link_cell = match (link_editing, linked) {
            (Some(edit), _) => div().child(Input::new(&edit.input)).into_any_element(),
            // A linked app: a chip with a clear (×). Dimmed + click-to-enable
            // when auto-switch is off.
            (None, Some(app)) if ctx.auto_switch => {
                let clear_name = name.clone();
                link_chip(("prof-link", i), &ctx.mono)
                    .child(div().text_size(theme::sp(11.)).child("◆"))
                    .child(div().truncate().child(app))
                    .child(
                        div()
                            .id(("prof-link-x", i))
                            .flex_none()
                            .size(theme::sp(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme::sp(3.))
                            .text_color(theme::mute())
                            .hover(|s| s.bg(theme::red_wash()).text_color(theme::red()))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_link(i, clear_name.clone(), None, cx)
                            }))
                            .child("×"),
                    )
                    .into_any_element()
            }
            (None, Some(app)) => link_chip(("prof-link-off", i), &ctx.mono)
                .opacity(0.45)
                .cursor_pointer()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_auto_switch(true, cx)))
                .child(div().text_size(theme::sp(11.)).child("◆"))
                .child(div().truncate().child(app))
                .into_any_element(),
            // No link: a ghost "+ Link app…" button.
            (None, None) => link_add(("prof-link-add", i))
                .when(ctx.auto_switch, |el| {
                    el.hover(|s| s.border_color(theme::rust()).text_color(theme::ink()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_link_edit(i, window, cx)
                        }))
                })
                .when(!ctx.auto_switch, |el| {
                    el.opacity(0.45).on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.set_auto_switch(true, cx)),
                    )
                })
                .child("+ Link app…")
                .into_any_element(),
        };

        // ── KEBAB ──────────────────────────────────────────────────────────
        // Delete is only actionable when the daemon would accept it: it rejects
        // deleting the active profile, the configured default, and the last
        // remaining profile. Offering it anyway would just flicker the row away
        // and snap back online, or strand a locally-deleted row offline.
        let can_delete = !is_active && !is_default && ctx.count > 1;
        let delete_note = if is_active {
            "Can't delete the active profile — switch to another first"
        } else if is_default {
            "Can't delete the default profile — pick another default first"
        } else {
            "Can't delete the last profile"
        };

        let name_default = name.clone();
        let name_dup = name.clone();
        let name_export = name.clone();

        let menu = div()
            .absolute()
            .top(theme::sp(36.))
            .right(theme::sp(0.))
            .w(theme::sp(210.))
            .flex()
            .flex_col()
            .gap(theme::sp(1.))
            .p(theme::sp(5.))
            .rounded(theme::sp(theme::RADIUS))
            .bg(theme::panel_1())
            .border_1()
            .border_color(theme::line())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            // Set active (hidden if already active).
            .when(!is_active, |el| {
                el.child(menu_item(
                    ("prof-menu-active", i),
                    "Set active",
                    true,
                    false,
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.set_active(i, cx)),
                ))
            })
            // Set as default (marked + inert if already).
            .child(menu_item(
                ("prof-menu-default", i),
                if is_default {
                    "Default ✓"
                } else {
                    "Set as default"
                },
                !is_default,
                false,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_default(name_default.clone(), cx)
                }),
            ))
            .child(menu_item(
                ("prof-menu-dup", i),
                "Duplicate",
                true,
                false,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menu_open = None;
                    this.duplicate_profile(name_dup.clone(), cx);
                }),
            ))
            // Export opens the destination dialog; the bundle itself comes from
            // the daemon, so the item hides offline.
            .when(!ctx.offline, |el| {
                el.child(menu_item(
                    ("prof-menu-export", i),
                    "Export…",
                    true,
                    false,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_export(name_export.clone(), window, cx)
                    }),
                ))
            })
            .child(menu_item(
                ("prof-menu-rename", i),
                "Rename",
                true,
                false,
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_name_edit(i, window, cx)
                }),
            ))
            .child(
                div()
                    .h(theme::sp(1.))
                    .mx(theme::sp(6.))
                    .my(theme::sp(4.))
                    .bg(theme::line_soft()),
            )
            .child(menu_item(
                ("prof-menu-delete", i),
                "Delete",
                can_delete,
                true,
                cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menu_open = None;
                    this.delete_profile(i, cx);
                }),
            ))
            // The daemon's refusal reasons, spelled out rather than hidden in a
            // tooltip the way the CSS build did.
            .when(!can_delete, |el| {
                el.child(
                    div()
                        .px(theme::sp(10.))
                        .pb(theme::sp(4.))
                        .text_size(theme::sp(10.))
                        .text_color(theme::mute())
                        .child(delete_note),
                )
            });

        let kebab = div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id(("prof-kebab", i))
                    .size(theme::sp(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme::sp(theme::RADIUS_SM))
                    .text_size(theme::sp(16.))
                    .text_color(theme::mute())
                    .hover(|s| s.bg(theme::panel_2()).text_color(theme::ink()))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let next = (this.menu_open != Some(i)).then_some(i);
                        this.end_edits(cx);
                        this.menu_open = next;
                        cx.notify();
                    }))
                    .child("⋮"),
            )
            // Deferred so the menu paints above the rows below it; the
            // click-away catcher is deferred one priority lower, mirroring the
            // old z-index 20/30 pair.
            .when(self.menu_open == Some(i), |el| {
                el.child(deferred(menu).with_priority(2))
            });

        // ── ROW ────────────────────────────────────────────────────────────
        // A row that is BOTH active and default shows the ACTIVE treatment.
        let (border, background) = match (is_active, is_default) {
            (true, _) => (theme::rust(), theme::rust_wash()),
            (false, true) => (theme::line(), theme::panel_1()),
            (false, false) => (theme::line_soft(), theme::panel_0()),
        };

        div()
            .flex()
            .items_center()
            .gap(theme::sp(14.))
            .px(theme::sp(13.))
            .py(theme::sp(11.))
            .rounded(theme::sp(theme::RADIUS_SM))
            .bg(background)
            .border_1()
            .border_color(border)
            // Hover lifts a plain row only: the active and default treatments
            // already carry a lit border, and washing over them reads as noise.
            .when(!is_active && !is_default, |el| {
                el.hover(|s| s.bg(theme::panel_1()).border_color(theme::line()))
            })
            .child(div().flex_none().w(theme::sp(72.)).child(status))
            .child(name_cell)
            .child(div().flex_none().w(theme::sp(200.)).child(link_cell))
            .child(kebab)
            .into_any_element()
    }

    /// The shared export/import dialog chassis: the click-away overlay, a
    /// centered card, title/sub, the path field, the dialog's own extra rows,
    /// the error slot, and the Cancel/confirm pair.
    fn render_dialog(&self, dialog: &Dialog, mono: SharedString, cx: &Context<Self>) -> AnyElement {
        let (title, sub, confirm_label, busy_label, overwrite_label) = match dialog {
            Dialog::Export(name) => (
                "EXPORT PROFILE",
                format!(
                    "Writes “{name}” — with the macros it references — as a portable TOML \
                     bundle. Import it on any machine."
                ),
                "Export",
                "Exporting…",
                "Overwrite an existing file",
            ),
            Dialog::Import => (
                "IMPORT PROFILES",
                "Path to an exported profile bundle — or a full config.toml (e.g. \
                 ~/.config/rusty-tartarus/config.toml), which imports every profile in it. \
                 Imports apply live and persist when you press Save."
                    .to_string(),
                "Import",
                "Importing…",
                "Overwrite existing profiles/macros",
            ),
        };
        let importing = matches!(dialog, Dialog::Import);
        let busy = self.io_busy;

        let body = card()
            .w(theme::sp(480.))
            .p(theme::sp(18.))
            .border_color(theme::line())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .font_family(mono)
                    .text_size(theme::sp(12.))
                    .text_color(theme::ink())
                    .child(title),
            )
            .child(
                div()
                    .text_size(theme::sp(12.))
                    .line_height(theme::sp(18.))
                    .text_color(theme::dim())
                    .child(sub),
            )
            .child(div().w_full().child(Input::new(&self.path_input)))
            // The daemon's collision error suggests renaming — so the dialog
            // must actually offer one (single-profile files only, mirroring the
            // CLI's --rename).
            .when(importing, |el| {
                el.child(div().w_full().child(Input::new(&self.rename_input)))
            })
            .child(switch_row(
                "dialog-overwrite",
                overwrite_label,
                self.overwrite,
                cx.listener(|this, on: &bool, _, cx| {
                    this.overwrite = *on;
                    cx.notify();
                }),
            ))
            .when_some(self.io_error.clone(), |el, err| {
                el.child(
                    div()
                        .text_size(theme::sp(12.))
                        .line_height(theme::sp(18.))
                        .text_color(theme::red())
                        .child(err),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(theme::sp(8.))
                    .pt(theme::sp(2.))
                    .child(btn_ghost(
                        "dialog-cancel",
                        "Cancel",
                        cx.listener(|this, _: &ClickEvent, _, cx| this.close_dialog(cx)),
                    ))
                    .child(btn_primary(
                        "dialog-confirm",
                        if busy { busy_label } else { confirm_label },
                        cx.listener(|this, _: &ClickEvent, _, cx| this.confirm_dialog(cx)),
                    )),
            );

        // The scrim covers the screen REGION, not the whole window — the shell
        // owns the masthead and tab rail and renders this view as a child.
        div()
            .id("dialog-overlay")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x0A09079E))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_dialog(cx)),
            )
            .child(body)
            .into_any_element()
    }
}

impl Render for Profiles {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Snapshot what the screen needs, then drop the borrow before building
        // listeners (the shell's convention).
        let (profiles, ctx) = {
            let s = self.store.read(cx);
            (
                s.profiles.clone(),
                RowCtx {
                    active: s.active_profile,
                    // The configured default profile, compared BY NAME.
                    default_profile: s
                        .status
                        .as_ref()
                        .map(|st| st.default_profile.clone())
                        .unwrap_or_default(),
                    offline: s.offline,
                    // Auto-switch is a GLOBAL/per-device setting carried in
                    // DaemonStatus, not a per-profile one.
                    auto_switch: s.status.as_ref().is_some_and(|st| st.auto_switch),
                    count: s.profiles.len(),
                    mono: theme::mono(cx),
                },
            )
        };

        // ── Thin toolbar: global auto-switch + notes + New profile ─────────
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(theme::sp(14.))
            .min_h(theme::sp(44.))
            .px(theme::sp(4.))
            .child(div().flex_none().child(switch_row(
                "prof-auto-switch",
                "Auto-switch by focused app",
                ctx.auto_switch,
                cx.listener(|this, on: &bool, _, cx| this.set_auto_switch(*on, cx)),
            )))
            .when(ctx.offline, |el| {
                el.child(meta("Daemon offline — last-known state shown.", &ctx.mono))
            })
            .when_some(self.io_note.clone(), |el, note| {
                el.child(meta(note, &ctx.mono))
            })
            .child(div().flex_1())
            // Import needs a reachable daemon; the button hides offline (same
            // rule as the shell's Save affordance).
            .when(!ctx.offline, |el| {
                el.child(btn_ghost(
                    "prof-import",
                    "Import…",
                    cx.listener(|this, _: &ClickEvent, window, cx| this.open_import(window, cx)),
                ))
            })
            .child(btn_primary(
                "prof-new",
                "New profile",
                cx.listener(|this, _: &ClickEvent, _, cx| this.create_profile(cx)),
            ));

        // ── Profile rows ───────────────────────────────────────────────────
        let list = div()
            .id("profile-list")
            .flex()
            .flex_col()
            .gap(theme::sp(8.))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .children(
                profiles
                    .iter()
                    .enumerate()
                    .map(|(i, p)| self.profile_row(i, p, &ctx, cx)),
            );

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .p(theme::sp(22.))
            .child(page_head(
                "Per-profile key bindings, calibration, and lighting. A profile activates \
                 automatically when its linked application comes into focus.",
            ))
            .child(toolbar)
            .child(div().h(theme::sp(14.)).flex_none())
            .child(list)
            // The kebab menus' click-away catcher, deferred so it sits above the
            // rows but below the menu itself (priority 2).
            .when(self.menu_open.is_some(), |el| {
                el.child(deferred(
                    div()
                        .id("prof-menu-dismiss")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.menu_open = None;
                                cx.notify();
                            }),
                        ),
                ))
            })
            .when_some(self.dialog.clone(), |el, dialog| {
                el.child(self.render_dialog(&dialog, ctx.mono.clone(), cx))
            })
    }
}

// --- Small row/menu pieces ---------------------------------------------------

/// A monospace status pill (ACTIVE / DEFAULT).
fn badge(text: &'static str, ink: Rgba, bg: Rgba, mono: &SharedString) -> gpui::Div {
    div()
        .flex_none()
        .px(theme::sp(7.))
        .py(theme::sp(3.))
        .rounded(theme::sp(3.))
        .bg(bg)
        .font_family(mono.clone())
        .text_size(theme::sp(9.5))
        .text_color(ink)
        .child(text)
}

/// A quiet monospace note (offline warning, export/import result).
fn meta(text: impl Into<SharedString>, mono: &SharedString) -> gpui::Div {
    div()
        .font_family(mono.clone())
        .text_size(theme::sp(11.))
        .text_color(theme::mute())
        .child(text.into())
}

/// The linked-app chip chassis, shared by the live and dimmed variants. The id
/// is per-row: element ids must be unique under the list's own id.
fn link_chip(id: (&'static str, usize), mono: &SharedString) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(theme::sp(6.))
        .px(theme::sp(9.))
        .py(theme::sp(4.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .bg(theme::panel_1())
        .border_1()
        .border_color(theme::line())
        .font_family(mono.clone())
        .text_size(theme::sp(11.))
        .text_color(theme::dim())
}

/// The dashed "+ Link app…" affordance shown when a profile has no link.
fn link_add(id: (&'static str, usize)) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .px(theme::sp(10.))
        .py(theme::sp(4.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .border_1()
        .border_dashed()
        .border_color(theme::line())
        .text_size(theme::sp(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::dim())
        .cursor_pointer()
}

/// One row in a kebab menu. `enabled` false paints it muted and drops the click
/// handler — the daemon would refuse the operation anyway.
fn menu_item(
    id: (&'static str, usize),
    label: &'static str,
    enabled: bool,
    danger: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let ink = match (enabled, danger) {
        (false, _) => theme::mute(),
        (true, true) => theme::red(),
        (true, false) => theme::dim(),
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .px(theme::sp(10.))
        .py(theme::sp(8.))
        .rounded(theme::sp(theme::RADIUS_SM))
        .text_size(theme::sp(12.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(move |s| {
                    if danger {
                        s.bg(theme::red_wash()).text_color(theme::red())
                    } else {
                        s.bg(theme::panel_2()).text_color(theme::ink())
                    }
                })
                .on_click(on_click)
        })
        .child(label)
}

// --- Export / import ---------------------------------------------------------

/// Run a blocking file operation off the UI thread, bounded so a hung
/// filesystem (NFS, autofs) can't pin the busy dialog forever. The background
/// thread may linger past the timeout; the dialog does not.
///
/// GPUI ships no timeout combinator and this crate pulls in no futures
/// utilities, so the bound is a poll: the work runs on the background executor
/// and hands its result back through a channel the caller checks between short
/// timers.
async fn bounded_blocking<T: Send + 'static>(
    what: &'static str,
    cx: &mut AsyncApp,
    op: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    const DEADLINE: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(20);

    let (tx, rx) = std::sync::mpsc::channel();
    cx.background_executor()
        .spawn(async move {
            let _ = tx.send(op());
        })
        .detach();

    let started = Instant::now();
    loop {
        if let Ok(value) = rx.try_recv() {
            return Ok(value);
        }
        if started.elapsed() >= DEADLINE {
            return Err(format!("{what} timed out after {}s", DEADLINE.as_secs()));
        }
        cx.background_executor().timer(POLL).await;
    }
}

/// Write the bundle file. Without `overwrite`, `create_new` (O_EXCL) refuses an
/// existing destination at the filesystem level — unlike an `exists()`
/// pre-check it cannot race a concurrent create, and it also refuses a dangling
/// symlink instead of writing through it.
fn write_bundle_file(path: &Path, toml: &str, overwrite: bool) -> Result<(), String> {
    use std::io::Write;
    if overwrite {
        return std::fs::write(path, toml).map_err(|e| format!("writing {}: {e}", path.display()));
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut f) => f
            .write_all(toml.as_bytes())
            .map_err(|e| format!("writing {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(format!(
            "{} already exists — toggle “Overwrite an existing file” to replace it",
            path.display()
        )),
        Err(e) => Err(format!("writing {}: {e}", path.display())),
    }
}

/// A completed exchange (any decoded reply, even a rejection) proves the daemon
/// is reachable: clear a stale offline flag, exactly as the store layer does
/// after its own sends.
fn mark_online(store: &Entity<AppState>, cx: &mut AsyncApp) {
    let _ = store.update(cx, |s, cx| {
        if s.offline {
            s.offline = false;
            cx.notify();
        }
    });
}

/// A transport-level failure means the daemon is unreachable: log it and flip
/// the store into the honest offline state.
fn mark_offline(store: &Entity<AppState>, err: &str, cx: &mut AsyncApp) {
    tracing::warn!("daemon unreachable: {err}");
    let _ = store.update(cx, |s, cx| {
        if !s.offline {
            s.offline = true;
            cx.notify();
        }
    });
}

/// Fetch a profile's portable export bundle (profile + referenced macros) from
/// the daemon. `Err` carries a display-ready message (daemon rejection or
/// transport failure — the latter also flips the offline badge).
async fn export_profile_bundle(
    store: &Entity<AppState>,
    name: String,
    cx: &mut AsyncApp,
) -> Result<ProfileExport, String> {
    let request = ipc::req_export_profile(name);
    let reply = cx
        .background_executor()
        .spawn(async move { ipc::send(&request) })
        .await;
    match reply {
        Ok(Response::ProfileExport(bundle)) => {
            mark_online(store, cx);
            Ok(bundle)
        }
        Ok(Response::Error { message }) => {
            mark_online(store, cx);
            Err(message)
        }
        Ok(other) => {
            mark_online(store, cx);
            Err(format!("unexpected daemon reply: {other:?}"))
        }
        Err(e) => {
            mark_offline(store, &e.to_string(), cx);
            Err("daemon unreachable".into())
        }
    }
}

/// Fetch the bundle from the daemon and write it where the dialog says.
async fn do_export(
    store: &Entity<AppState>,
    name: String,
    path_raw: String,
    overwrite: bool,
    cx: &mut AsyncApp,
) -> Result<String, String> {
    let trimmed = path_raw.trim();
    if trimmed.is_empty() {
        return Err("enter a destination path".into());
    }
    let path = expand_tilde(trimmed);
    let bundle = export_profile_bundle(store, name.clone(), cx).await?;
    let toml = tartarust_ipc::bundle::to_toml(&bundle)?;
    let macro_count = bundle.macros.len();
    let write_path = path.clone();
    bounded_blocking("writing the bundle", cx, move || {
        write_bundle_file(&write_path, &toml, overwrite)
    })
    .await??;
    Ok(format!(
        "Exported “{name}” ({macro_count} macro(s)) to {}",
        path.display()
    ))
}

/// Import profile entries (from an export bundle or a whole config file) into
/// the daemon, sequentially. Per-profile rejections don't stop the loop —
/// "imports every profile in it" must mean every importable one, matching the
/// CLI — only a transport failure aborts. `rename` applies when there is exactly
/// ONE entry (like the CLI's --rename). Ends with a resync either way, so the
/// profile list, macros, and the unsaved flag are authoritative — like every
/// other edit, an import is session-live until the user presses Save. `Ok` is
/// how many profiles landed; `Err` still reports how many landed alongside what
/// failed.
async fn import_profile_entries(
    store: &Entity<AppState>,
    entries: Vec<(Profile, Vec<Macro>)>,
    rename: Option<String>,
    overwrite: bool,
    cx: &mut AsyncApp,
) -> Result<usize, String> {
    if rename.is_some() && entries.len() != 1 {
        return Err("a rename only applies to single-profile bundle files".into());
    }
    let mut imported = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for (profile, macros) in entries {
        let shown = rename.clone().unwrap_or_else(|| profile.name.clone());
        let request = ipc::req_import_profile(profile, macros, rename.clone(), overwrite);
        let reply = cx
            .background_executor()
            .spawn(async move { ipc::send(&request) })
            .await;
        match reply {
            Ok(Response::Ack) => imported += 1,
            Ok(Response::Error { message }) => {
                failures.push(format!("“{shown}”: {message}"));
            }
            Ok(other) => {
                failures.push(format!("“{shown}”: unexpected daemon reply: {other:?}"));
                break;
            }
            Err(e) => {
                // Break, not return: profiles already imported this loop are
                // live in the daemon, so the unsaved flag + resync below must
                // still run (the resync itself no-ops while unreachable).
                mark_offline(store, &e.to_string(), cx);
                failures.push(format!("“{shown}”: {e} — import aborted"));
                break;
            }
        }
    }
    // `ImportProfile` is not in `ipc::dirties` (the daemon applies it live but
    // defers the disk write), so flip the Save affordance here.
    if imported > 0 {
        let _ = store.update(cx, |s, cx| {
            if !s.unsaved {
                s.unsaved = true;
                cx.notify();
            }
        });
    }
    // Resync even on failure: a config-file import can partially land before a
    // rejection, and the list must reflect what the daemon actually holds.
    // `spawn_initial_load` is the store layer's only public "pull a snapshot and
    // overlay it" entry point; it probes once and overlays daemon-owned fields
    // only, which is exactly the resync this needs.
    let handle = store.clone();
    let _ = cx.update(|cx| store::spawn_initial_load(handle, cx));

    if failures.is_empty() {
        Ok(imported)
    } else if imported > 0 {
        Err(format!(
            "imported {imported}, but {} failed — {} (press Save to keep the imported ones)",
            failures.len(),
            failures.join("; ")
        ))
    } else {
        Err(failures.join("; "))
    }
}

/// Read + classify the file (export bundle or full config), push the profile(s)
/// to the daemon, and resync. Session-live until the user presses Save — the
/// same defer-all convention as every other edit in the UI.
async fn do_import(
    store: &Entity<AppState>,
    path_raw: String,
    rename_raw: String,
    overwrite: bool,
    cx: &mut AsyncApp,
) -> Result<String, String> {
    let trimmed = path_raw.trim();
    if trimmed.is_empty() {
        return Err("enter a file path".into());
    }
    let rename = rename_raw.trim();
    let rename = (!rename.is_empty()).then(|| rename.to_string());
    let path = expand_tilde(trimmed);
    let read_path = path.clone();
    let text = bounded_blocking("reading the file", cx, move || {
        std::fs::read_to_string(&read_path)
    })
    .await?
    .map_err(|e| format!("reading {}: {e}", path.display()))?;
    let entries = match tartarust_ipc::bundle::parse(&text)? {
        ImportSource::Bundle(b) => vec![(b.profile, b.macros)],
        ImportSource::Config(entries) => entries,
    };
    let n = import_profile_entries(store, entries, rename, overwrite, cx).await?;
    Ok(format!("Imported {n} profile(s) — press Save to keep"))
}
