//! The entire UI stylesheet as one Rust string constant.
//!
//! We run as a plain `cargo` binary inside the headless `cage` harness, NOT via
//! `dx`, so the `asset!`/Tailwind bundling pipeline does not run. Injecting the
//! CSS as a single `<style>` block at the app root is the loading path proven to
//! render under the harness — see `main.rs::App`.
//!
//! Design language: a precision instrument, not a marketing page. Graphite
//! surfaces with faint elevation, monospace for device data (key legends,
//! actuation depths, hex codes), a clean sans for chrome. Razer green (#44d62c)
//! is reserved for the *active / selected* state — it signals, it doesn't decorate.

pub const APP_CSS: &str = r#"
:root {
  --bg:        #0a0b0c;
  --surface-0: #101214;
  --surface-1: #16191c;
  --surface-2: #1d2125;
  --surface-3: #262b30;
  --line:      #2c3238;
  --line-soft: #21262b;
  --text:      #e7eaec;
  --text-dim:  #9aa3a9;
  --text-mute: #626d75;
  --accent:    #44d62c;
  --accent-dim:#2f7d22;
  --accent-glow: rgba(68,214,44,0.16);
  --danger:    #ff5a4d;
  --amber:     #f5b942;
  --radius:    10px;
  --radius-sm: 6px;
  --sans: 'Inter','Segoe UI',system-ui,-apple-system,sans-serif;
  --mono: 'JetBrains Mono','SFMono-Regular','Cascadia Code',ui-monospace,'Menlo',monospace;
}

* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; height: 100%; }
body {
  background: var(--bg);
  color: var(--text);
  font-family: var(--sans);
  font-size: 14px;
  line-height: 1.45;
  -webkit-font-smoothing: antialiased;
  overflow: hidden;
}
#main, body > div { height: 100vh; }

/* ----- App shell ---------------------------------------------------------- */
.app {
  display: grid;
  grid-template-columns: 232px 1fr;
  grid-template-rows: 56px 1fr;
  grid-template-areas: "brand topbar" "nav content";
  height: 100vh;
  background:
    radial-gradient(1200px 600px at 80% -10%, rgba(68,214,44,0.05), transparent 60%),
    var(--bg);
}

/* ----- Brand block (top-left) -------------------------------------------- */
.brand {
  grid-area: brand;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 0 18px;
  border-bottom: 1px solid var(--line-soft);
  border-right: 1px solid var(--line-soft);
  background: var(--surface-0);
}
.brand-mark {
  width: 22px; height: 22px;
  border-radius: 5px;
  background: linear-gradient(135deg, var(--accent), var(--accent-dim));
  box-shadow: 0 0 14px var(--accent-glow);
  flex: none;
}
.brand-name { font-weight: 700; letter-spacing: -0.01em; font-size: 15px; }
.brand-name .accent { color: var(--accent); }

/* ----- Top bar ------------------------------------------------------------ */
.topbar {
  grid-area: topbar;
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 0 22px;
  border-bottom: 1px solid var(--line-soft);
  background: var(--surface-0);
}
.device-chip {
  display: flex; align-items: center; gap: 9px;
  padding: 6px 12px;
  background: var(--surface-2);
  border: 1px solid var(--line);
  border-radius: 999px;
  font-size: 12.5px;
}
.device-chip .dot {
  width: 7px; height: 7px; border-radius: 50%;
  background: var(--accent);
  box-shadow: 0 0 8px var(--accent);
}
.device-chip .dot.off { background: var(--text-mute); box-shadow: none; }
.device-chip .name { font-weight: 600; }
.device-chip .status { color: var(--text-mute); font-family: var(--mono); font-size: 11px; }

.topbar-spacer { flex: 1; }

.profile-select {
  display: flex; align-items: center; gap: 8px;
  font-size: 12px; color: var(--text-dim);
}
.profile-select .save-profile { padding: 5px 13px; font-size: 12px; }
.profile-select select {
  appearance: none;
  background: var(--surface-2);
  color: var(--text);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  padding: 7px 30px 7px 12px;
  font-family: var(--sans);
  font-size: 13px;
  cursor: pointer;
  background-image:
    linear-gradient(45deg, transparent 50%, var(--text-dim) 50%),
    linear-gradient(135deg, var(--text-dim) 50%, transparent 50%);
  background-position: calc(100% - 16px) 50%, calc(100% - 11px) 50%;
  background-size: 5px 5px, 5px 5px;
  background-repeat: no-repeat;
}

