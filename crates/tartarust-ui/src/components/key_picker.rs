//! A reusable, searchable, category-grouped single-key picker.
//!
//! One trigger button opens a popover with a pinned search box and a scrollable,
//! grouped list of every bindable key. It emits a single evdev name; chords are
//! assembled by the consumer from several picks. Reused for Customize (each chord
//! key + mouse/multimedia) and Macros (per step).
//!
//! The catalog is `&'static`, so nothing is cloned in the hot path: the filter is
//! a `use_memo` that produces an ordered `Vec<usize>` of indices into the static
//! [`search_index`] slice. Render and keyboard navigation both walk that single
//! list, which keeps the highlighted row and the rendered row in lockstep.

use dioxus::prelude::*;

use crate::model::{key_label, search_index};

/// A searchable single-key picker. `value` is the current evdev name (may be
/// empty); `on_pick` fires with the chosen evdev name. `placeholder` relabels
/// the trigger button when `value` is empty (default "Choose key…"), so a
/// caller can make the trigger read e.g. "+ Tap" directly.
#[component]
pub fn KeyPicker(
    value: String,
    on_pick: EventHandler<String>,
    placeholder: Option<String>,
) -> Element {
    let mut open = use_signal(|| false);
    let mut query = use_signal(String::new);
    let mut highlighted = use_signal(|| 0usize);

    // The static, catalog-ordered, pre-lowercased search rows. Filtering yields
    // an ordered list of indices into this slice — one source of truth shared by
    // the renderer and the keyboard handler, so highlight never drifts from the
    // rows on screen. `Vec<usize>` (not `Vec<&KeyEntry>`) also keeps the memo
    // independent of the catalog types' derives.
    let filtered = use_memo(move || {
        let rows = search_index();
        let q = query().to_lowercase();
        if q.is_empty() {
            return (0..rows.len()).collect::<Vec<usize>>();
        }
        rows.iter()
            .enumerate()
            .filter(|(_, r)| r.0.contains(&q))
            .map(|(i, _)| i)
            .collect::<Vec<usize>>()
    });

    // A fresh filter invalidates the old highlight position; reset to the top.
    use_effect(move || {
        let _ = query.read();
        highlighted.set(0);
    });

    let label = if value.is_empty() {
        placeholder.unwrap_or_else(|| "Choose key…".to_string())
    } else {
        key_label(&value)
    };

    rsx! {
        div { class: "key-picker",
            button {
                class: "kp-trigger",
                r#type: "button",
                onclick: move |_| {
                    let next = !open();
                    open.set(next);
                    if next {
                        query.set(String::new());
                        highlighted.set(0);
                    }
                },
                "{label}"
            }

            if open() {
                // Inline-styled backdrop (no dedicated CSS class in the spec): a
                // fixed full-screen catcher under the panel that closes on click.
                // Inline style matches the keypad.rs precedent.
                div {
                    style: "position:fixed;inset:0;z-index:50;",
                    onclick: move |_| open.set(false),
                }

                div { class: "kp-panel",
                    input {
                        class: "kp-search",
                        r#type: "text",
                        autofocus: true,
                        // Belt-and-suspenders: `autofocus` reliably fires on the
                        // first open, but webviews (WebKitGTK here) may skip it on
                        // re-mount, which would kill keyboard nav. Focus imperatively
                        // on mount so every open lands the caret in the search box.
                        onmounted: move |e| {
                            spawn(async move {
                                let _ = e.set_focus(true).await;
                            });
                        },
                        placeholder: "Search keys…",
                        value: "{query}",
                        oninput: move |e| query.set(e.value()),
                        onkeydown: move |e| {
                            let len = filtered.peek().len();
                            match e.key() {
                                Key::Escape => {
                                    e.prevent_default();
                                    open.set(false);
                                }
                                Key::ArrowDown => {
                                    e.prevent_default();
                                    if len > 0 {
                                        let h = (*highlighted.peek() + 1).min(len - 1);
                                        highlighted.set(h);
                                    }
                                }
                                Key::ArrowUp => {
                                    e.prevent_default();
                                    let h = highlighted.peek().saturating_sub(1);
                                    highlighted.set(h);
                                }
                                Key::Enter => {
                                    e.prevent_default();
                                    let h = *highlighted.peek();
                                    if let Some(&row) = filtered.peek().get(h) {
                                        on_pick.call(search_index()[row].1.evdev.to_string());
                                        open.set(false);
                                    }
                                }
                                _ => {}
                            }
                        },
                    }

                    div { class: "kp-list",
                        {
                            let rows = search_index();
                            let matched = filtered.read();
                            if matched.is_empty() {
                                rsx! { div { class: "kp-empty", "No keys match" } }
                            } else {
                                let hl = *highlighted.read();
                                // Walk matched indices (catalog order). Open a new
                                // group whenever the category changes; `flat` is the
                                // running position used to match the highlight.
                                // Comparing the head STRING is equivalent to comparing
                                // categories (distinct categories have distinct heads
                                // and rows are contiguous) and needs no enum derives.
                                let mut prev_head: Option<&'static str> = None;
                                let mut nodes: Vec<Element> = Vec::new();
                                let mut group: Vec<Element> = Vec::new();
                                let mut group_head = "";

                                let flush = |nodes: &mut Vec<Element>, head: &str, group: &mut Vec<Element>| {
                                    if group.is_empty() {
                                        return;
                                    }
                                    let rows = std::mem::take(group);
                                    let head = head.to_string();
                                    nodes.push(rsx! {
                                        div { class: "kp-group",
                                            div { class: "kp-group-head", "{head}" }
                                            {rows.into_iter()}
                                        }
                                    });
                                };

                                for (flat, &row) in matched.iter().enumerate() {
                                    let r = &rows[row];
                                    let head = r.2;
                                    if prev_head != Some(head) {
                                        flush(&mut nodes, group_head, &mut group);
                                        group_head = head;
                                        prev_head = Some(head);
                                    }
                                    let entry_cls = if flat == hl {
                                        "kp-entry highlighted"
                                    } else {
                                        "kp-entry"
                                    };
                                    let label = r.1.label;
                                    let evdev = r.1.evdev;
                                    group.push(rsx! {
                                        div {
                                            class: "{entry_cls}",
                                            onmouseenter: move |_| highlighted.set(flat),
                                            onclick: move |_| {
                                                on_pick.call(evdev.to_string());
                                                open.set(false);
                                            },
                                            span { "{label}" }
                                            span { class: "kp-raw", "{evdev}" }
                                        }
                                    });
                                }
                                flush(&mut nodes, group_head, &mut group);

                                rsx! { {nodes.into_iter()} }
                            }
                        }
                    }
                }
            }
        }
    }
}
