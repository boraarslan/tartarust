# GPUI migration plan

Replace the Dioxus 0.7 webview UI in `tartarust-ui` with GPUI, the native
GPU-accelerated framework from the Zed team (<https://gpui.rs>). This document
is the plan of record for the `gpui-migration` branch.

## 1. Goal

- Port `tartarust-ui` from Dioxus (webview, webkit2gtk) to GPUI (native, Vulkan).
- Keep full feature parity: all 5 screens, the keypad widget, live calibration, and profile IO.
- Remove the webkit2gtk-4.1 and gtk3 build dependencies.
- Keep the headless `cage` + `grim` screenshot harness as the only dev run path.

## 2. Non-goals

- No changes to the daemon, the CLI, `tartarust-protocol`, or `tartarust-hid`.
- No changes to the `tartarust-ipc` wire schema. The daemon does not restart for this migration.
- No new features. Parity first.

## 3. Target stack

| Crate | Version | Purpose |
|---|---|---|
| `gpui` | `=0.2.2` (crates.io) | Core framework: entities, views, elements, layout, text, windowing |
| `gpui-component` | `=0.5.1` (crates.io) | Text input widget only (see Decision 2) |

Verified in Phase 0: the `gpui_platform` companion crate exists only on zed git
main, not in the crates.io release. `gpui 0.2.2` default features are
`["font-kit", "wayland", "x11", "windows-manifest"]` — Linux needs nothing
extra. `gpui-component 0.5.1` depends on `gpui ^0.2.2` from crates.io, so the
version-conflict risk is retired.

GPUI is pre-1.0 and breaks between versions. Pin exact versions. Upgrade only
as a deliberate, separate change. GPUI requires the latest stable Rust
(workspace is on 1.95 — verify in Phase 0).

Licenses: `gpui` and `gpui-component` are Apache-2.0. Both are compatible with
this project's GPL-3.0-or-later.

### Decision 1: rewrite in place on this branch

Rewrite the UI crate module by module on `gpui-migration`. Do not add a
feature flag that keeps both stacks buildable — that would keep webkit2gtk in
the dependency tree and double the CI cost. The `master` branch holds the
Dioxus reference implementation until merge.

### Decision 2: hand-roll widgets, except text input

The current UI already hand-rolls almost everything: sliders, switches,
segmented controls, the color swatch grid, popovers, modals, and the 764-line
keypad are all styled `div`s. These port directly to GPUI `div()` trees.

Text input is the exception. A correct input field needs selection, clipboard,
IME, and focus handling — a large lift in raw GPUI (its `input.rs` example
exists because none is built in). The app has 7 text inputs, 1 number input,
and 1 search box. Plan: adopt the `gpui-component` input module for these.
Fallback if its pinned `gpui` version conflicts with `=0.2.2`: vendor a
minimal input widget into `components/`.

### Decision 3: drop tokio from the UI crate

GPUI owns the event loop and has its own foreground and background executors.
The IPC transport is one newline-delimited JSON request per connection —
trivially blocking-friendly. Rewrite `ipc.rs` `send_on` on
`std::os::unix::net::UnixStream` and run each call on
`cx.background_executor()`. This removes the tokio runtime from the UI
process. The calibration 100 ms poll becomes a `cx.spawn` loop with the
background executor's timer.

## 4. What survives, what is rewritten

| Module | Lines | Fate |
|---|---|---|
| `model.rs` | 881 | Survives unchanged (pure domain + geometry, no Dioxus) |
| `desktop.rs` | 736 | Survives. Swap `tao::window::Icon` decode for GPUI's equivalent or drop it (see §6 window icon) |
| `ipc.rs` | 1139 | Logic survives. Transport swap (~100 lines) per Decision 3. Tests move off `#[tokio::test]` to std-thread fakes |
| `lib.rs` | 159 | Rewrite: `Application::new().run()`, root view, screen enum replaces the router |
| `style.rs` | 1168 | Deleted. Replaced by a `theme.rs` (color/spacing constants from the 389 CSS variables) plus inline fluent styling |
| `screens/*` | 3120 | Rewrite: RSX → `Render` impls with `div()` builders |
| `components/*` | 1282 | Rewrite: same structure, GPUI elements |

Roughly 5,700 lines to rewrite, 2,700 to keep.

## 5. Concept mapping

| Dioxus (current) | GPUI (target) |
|---|---|
| `Signal<AppState>` via `use_context_provider` (`lib.rs:109`) | One `Entity<AppState>` owned by the root view. Children hold the handle, mutate via `entity.update(cx, ...)`, re-render via `cx.notify()` |
| `use_signal` locals (39 sites) | View struct fields on the owning view's `Entity` |
| `use_memo`, `use_resource`, `use_future` | Plain fields recomputed in `render`, or `cx.spawn` tasks |
| Router + 5 `#[layout(Shell)]` routes | `enum Screen { Customize, Lighting, Calibration, Macros, Profiles }` field on the root view. Tab rail sets it |
| `spawn(async ...)` fire-and-forget IPC (19 sites) | `cx.spawn` / background executor tasks. Keep the `fire_or_resync` optimistic-write pattern verbatim |
| 1,150-line CSS const, 389 `var(--...)` | `Theme` struct + tailwind-style fluent API (`.flex()`, `.bg()`, `.hover()`) |
| `:hover`, `:focus-visible` | `.hover(...)`, focus styles via `FocusHandle` state |
| `transition`, `@keyframes cal-panel-in` | `with_animation` on the affected elements. Drop the `prefers-reduced-motion` query or gate on an env var |
| `::-webkit-slider-thumb`, `::-webkit-scrollbar` | Explicit thumb/track child elements in the custom `Slider`; custom scrollbar element on scroll handles |
| Container-query font clamp in keypad caps | Compute font size from measured element bounds, or fix sizes (window is fixed 1280×800) |
| `document::eval` focus call (`profiles.rs:422`) | `FocusHandle::focus()` — the one JS interop site disappears |
| `WindowBuilder::with_decorations(false)` + `window.drag()` titlebar | `WindowOptions` client-side decorations. Map drag/minimize/maximize/close to GPUI window APIs (Zed ships CSD on Linux, so the surface exists — verify names in Phase 0) |
| `onmousedown`/`onmouseup` + cap-enter drag-sweep select | `on_mouse_down` / `on_mouse_up` / `on_mouse_move` on elements. Same `CapPointer`/`CapPhase` model |
| Native `<select>` (2 sites) | Small popover list widget (KeyPicker already is one — reuse its chassis) |
| `dioxus::logger` | `tracing_subscriber` directly (tracing is already a dep) |

## 6. Open platform questions (resolve in Phase 0)

1. **Headless rendering.** GPUI renders through Vulkan (blade) on Linux. The
   harness compositor is software (`WLR_RENDERER=pixman`), but the *client*
   needs a Vulkan device. This machine has only the NVIDIA ICD (RTX 4070 Ti) —
   no lavapipe (`vulkan-swrast` is in the repos, not installed). A GPU-Vulkan
   client needs the compositor to advertise linux-dmabuf, which pixman does
   not. Spike order: try `HEADLESS_WLR_RENDERER=gles2` (or `vulkan`) so cage
   runs on the GPU, else install `vulkan-swrast` and pin `VK_DRIVER_FILES` to
   lavapipe under pixman. The spike must produce a `grim` screenshot of a GPUI
   hello-world under `cage` before anything else proceeds.
2. **Window icon and app id.** X11 taskbar icon currently comes from a decoded
   PNG via tao. Check whether `WindowOptions` exposes an app id (for
   `WM_CLASS`/Wayland `app_id` = `tartarust-ui`) and an icon. If no icon API
   exists: X11 falls back to the desktop entry like Wayland does, and
   `desktop.rs` install logic already covers that. The `png` dep may drop.
3. **Fonts.** The CSS stack (`TX-02`/`Berkeley Mono`/... and `IBM Plex
   Sans`/...) resolves via fontconfig today. GPUI resolves via font-kit.
   Verify family-name matching for the installed fonts and encode the fallback
   order in `theme.rs`.
4. **CI build dependencies.** Replace the webkit/gtk apt set with GPUI's Linux
   needs (expected: `libwayland-dev`, `libxkbcommon-x11-dev`, `libfontconfig1-dev`,
   `libx11-dev`/xcb libs, Vulkan headers — confirm from first CI failure).
   Measure the clean-build time delta and add `Swatinem/rust-cache` if CI slows
   materially.

## 7. Phases

Each phase ends rustfmt-clean and clippy-clean. Screenshots come from
`./scripts/headless-screenshot.sh` only — never a real window.

### Phase 0 — Spike (de-risk before commitment)

1. Add `gpui` to a scratch example (`spike/`, workspace-excluded), not to the workspace yet.
2. Render a hello-world window. Run it under the headless harness. Capture a screenshot.
3. Update `scripts/headless-screenshot.sh`: remove the five `WEBKIT_*`/`GDK_*` lines, add the Vulkan/lavapipe env.
4. Test `gpui-component`'s input against crates.io `gpui =0.2.2`. Record the verdict for Decision 2.
5. Confirm Rust 1.95 satisfies GPUI's toolchain floor.
6. Record build time and binary size.

Exit: a committed screenshot of a GPUI window from the harness. If Vulkan
cannot work headless, stop and re-plan before any rewrite work.

**Phase 0 results (2026-07-31): PASSED.**

- `screenshots/gpui-spike.png` renders correctly from the harness with
  `HEADLESS_WLR_RENDERER=gles2` (compositor on the AMD iGPU advertises
  linux-dmabuf, GPUI client renders on the NVIDIA ICD — cross-GPU works).
  Under the default `pixman` renderer, the client dies with
  `Failed to create surface: PlatformNotSupported` — GPUI runs need `gles2`.
- `gpui =0.2.2` + `gpui-component =0.5.1` compile together from crates.io. The
  `Input` widget renders. `Root` wrapper + `gpui_component::init(cx)` required.
- `WindowOptions.app_id` exists and works. There is NO window-icon API — X11
  taskbar icon now comes from the desktop entry (like Wayland). Drop `png`.
- Font check: `font_family("Berkeley Mono")` resolves the installed face; a
  missing family (`TX-02`) falls back to a legible mono default.
- rustc 1.97.1 builds it. Cold build 35 s on this machine (fast linker),
  debug binary ~546 MB (debuginfo — add a `split-debuginfo` or strip note to
  the dev profile if it bothers).

### Phase 1 — Foundation

1. Swap `tartarust-ui` dependencies: remove `dioxus`, add the GPUI crates.
2. Write `theme.rs` from the `style.rs` variable block (colors, spacing, radii, font stacks).
3. Rewrite `ipc.rs` transport per Decision 3. Port its 10 tests to std-thread fake daemons.
4. Rewrite `lib.rs`: `Application` setup, root view with `Screen` enum, `Entity<AppState>` store, offline mock state, the `TARTARUST_SCREEN` harness hook, and the existing daemon autostart + 12×500 ms retry load sequence.
5. Port `Shell`: masthead, custom window controls (drag, minimize, maximize with 400 ms double-press, close), tab rail, status strip.

Exit: shell renders all-offline with empty content region, screenshot passes.

**Phase 1 results (2026-07-31): DONE.**

- `screenshots/gpui-shell.png`: masthead (nameplate, annunciator, profile
  readout, window controls), tab rail with active underline, stub content,
  status strip — all offline-mock, via the harness (`gles2`).
- New modules: `theme.rs` (palette + font resolver as a GPUI global),
  `store.rs` (fire/resync/marks/overlay on `Entity<AppState>`), rewritten
  `lib.rs` + `components/shell.rs`. `ipc.rs` is sync std transport +
  `req_*` builders + `Snapshot` loader; its fake-daemon tests are std-thread.
- `cargo fmt` / `clippy -D warnings` / `cargo test -p tartarust-ui` (26/26)
  all green. `dioxus`, `png`, `tokio` are out of the UI dependency tree.
- Deferred in-phase: profile picker is a readout until the Phase 2 dropdown;
  client-side decorations black-frame → Phase 5 (see §8); old Dioxus screen/
  component/style files stay on disk undeclared as porting references.

### Phase 2 — Widget kit

Port `components/widgets.rs` and the small interactive set: `Slider` (with
amber variant, marker overlays), `Switch`/`Toggle`, `SpeedSeg` segmented
control, `PageHead`, popover chassis, modal/dialog chassis (overlay +
stop-propagation + busy/error states), color swatch grid, scroll area with
styled scrollbar, and the adopted text input. Small gallery screen behind
`TARTARUST_SCREEN=gallery` (dev-only) to screenshot the kit in one shot.

**Phase 2 results (2026-07-31): DONE.** `screenshots/gpui-gallery.png`
(`TARTARUST_SCREEN=gallery`). Decision 2 amended: gpui-component provides the
interactive primitives (Input, Slider+SliderState, Switch), themed via
`theme::apply_component_theme` (ThemeMode::Dark first, then palette
overrides). Hand-rolled: segmented control, buttons, LEDs, card, page_head
(`components/widgets.rs`), and the masthead profile dropdown (overlay +
absolute list pattern in shell.rs — the house popover chassis). Store gained
`switch_profile_and_reload` + `fetch_macro`. gpui-component's `TitleBar`
handles `Decorations::Client` — earmarked for the Phase 5 CSD fix. Gates
green.

### Phase 3 — Keypad

Port `components/keypad.rs` and `components/key_picker.rs`: all three modes
(`Bind`, `Lighting`, `Pressure`), 19 caps + thumb pad + wheel + spacebar +
thumb button geometry, pressure fill bars, per-key glow, luminance-based
legend color (the 5 pure helper tests survive as-is), multi-select
(click/shift/drag-sweep via mouse events), and the anchored in-cap popover
(`children` slot). This is the highest-effort single component — budget
accordingly.

**Phase 3 results (2026-07-31): DONE.** `components/keypad.rs` (1241 lines,
`RenderOnce` builder: mode/layer + binds/lighting/pressures/selection +
on_select/on_cap_pointer/on_axis_mode handlers) and `components/key_picker.rs`
(318 lines, `EventEmitter` view) — verified via the gallery screenshot with
mock binds (grid + thumb pad + wheel + space + thumb button, selection ring,
customized tick). The 5 color-helper tests survive. KeyPicker gets its runtime
exercise from the Phase 4 Macros/Customize screens.

**Harness note:** black screenshots were a FIRST-PRESENT RACE, not a rendering
bug — `grim` can capture before the client's first frame, and a solid-black
capture is a ~5 KB PNG. The harness inner script now retries while the capture
is < 10 KB (5 attempts, 2 s apart). The Phase 1 "CSD renders black" finding
may have been this race — retest `WindowDecorations::Client` in Phase 5 before
building the CSD frame.

### Phase 4 — Screens

Port in this order (rising complexity, each screenshot-verified against the
committed Dioxus PNGs):

1. **Lighting** — no local state, exercises keypad Lighting mode + widget kit.
2. **Customize** — dual keypads, `BindEditor` with pending-type switching, `ChordEditor`, `MousePicker`, `NamePicker`, `LaunchEditor`.
3. **Macros** — name rail, chip-flow step editor, delay number input, KeyPicker integration.
4. **Calibration** — live 100 ms analog poll loop (screen-local state, cancelled on screen switch), heatmap keypad, `CalModal`, Edit Mode bulk panel, drag-sweep.
5. **Profiles** — inline rename with focus, kebab menu, export/import dialogs, `bounded_blocking` file IO on the background executor.

**Phase 4 results (2026-07-31): DONE.** All five screens ported (agent-ported,
integrator-wired): `profiles.rs` (1342 lines), `lighting.rs` (652),
`customize.rs` (1128), `macros.rs` (~900), `calibration.rs` (with the 100 ms
poll as a held `Task<()>` — dropping the view cancels it). Each verified by a
harness screenshot (`screenshots/gpui-<screen>.png`). Shell owns lazy
per-screen view entities (`ensure_screen`). Known intentional deviations,
flagged in the ports: Customize uses one shared KeyPicker that closes on
selection/type change; the launch-path field drops an uncommitted edit if the
selection changes before Enter/blur (safer than the old wrong-target commit);
the graph-paper stage background is gone; the Customize stage scrolls instead
of the page.

### Phase 5 — Desktop integration and parity pass

1. Resolve the window icon / app id question from §6.2 in `desktop.rs`.
2. Re-verify daemon autostart, the env gates, and `install-desktop` CLI mode. Their 15 tests must pass unchanged.
3. Full-app pass against a **live daemon** (manual, on the real device): every mutation path in §9.
4. Regenerate all committed screenshots. Compare with the Dioxus set side by side.
5. Update CI: swap the apt package set, keep the three gates.
6. Update docs: `CLAUDE.md` build-deps line, `README.md`, `docs/PLAN.md`.

**Phase 5 results (2026-07-31): automatable part DONE.** CI apt set swapped to
`libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype-dev`
(verify on the first CI run — extend from build errors if needed). README +
CLAUDE.md updated (GPUI, new deps, harness). Harness default renderer flipped
to `gles2`; capture retry added. CSD verified working (risk retired). Desktop
integration covered by its 14 unchanged tests. **OUTSTANDING — needs the user
and the real device: the live-daemon pass (§9 mutation list) and a real-
compositor check of drag/minimize/maximize/CSD.**

### Phase 6 — Cleanup and release

1. Delete dead code: `style.rs`, unused deps (`png` if §6.2 allows, `serde_json` only if now unused — it is used by ipc.rs, so it stays).
2. `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` — all green.
3. Bump `[workspace.package].version`. The IPC schema is unchanged, so no daemon restart is required. Suggest `0.2.0` to signal the toolkit swap. Decide at merge.

**Phase 6 results (2026-07-31): DONE (except the version bump — user's call
at merge).** `style.rs` deleted; every module-level migration `allow` removed
(zero dead code — store.rs now uses the `req_*` builders throughout);
`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
and `cargo test --workspace` (192 tests) all green; final screenshot sweep of
all 7 shots (5 screens + hypershift + gallery) captured with the final
binary. Suggested bump: `0.2.0` (toolkit swap; IPC unchanged, so no daemon
restart is required).

## 8. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Vulkan unavailable in the headless harness | Blocks the whole dev workflow | Phase 0 gate. lavapipe. If dead: re-plan before rewrite |
| GPUI pre-1.0 API churn | Rework mid-migration | Pin `=0.2.2`. No upgrades during the migration |
| ~~`gpui-component` pins a git `gpui`, conflicts with crates.io pin~~ | Retired | Phase 0 verified: crates.io `gpui-component 0.5.1` requires `gpui ^0.2.2` |
| Text input quality (IME, selection, clipboard) | Profiles/Customize/KeyPicker degrade | Adopt, don't build. Test IME early in Phase 2 |
| GPUI compile time inflates CI | Slow feedback | rust-cache; measure in Phase 0 |
| Font resolution differs from fontconfig | Visual drift on the nameplate/mono readouts | Explicit fallback list in `theme.rs`, verify in Phase 0 screenshot |
| ~~Client-side decorations API gaps on Linux~~ | Retired | The Phase 1 "CSD renders black" finding was the harness first-present race (see Phase 3 harness note). With capture retries, `WindowDecorations::Client` renders clean and is shipped |
| Big-bang branch drifts from master | Painful merge | Keep `model.rs`/`desktop.rs`/IPC helper signatures identical so master fixes cherry-pick cleanly |

## 9. Acceptance criteria

- All 5 screens render under the headless harness with visual parity to the committed Dioxus screenshots.
- Against a live daemon: bind edit + save, lighting effect/brightness/custom paint, calibration single + bulk edit with live pressure, macro create/edit/delete, profile create/rename/link/default/export/import/delete, auto-switch toggle.
- Offline mode still renders the full mock state, and the reconnect/resync logic behaves as today.
- No `webkit2gtk`, `gtk`, `tao`, or `tokio` in `tartarust-ui`'s dependency tree.
- CI green on all three gates with the new apt set.
- `cargo install --path .` still installs `tartarust-daemon`, `tartarust`, `tartarust-ui`.