/* ----- Sidebar nav -------------------------------------------------------- */
.nav {
  grid-area: nav;
  background: var(--surface-0);
  border-right: 1px solid var(--line-soft);
  padding: 14px 12px;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.nav-section {
  font-size: 10px; letter-spacing: 0.14em; text-transform: uppercase;
  color: var(--text-mute);
  padding: 14px 12px 7px;
}
.nav-item {
  display: flex; align-items: center; gap: 12px;
  padding: 10px 12px;
  border-radius: var(--radius-sm);
  color: var(--text-dim);
  text-decoration: none;
  font-weight: 500;
  font-size: 13.5px;
  position: relative;
  transition: background .12s, color .12s;
}
.nav-item:hover { background: var(--surface-1); color: var(--text); }
.nav-item.active {
  background: var(--surface-2);
  color: var(--text);
}
.nav-item.active::before {
  content: "";
  position: absolute; left: -12px; top: 8px; bottom: 8px;
  width: 3px; border-radius: 0 3px 3px 0;
  background: var(--accent);
  box-shadow: 0 0 10px var(--accent-glow);
}
.nav-item .ico {
  width: 18px; height: 18px; flex: none;
  display: inline-flex; align-items: center; justify-content: center;
  color: currentColor;
}
.nav-item.active .ico { color: var(--accent); }
.nav-footer { margin-top: auto; padding: 12px; color: var(--text-mute); font-size: 11px; font-family: var(--mono); }

/* ----- Content area ------------------------------------------------------- */
.content {
  grid-area: content;
  overflow-y: auto;
  padding: 28px 34px;
}
.page-head { margin-bottom: 22px; }
.page-title { font-size: 22px; font-weight: 700; letter-spacing: -0.02em; margin: 0; }
.page-sub { color: var(--text-dim); font-size: 13px; margin-top: 5px; }

/* Two-column work area: stage (keypad) + inspector panel. The inspector column
   flexes between a readable min and a capped max instead of a hard 360px, so the
   stage keeps room to scale at ~900px and the panel doesn't bloat at ~1600px. */
.work {
  display: grid;
  grid-template-columns: minmax(0, 1fr) clamp(280px, 26vw, 360px);
  gap: 26px;
  align-items: start;
}
.work.wide { grid-template-columns: minmax(0, 1fr) clamp(260px, 23vw, 320px); }
/* Narrow windows: drop the inspector below the stage so neither column clips. */
@media (max-width: 1040px) {
  .work, .work.wide { grid-template-columns: minmax(0, 1fr); }
}

/* ----- Cards / panels ----------------------------------------------------- */
.card {
  background: var(--surface-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius);
  padding: 20px;
}
.card-title {
  font-size: 11px; letter-spacing: 0.1em; text-transform: uppercase;
  color: var(--text-mute);
  margin: 0 0 14px;
  display: flex; align-items: center; justify-content: space-between;
}
.card-title .val { color: var(--accent); font-family: var(--mono); letter-spacing: 0; }

/* Bind editor header: physical key chip → current function, both legible. */
.bind-head { display: flex; align-items: center; gap: 11px; margin: 0 0 18px; flex-wrap: wrap; }
.bind-head .bind-key {
  font-family: var(--mono); font-size: 17px; font-weight: 700; color: var(--text);
  background: var(--surface-3); border: 1px solid var(--line);
  border-radius: 8px; padding: 4px 13px; line-height: 1.2;
}
.bind-head .bind-sep { color: var(--text-mute); font-size: 15px; }
.bind-head .bind-now { font-size: 16px; font-weight: 600; color: var(--accent); }

/* ----- Keypad stage ------------------------------------------------------- */
/* The stage is a query container: every keypad dimension below is expressed in
   `cqw` (% of the stage's inline width) so the WHOLE keypad — caps, gaps, thumb
   cluster, spacebar — scales together with the window. `--kp` is the master cap
   size; the thumb box and its absolutely-%-positioned children derive from it,
   preserving the device's cap:thumb proportions at any width. clamp() floors the
   scale so nothing collapses at ~900px and caps it so it doesn't bloat at ~1600px. */
.stage {
  container-type: inline-size;
  background:
    radial-gradient(520px 320px at 50% 30%, rgba(68,214,44,0.04), transparent 70%),
    var(--surface-0);
  border: 1px solid var(--line-soft);
  border-radius: 14px;
  padding: 30px 26px 24px;
  /* Master scale tokens, all keyed to container width. */
  --kp:       clamp(40px, 13cqw, 62px);   /* one grid cap */
  --kp-gap:   clamp(7px, 2.3cqw, 11px);   /* inter-cap + inter-row gap */
  --kp-side:  clamp(20px, 7cqw, 34px);    /* grid <-> thumb-cluster gap */
  --kp-thumb: calc(var(--kp) * 2.71);     /* thumb box edge (168/62 ratio) */
  --kp-dir:   calc(var(--kp) * 0.74);     /* thumb direction cap (46/62) */
  --kp-r:     clamp(6px, 1.9cqw, 9px);    /* cap corner radius */
}
.keypad {
  display: flex;
  gap: var(--kp-side);
  align-items: flex-start;
  justify-content: center;
  /* Never let the scaled cluster spill the stage at narrow widths. */
  max-width: 100%;
}
.keypad-main { display: flex; flex-direction: column; gap: var(--kp-gap); }
/* Rows of 5/5/5/4: the short final row aligns LEFT so there's no phantom gap
   where a 5th cap would sit (no centering, no stagger). */
.kp-row { display: flex; gap: var(--kp-gap); justify-content: flex-start; }
/* The 19-key layout dropped the angled top/bottom rows. Neutralize the old
   stagger classes so a row that still carries them stays flush-left. */
.kp-row.angle-top, .kp-row.angle-bottom { transform: none; }

.keycap {
  width: var(--kp); height: var(--kp);
  border-radius: var(--kp-r);
  background: linear-gradient(180deg, var(--surface-3), var(--surface-1));
  border: 1px solid var(--line);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.03), 0 2px 4px rgba(0,0,0,0.4);
  display: flex; flex-direction: column;
  align-items: center; justify-content: center;
  cursor: pointer;
  position: relative;
  transition: transform .1s, border-color .12s, box-shadow .12s;
  user-select: none;
  /* NOT overflow:hidden — that would clip the Calibration popover (anchored
     INSIDE the selected cap, extending to left/right:105%). The `::after`
     overlays below carry their own matching border-radius so they stay inside
     the cap's rounded rect without needing the cap to clip. */
}
.keycap:hover { transform: translateY(-2px); border-color: #3a444c; }
.keycap.selected {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent), 0 0 18px var(--accent-glow);
}
/* Lift the selected cap (and the .cal-modal it hosts as a child) into its own
   high stacking context, so the floating actuation editor paints ABOVE every
   sibling cap — otherwise later-DOM caps (to the right / in lower rows) cover a
   right-opening modal. Applies to the spacebar + thumb button too (they host the
   modal as well). The caps are already position:relative. */
.keycap.selected,
.spacebar.selected,
.thumb-button.selected {
  z-index: 70;
}
.keycap .cap-legend {
  font-family: var(--mono);
  font-size: clamp(10px, 3.2cqw, 13px); font-weight: 600;
  color: var(--text);
}
/* The cap's current-function label: readable (not the old cryptic uppercase
   tag), accent-coloured, single-line and ellipsised so a long bind never
   overflows the cap. */
.keycap .cap-func {
  font-size: clamp(8px, 2.3cqw, 10px); font-weight: 600;
  color: var(--accent); letter-spacing: 0.01em;
  margin-top: 3px;
  max-width: calc(var(--kp) - 6px);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.keycap.disabled { opacity: 0.45; }
.keycap.disabled .cap-legend { text-decoration: line-through; color: var(--text-mute); }
/* Lighting mode: the cap face glows in its colour. */
.keycap.lit::after {
  content: "";
  position: absolute; inset: 0;
  border-radius: var(--kp-r);
  background: var(--key-color, transparent);
  opacity: 0.9;
  mix-blend-mode: screen;
}
.keycap.lit .cap-legend, .keycap.lit .cap-func { position: relative; z-index: 1; mix-blend-mode: difference; }

/* Thumb cluster: octagonal 8-way + wheel + space, stacked. Sizes track the same
   container-scaled `--kp*` tokens as the grid, so the cluster grows/shrinks in
   lockstep with the caps. The thumbpad box is a proportion of the cap unit; its
   direction caps are absolutely positioned by the keypad's `%` slot coords, so
   sizing the box responsively makes the whole octagon scale. */
.keypad-side { display: flex; flex-direction: column; gap: calc(var(--kp-gap) * 1.6); align-items: center; }
.thumbpad {
  position: relative;
  width: var(--kp-thumb); height: var(--kp-thumb);
}
.thumb-dir {
  position: absolute;
  width: var(--kp-dir); height: var(--kp-dir);
  border-radius: var(--kp-r);
  background: linear-gradient(180deg, var(--surface-3), var(--surface-1));
  border: 1px solid var(--line);
  display: flex; align-items: center; justify-content: center;
  font-family: var(--mono); font-size: clamp(9px, 2.6cqw, 11px); font-weight: 600;
  color: var(--text-dim);
  cursor: pointer;
  transition: border-color .12s, color .12s, transform .1s;
}
.thumb-dir:hover { color: var(--text); border-color: #3a444c; transform: scale(1.04); }
.thumb-dir.selected { border-color: var(--accent); color: var(--text); box-shadow: 0 0 0 1px var(--accent), 0 0 14px var(--accent-glow); }
/* Static hub (Lighting/Pressure): a dashed circle dead-center of the box. */
.thumb-hub {
  position: absolute; left: 50%; top: 50%;
  transform: translate(-50%, -50%);
  width: var(--kp-dir); height: var(--kp-dir); border-radius: 50%;
  background: var(--surface-1);
  border: 1px dashed var(--line);
  display: flex; align-items: center; justify-content: center;
  font-size: clamp(8px, 2.2cqw, 9px); color: var(--text-mute); letter-spacing: 0.1em;
}
/* In Bind mode the hub is a single 4-way/8-way TOGGLE button. It shows the
   current mode and flips on click. Centered in the box and width-capped to the
   central gap (thumb box minus the two flanking dir caps) so it never overlaps
   the surrounding N/E/S/W caps. The `.four` state tints it accent so the active
   mode reads at a glance, on top of the label. */
.thumb-hub.toggle {
  left: 50%; top: 50%;
  transform: translate(-50%, -50%);
  width: auto;
  max-width: calc(var(--kp-thumb) - 2 * var(--kp-dir) - 6px);
  height: auto; border-radius: 999px;
  background: var(--surface-2); border: 1px solid var(--line);
  padding: 4px 9px; letter-spacing: 0.04em;
  font-family: var(--sans); font-size: clamp(6.5px, 1.7cqw, 8.5px); font-weight: 700;
  color: var(--text-dim); white-space: nowrap; cursor: pointer; overflow: hidden;
  transition: background .12s, color .12s, border-color .12s;
}
.thumb-hub.toggle:hover { color: var(--text); border-color: #3a444c; }
.thumb-hub.toggle.four { background: var(--accent); color: #07140a; border-color: var(--accent); }

.wheel {
  width: calc(var(--kp-thumb) * 0.54); height: calc(var(--kp) * 0.65); border-radius: 999px;
  background: linear-gradient(180deg, var(--surface-3), var(--surface-1));
  border: 1px solid var(--line);
  display: flex; align-items: center; justify-content: space-around;
  font-family: var(--mono); font-size: clamp(10px, 2.8cqw, 12px); color: var(--text-dim);
}
.wheel .w-seg { cursor: pointer; padding: 4px 6px; border-radius: 6px; }
.wheel .w-seg:hover { color: var(--text); }
.wheel .w-seg.selected { color: var(--accent); }

/* The big thumb actuator (spacebar). `.thumb-button` shares the look so a
   separately-rendered thumb-button cap matches and carries the pressure fill. */
.spacebar, .thumb-button {
  width: var(--kp-thumb); height: var(--kp-dir); border-radius: var(--kp-r);
  background: linear-gradient(180deg, var(--surface-3), var(--surface-1));
  border: 1px solid var(--line);
  display: flex; align-items: center; justify-content: center;
  font-family: var(--mono); font-size: clamp(9px, 2.6cqw, 11px); letter-spacing: 0.16em;
  color: var(--text-dim); cursor: pointer;
  position: relative;
  /* No overflow:hidden — the thumb-button hosts the Calibration popover. The
     pressure fill (`.pressed::after`) is bounded by its own border-radius. */
  transition: border-color .12s, color .12s;
}
.spacebar:hover, .thumb-button:hover { color: var(--text); border-color: #3a444c; }
.spacebar.selected, .thumb-button.selected { border-color: var(--accent); color: var(--text); box-shadow: 0 0 0 1px var(--accent), 0 0 14px var(--accent-glow); }
/* The thumb button (ALT) is round — a distinct, thumb-sized disc, not a bar. */
.thumb-button {
  width: calc(var(--kp-dir) * 1.5); height: calc(var(--kp-dir) * 1.5);
  border-radius: 50%; letter-spacing: 0.1em;
}
.thumb-button.pressed::after { border-radius: 50%; }
/* The disc's label ellipsises so a bound value can't spill outside the circle. */
.thumb-button .tb-label {
  max-width: calc(var(--kp-dir) * 1.5 - 12px);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}

/* Spacebar in Bind mode stacks its legend over a readable current-function
   label (like the grid caps); the spacebar is bindable + analog, so it must
   reflect its bind, not read as inert. */
.spacebar { flex-direction: column; gap: 3px; }
.spacebar .cap-func { max-width: calc(var(--kp-thumb) - 16px); }
.spacebar.disabled, .thumb-button.disabled { opacity: 0.45; }
.spacebar.disabled .cap-legend,
.thumb-button.disabled .tb-label { text-decoration: line-through; color: var(--text-mute); }

/* ----- Dual-layer keypad stack (Customize: Base + Hypershift) -------------- */
/* Two labelled keypads stacked in one stage. Each sits in a `.keypad-layer`
   block under a small `.layer-label`; the divider gives breathing room so the two
   never crowd. Both keypads share the SAME geometry tokens (one component, two
   layers) — they differ only in which layer's binds they show, so the Base and
   Hypershift grids are visually identical. */
.keypad-stack { display: flex; flex-direction: column; gap: 26px; }
.keypad-layer { display: flex; flex-direction: column; gap: 12px; }
.keypad-layer + .keypad-layer { padding-top: 22px; border-top: 1px solid var(--line-soft); }
.layer-label {
  display: inline-flex; align-items: center; gap: 8px; align-self: center;
  font-size: 10px; letter-spacing: 0.14em; text-transform: uppercase; font-weight: 700;
  color: var(--text-mute);
}
.layer-label .dot {
  width: 7px; height: 7px; border-radius: 50%;
  background: var(--accent); box-shadow: 0 0 8px var(--accent-glow);
}
.layer-label.shift { color: var(--amber); }
.layer-label.shift .dot { background: var(--amber); box-shadow: 0 0 8px rgba(245,185,66,0.4); }

/* ----- Inspector / bind editor ------------------------------------------- */
.inspector { display: flex; flex-direction: column; gap: 18px; }
.field { margin-bottom: 14px; }
.field:last-child { margin-bottom: 0; }
.field-label {
  display: block;
  font-size: 11px; letter-spacing: 0.06em; text-transform: uppercase;
  color: var(--text-mute); margin-bottom: 7px;
}
.field-value { font-family: var(--mono); font-size: 14px; color: var(--text); }

.select-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 7px; }
.chip {
  padding: 9px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--surface-1);
  color: var(--text-dim);
  font-size: 12.5px;
  text-align: center;
  cursor: pointer;
  transition: all .12s;
}
.chip:hover { border-color: #3a444c; color: var(--text); }
.chip.active {
  border-color: var(--accent);
  color: var(--text);
  background: rgba(68,214,44,0.08);
}

.text-input {
  width: 100%;
  background: var(--surface-1);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  color: var(--text);
  padding: 10px 12px;
  font-family: var(--mono);
  font-size: 13px;
}
.text-input:focus { outline: none; border-color: var(--accent-dim); }

.empty-inspector {
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  height: 220px; text-align: center; color: var(--text-mute);
  border: 1px dashed var(--line); border-radius: var(--radius);
  gap: 8px;
}
.empty-inspector .big { font-size: 26px; opacity: 0.5; }

/* ----- Layer toggle (Hypershift) ----------------------------------------- */
.layer-toggle {
  display: inline-flex; padding: 3px;
  background: var(--surface-2); border: 1px solid var(--line);
  border-radius: 999px;
}
.layer-toggle button {
  border: none; background: transparent; color: var(--text-dim);
  padding: 6px 16px; border-radius: 999px; cursor: pointer;
  font-family: var(--sans); font-size: 12.5px; font-weight: 600;
  transition: all .12s;
}
.layer-toggle button.active { background: var(--accent); color: #07140a; }
.layer-toggle button.active.shift { background: var(--amber); }

/* ----- Sliders ------------------------------------------------------------ */
.slider-row { display: flex; align-items: center; gap: 14px; }
.slider {
  -webkit-appearance: none; appearance: none;
  flex: 1; height: 5px; border-radius: 3px;
  background: var(--surface-3);
  outline: none;
}
.slider::-webkit-slider-thumb {
  -webkit-appearance: none; appearance: none;
  width: 16px; height: 16px; border-radius: 50%;
  background: var(--accent);
  box-shadow: 0 0 8px var(--accent-glow);
  cursor: pointer; border: 2px solid #0a0b0c;
}
.slider.amber::-webkit-slider-thumb { background: var(--amber); }
.slider-val { font-family: var(--mono); font-size: 13px; min-width: 64px; text-align: right; color: var(--text); }

/* ----- Toggle switch ------------------------------------------------------ */
.switch {
  width: 38px; height: 21px; border-radius: 999px;
  background: var(--surface-3); border: 1px solid var(--line);
  position: relative; cursor: pointer; flex: none;
  transition: background .14s;
}
.switch.on { background: var(--accent-dim); border-color: var(--accent); }
.switch::after {
  content: ""; position: absolute; top: 2px; left: 2px;
  width: 15px; height: 15px; border-radius: 50%;
  background: var(--text); transition: transform .14s;
}
.switch.on::after { transform: translateX(17px); background: #07140a; }
.switch-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; }

/* ----- Color picker ------------------------------------------------------- */
.swatch-grid { display: grid; grid-template-columns: repeat(6, 1fr); gap: 8px; }
.swatch {
  aspect-ratio: 1; border-radius: var(--radius-sm);
  border: 1px solid var(--line); cursor: pointer;
  transition: transform .1s;
}
.swatch:hover { transform: scale(1.08); }
.swatch.active { box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--accent); }
.color-readout {
  display: flex; align-items: center; gap: 12px; margin-top: 14px;
}
.color-preview { width: 44px; height: 44px; border-radius: var(--radius-sm); border: 1px solid var(--line); }
.color-hex { font-family: var(--mono); font-size: 15px; }

/* ----- Banner ------------------------------------------------------------- */
.banner {
  display: flex; align-items: center; gap: 12px;
  padding: 12px 16px;
  background: rgba(245,185,66,0.08);
  border: 1px solid rgba(245,185,66,0.3);
  border-radius: var(--radius);
  color: var(--amber);
  font-size: 13px;
  margin-bottom: 20px;
}
.banner .b-ico { font-size: 16px; }
.banner b { color: #ffd98a; }

/* ----- Lists (macros / profiles) ----------------------------------------- */
.list { display: flex; flex-direction: column; gap: 7px; }
.list-item {
  display: flex; align-items: center; gap: 12px;
  padding: 13px 14px;
  background: var(--surface-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
  cursor: pointer;
  transition: border-color .12s, background .12s;
}
.list-item:hover { border-color: var(--line); background: var(--surface-1); }
.list-item.active { border-color: var(--accent); background: rgba(68,214,44,0.05); }
.list-item .li-main { flex: 1; }
.list-item .li-name { font-weight: 600; font-size: 13.5px; }
.list-item .li-meta { font-size: 11.5px; color: var(--text-mute); font-family: var(--mono); margin-top: 2px; }
.list-item .li-badge {
  font-size: 10px; padding: 3px 8px; border-radius: 999px;
  background: var(--surface-2); color: var(--text-dim); letter-spacing: 0.05em;
}
.list-item .li-badge.live { background: rgba(68,214,44,0.12); color: var(--accent); }

/* ----- Macro step rows ---------------------------------------------------- */
.step {
  display: flex; align-items: center; gap: 12px;
  padding: 10px 12px;
  border-radius: var(--radius-sm);
  background: var(--surface-1);
  border: 1px solid var(--line-soft);
  font-family: var(--mono); font-size: 13px;
}
.step .step-num { color: var(--text-mute); width: 22px; }
.step .step-kind {
  font-size: 10px; text-transform: uppercase; letter-spacing: 0.06em;
  padding: 2px 7px; border-radius: 4px; font-family: var(--sans); font-weight: 600;
}
.step .step-kind.press   { background: rgba(68,214,44,0.14); color: var(--accent); }
.step .step-kind.release { background: var(--surface-3); color: var(--text-dim); }
.step .step-kind.delay   { background: rgba(245,185,66,0.14); color: var(--amber); }
.step .step-body { flex: 1; }
.step .step-x { color: var(--text-mute); cursor: pointer; }
.step .step-x:hover { color: var(--danger); }

/* ----- Buttons ------------------------------------------------------------ */
.btn {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 9px 15px; border-radius: var(--radius-sm);
  border: 1px solid var(--line); background: var(--surface-2);
  color: var(--text); font-family: var(--sans); font-size: 13px; font-weight: 500;
  cursor: pointer; transition: all .12s;
}
.btn:hover { border-color: #3a444c; background: var(--surface-3); }
.btn.primary { background: var(--accent); color: #07140a; border-color: var(--accent); font-weight: 600; }
.btn.primary:hover { background: #50e637; }
.btn.danger { color: var(--danger); }
.btn.danger:hover { border-color: var(--danger); }
.btn.rec { color: var(--danger); }
.btn.rec .rec-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--danger); box-shadow: 0 0 8px var(--danger); }
.btn-row { display: flex; gap: 8px; flex-wrap: wrap; }

.divider { height: 1px; background: var(--line-soft); margin: 16px 0; }

/* ----- Analog calibration (live pressure) -------------------------------- */
.analog-keys { display: flex; flex-direction: column; max-width: 560px; }
/* The live pressure track: a filled bar (current pressure) with thin vertical
   markers for the release / actuation / second-step thresholds. */
.press-bar {
  position: relative;
  height: 22px; border-radius: 6px;
  background: var(--surface-2);
  border: 1px solid var(--line);
  overflow: hidden;
}
.press-fill {
  position: absolute; left: 0; top: 0; bottom: 0;
  background: linear-gradient(90deg, var(--accent-dim), var(--accent));
  box-shadow: 0 0 10px var(--accent-glow);
  transition: width .08s linear;
}
.press-mark {
  position: absolute; top: -1px; bottom: -1px;
  width: 2px; transform: translateX(-1px);
  z-index: 1;
}
.press-mark.actuate { background: var(--accent); box-shadow: 0 0 6px var(--accent); }
.press-mark.release { background: var(--text-mute); }
.press-mark.second  { background: var(--amber); box-shadow: 0 0 6px rgba(245,185,66,0.5); }
.press-readout {
  font-family: var(--mono); font-size: 11.5px; color: var(--text-mute);
  margin-top: 6px; text-align: right;
}

/* ========================================================================== */
/* Page-redesign primitives (modal / menu / pickers / chords / steps / etc.)   */
/* ========================================================================== */

/* ----- Backdrop + modal --------------------------------------------------- */
.backdrop {
  position: fixed; inset: 0; z-index: 40;
  background: rgba(5,6,7,0.62);
  backdrop-filter: blur(1.5px);
  display: flex; align-items: center; justify-content: center;
}
.modal {
  width: min(560px, calc(100vw - 48px));
  max-height: calc(100vh - 64px);
  display: flex; flex-direction: column;
  background: var(--surface-0);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 18px 50px rgba(0,0,0,0.6), 0 0 0 1px rgba(0,0,0,0.3);
  overflow: hidden;
}
.modal-head {
  display: flex; align-items: center; justify-content: space-between; gap: 12px;
  padding: 16px 18px;
  border-bottom: 1px solid var(--line-soft);
  font-size: 14px; font-weight: 700; letter-spacing: -0.01em;
}
.modal-body { padding: 18px; overflow-y: auto; }
.modal-foot {
  display: flex; align-items: center; justify-content: flex-end; gap: 8px;
  padding: 14px 18px;
  border-top: 1px solid var(--line-soft);
  background: var(--surface-0);
}
.modal-x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 28px; height: 28px; flex: none;
  border: none; background: transparent;
  color: var(--text-mute); border-radius: var(--radius-sm);
  font-size: 16px; line-height: 1; cursor: pointer;
  transition: background .12s, color .12s;
}
.modal-x:hover { background: var(--surface-2); color: var(--text); }

/* ----- Popover + kebab menu ----------------------------------------------- */
.popover {
  position: absolute; z-index: 30;
  background: var(--surface-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 12px 32px rgba(0,0,0,0.55);
  padding: 6px;
}
.menu {
  position: absolute; z-index: 30;
  min-width: 168px;
  background: var(--surface-1);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  box-shadow: 0 12px 32px rgba(0,0,0,0.55);
  padding: 5px;
  display: flex; flex-direction: column; gap: 1px;
}
.menu-item {
  display: flex; align-items: center; gap: 10px;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  color: var(--text-dim);
  font-size: 13px; font-weight: 500;
  cursor: pointer; text-align: left;
  border: none; background: transparent; width: 100%;
  transition: background .12s, color .12s;
}
.menu-item:hover { background: var(--surface-2); color: var(--text); }
.menu-item.danger { color: var(--danger); }
.menu-item.danger:hover { background: rgba(255,90,77,0.1); color: var(--danger); }
.menu-sep { height: 1px; background: var(--line-soft); margin: 4px 6px; }
.kebab {
  display: inline-flex; align-items: center; justify-content: center;
  width: 30px; height: 30px; flex: none;
  border: none; background: transparent;
  color: var(--text-mute); border-radius: var(--radius-sm);
  font-size: 17px; line-height: 1; cursor: pointer;
  transition: background .12s, color .12s;
}
.kebab:hover { background: var(--surface-2); color: var(--text); }

/* ----- Key picker --------------------------------------------------------- */
.key-picker { position: relative; }
.kp-trigger {
  display: inline-flex; align-items: center; justify-content: space-between; gap: 8px;
  width: 100%;
  padding: 9px 12px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--surface-1);
  color: var(--text);
  font-family: var(--mono); font-size: 13px;
  cursor: pointer; text-align: left;
  transition: border-color .12s, background .12s;
}
.kp-trigger:hover { border-color: #3a444c; background: var(--surface-2); }
.kp-panel {
  /* Above the picker's inline backdrop (fixed, z-index:50) so the panel paints
     over it and stays clickable instead of being covered by the catcher. */
  position: absolute; z-index: 60; top: calc(100% + 6px); left: 0;
  width: 280px; max-height: 340px;
  display: flex; flex-direction: column;
  background: var(--surface-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 14px 36px rgba(0,0,0,0.6);
  overflow: hidden;
}
.kp-search {
  flex: none;
  margin: 8px; padding: 8px 11px;
  background: var(--surface-2);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  color: var(--text);
  font-family: var(--sans); font-size: 13px;
}
.kp-search:focus { outline: none; border-color: var(--accent-dim); }
.kp-list { flex: 1; overflow-y: auto; padding: 0 4px 6px; }
.kp-group { padding-top: 2px; }
.kp-group-head {
  position: sticky; top: 0; z-index: 1;
  padding: 7px 9px 5px;
  background: var(--surface-1);
  font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase;
  color: var(--text-mute);
}
.kp-entry {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  padding: 7px 9px;
  border-radius: var(--radius-sm);
  font-size: 13px; color: var(--text-dim);
  cursor: pointer;
}
.kp-entry:hover { background: var(--surface-2); color: var(--text); }
.kp-raw { font-family: var(--mono); font-size: 11px; color: var(--text-mute); flex: none; }
.kp-entry.highlighted {
  background: rgba(68,214,44,0.1);
  color: var(--text);
  box-shadow: inset 0 0 0 1px var(--accent-dim);
}
.kp-empty {
  padding: 22px 12px; text-align: center;
  font-size: 12.5px; color: var(--text-mute);
}

/* ----- Customize: chord editor + segmented control ------------------------ */
.chord-pills { display: flex; flex-wrap: wrap; align-items: center; gap: 7px; }
.pill {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 6px 8px 6px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--surface-2);
  color: var(--text);
  font-family: var(--mono); font-size: 12.5px; font-weight: 600;
}
.pill-x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 16px; height: 16px; flex: none;
  border-radius: 4px;
  color: var(--text-mute); cursor: pointer;
  font-size: 13px; line-height: 1;
  transition: background .12s, color .12s;
}
.pill-x:hover { background: rgba(255,90,77,0.12); color: var(--danger); }
.pill-add {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 6px 11px;
  border-radius: var(--radius-sm);
  border: 1px dashed var(--line);
  background: transparent;
  color: var(--text-dim);
  font-family: var(--sans); font-size: 12.5px; font-weight: 600;
  cursor: pointer;
  transition: border-color .12s, color .12s;
}
.pill-add:hover { border-color: var(--accent-dim); color: var(--text); }

/* Reusable segmented control (Customize, Lighting, thumb toggle). */
.seg {
  display: inline-flex; padding: 3px;
  background: var(--surface-2); border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  gap: 2px;
}
.seg button {
  border: none; background: transparent; color: var(--text-dim);
  padding: 6px 13px; border-radius: 4px; cursor: pointer;
  font-family: var(--sans); font-size: 12.5px; font-weight: 600;
  transition: background .12s, color .12s;
}
.seg button:hover { color: var(--text); }
.seg button.active { background: var(--accent); color: #07140a; }

/* ----- Macros page -------------------------------------------------------- */
.work.macros { grid-template-columns: 220px 1fr; }
.macro-toolbar {
  display: flex; align-items: center; gap: 10px;
  padding: 9px 12px;
  background: var(--surface-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
}
.step-flow { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.step-chip {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 6px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--surface-1);
  color: var(--text);
  font-family: var(--mono); font-size: 12px;
  cursor: pointer;
  transition: border-color .12s, box-shadow .12s;
}
.step-chip .num {
  display: inline-flex; align-items: center; justify-content: center;
  min-width: 16px; height: 16px;
  border-radius: 4px;
  background: var(--surface-3);
  color: var(--text-mute);
  font-size: 10px; font-family: var(--sans); font-weight: 700;
}
.step-chip.press   { border-color: var(--accent-dim); background: rgba(68,214,44,0.1); }
.step-chip.press .num { background: rgba(68,214,44,0.18); color: var(--accent); }
.step-chip.release { background: var(--surface-1); color: var(--text-dim); }
.step-chip.delay {
  border-color: rgba(245,185,66,0.4);
  background: rgba(245,185,66,0.08);
  color: var(--amber);
  padding: 5px 9px; font-size: 11.5px;
}
.step-chip.editing { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent), 0 0 12px var(--accent-glow); }
.chip-x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 15px; height: 15px; flex: none;
  border-radius: 4px;
  color: var(--text-mute); cursor: pointer;
  font-size: 12px; line-height: 1;
  transition: background .12s, color .12s;
}
.chip-x:hover { background: rgba(255,90,77,0.12); color: var(--danger); }

/* ----- Calibration: per-key pressure fill + floating modal ---------------- */
/* Bottom-rising overlay, accent-tinted, behind the legend (cf. .keycap.lit).
   The grid cap, spacebar, and thumb caps all share the same fill so analog
   pressure reads identically on every actuator. `--press` is the 0..100% height. */
.keycap.pressed::after,
.spacebar.pressed::after,
.thumb-dir.pressed::after,
.thumb-button.pressed::after {
  content: "";
  position: absolute; left: 0; right: 0; bottom: 0;
  height: var(--press, 0%);
  /* Match the cap's corner radius so the bottom-rising fill follows the rounded
     rect instead of poking past it — the caps no longer overflow:hidden (that
     would clip the Calibration popover). At low fill the rounded top corners sit
     on a 22%-opacity sliver and read flush. */
  border-radius: var(--kp-r);
  background: linear-gradient(0deg, var(--accent), var(--accent-dim));
  opacity: 0.22;
  transition: height .08s linear;
  pointer-events: none;
}
.keycap.pressed .cap-legend, .keycap.pressed .cap-func { position: relative; z-index: 1; }

/* The keypad anchors this popover INSIDE the selected cap (the cap is
   position:relative), so it floats beside that cap instead of the stage. Default
   opens to the RIGHT; right-column caps add `.open-left` to open the other way so
   the 240px panel never runs off-screen. High z-index keeps it over its neighbours. */
.cal-modal {
  position: absolute; z-index: 60;
  top: 0; left: 105%;
  width: 240px;
  background: var(--surface-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 14px 36px rgba(0,0,0,0.6);
  padding: 14px;
}
.cal-modal.open-left { left: auto; right: 105%; }
.cal-modal-head {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  margin-bottom: 12px;
  font-family: var(--mono); font-size: 13px; font-weight: 600;
}

/* Calibration multi-select: caps in the selection set get an accent ring (drag
   or Shift+click). Distinct from the single `.selected` glow. */
.keycap.multi, .spacebar.multi {
  border-color: var(--accent);
  box-shadow: inset 0 0 0 1px var(--accent), 0 0 12px var(--accent-glow);
}

/* Bulk actuation editor — a centred floating dialog (targets many caps, so it
   isn't cap-anchored like .cal-modal) over its own dimming backdrop. */
.multi-backdrop {
  position: fixed; inset: 0; z-index: 80;
  background: rgba(5,6,7,0.62);
  backdrop-filter: blur(1.5px);
}
.multi-modal {
  position: fixed; z-index: 81;
  top: 50%; left: 50%; transform: translate(-50%, -50%);
  width: 320px; max-width: calc(100vw - 48px);
  background: var(--surface-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 20px 50px rgba(0,0,0,0.65);
  padding: 18px;
}

/* ----- Profiles page ------------------------------------------------------ */
.profile-toolbar {
  display: flex; align-items: center; gap: 14px;
  min-height: 44px;
  padding: 0 4px;
  margin-bottom: 14px;
}
.profile-row {
  display: grid;
  grid-template-columns: auto 1fr minmax(170px, 240px) 32px;
  align-items: center;
  gap: 14px;
  padding: 12px 14px;
  background: var(--surface-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
  transition: border-color .12s, background .12s;
}
.profile-row:hover { border-color: var(--line); background: var(--surface-1); }
/* .is-default before .active (equal specificity) so a row that is BOTH active
   and default shows the ACTIVE accent background, not the default surface. */
.profile-row.is-default { border-color: var(--line); background: var(--surface-1); }
.profile-row.active { border-color: var(--accent); background: rgba(68,214,44,0.05); }
/* .li-badge is descendant-scoped to .list-item upstream; re-base it for rows. */
.profile-row .li-badge {
  font-size: 10px; padding: 3px 8px; border-radius: 999px;
  background: var(--surface-2); color: var(--text-dim); letter-spacing: 0.05em;
}
.profile-row .li-badge.live { background: rgba(68,214,44,0.12); color: var(--accent); }
.li-badge.default { background: var(--surface-3); color: var(--text-dim); }
.li-name-input {
  width: 100%;
  background: transparent;
  border: 1px solid transparent;
  border-radius: var(--radius-sm);
  color: var(--text);
  padding: 6px 8px;
  font-family: var(--sans); font-size: 14px; font-weight: 600;
  transition: border-color .12s, background .12s;
}
.li-name-input:hover { border-color: var(--line-soft); }
.li-name-input:focus { outline: none; border-color: var(--accent-dim); background: var(--surface-1); }
.link-chip {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 4px 8px;
  border-radius: 999px;
  border: 1px solid var(--line);
  background: var(--surface-2);
  color: var(--text-dim);
  font-size: 11.5px;
}
.link-chip .glyph { font-size: 11px; opacity: 0.85; }
.link-x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 14px; height: 14px; flex: none;
  border-radius: 50%;
  color: var(--text-mute); cursor: pointer;
  font-size: 11px; line-height: 1;
  transition: background .12s, color .12s;
}
.link-x:hover { background: rgba(255,90,77,0.14); color: var(--danger); }
.link-chip.disabled { opacity: 0.45; }
.link-add {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 4px 10px;
  border-radius: 999px;
  border: 1px dashed var(--line);
  background: transparent;
  color: var(--text-dim);
  font-size: 11.5px; font-weight: 600;
  cursor: pointer;
  transition: border-color .12s, color .12s;
}
.link-add:hover { border-color: var(--accent-dim); color: var(--text); }
"#;
