//! The entire UI stylesheet as one Rust string constant.
//!
//! We run as a plain `cargo` binary inside the headless `cage` harness, NOT via
//! `dx`, so the `asset!`/Tailwind bundling pipeline does not run. Injecting the
//! CSS as a single `<style>` block at the app root is the loading path proven to
//! render under the harness — see `lib.rs::App`.
//!
//! Design language (v5 "bench instrument", spec: docs/design/v5-bench-instrument.md):
//! the faceplate of a precision test instrument. Warm-iron surfaces, TX-02 for
//! identity + data, IBM Plex Sans for controls, and an annunciator color system
//! where every color has exactly one meaning —
//!   rust  #D96A2B  your hand: selection, primary actions, editing focus
//!   live  #7FBF3F  the device talking: link LED, live analog pressure
//!   amber #D9A44A  drift: unsaved session, custom actuation
//!   red   #D0574C  destructive, macro record
//! Nothing is colored for decoration; there are no glows except actual LEDs.

pub const APP_CSS: &str = r#"
:root {
  --chassis:  #131110;
  --panel-0:  #1A1815;
  --panel-1:  #211E1A;
  --panel-2:  #2A2620;
  --line:     #39332B;
  --line-soft:#2A2621;
  --ink:      #ECE8DF;
  --dim:      #A39C8D;
  --mute:     #6F695C;

  --rust:      #D96A2B;
  --rust-hi:   #E87F3E;
  --rust-deep: #8A3F17;
  --rust-wash: rgba(217,106,43,0.10);
  --live:      #7FBF3F;
  --live-dim:  #4C7A26;
  --live-wash: rgba(127,191,63,0.12);
  --amber:     #D9A44A;
  --amber-wash:rgba(217,164,74,0.10);
  --red:       #D0574C;
  --red-wash:  rgba(208,87,76,0.12);

  --radius:    6px;
  --radius-sm: 4px;
  --mono: 'TX-02','Berkeley Mono','JetBrains Mono',ui-monospace,'DejaVu Sans Mono',monospace;
  --sans: 'IBM Plex Sans','Fira Sans','DejaVu Sans',system-ui,sans-serif;
}

* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; height: 100%; }
body {
  background: var(--chassis);
  color: var(--ink);
  font-family: var(--sans);
  font-size: 13px;
  line-height: 1.5;
  -webkit-font-smoothing: antialiased;
  overflow: hidden;
}
#main, body > div { height: 100vh; }

::selection { background: var(--rust-wash); color: var(--ink); }

/* Instrument scrollbars: thin, machined, no chrome. */
::-webkit-scrollbar { width: 8px; height: 8px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: var(--panel-2); border-radius: 4px; }
::-webkit-scrollbar-thumb:hover { background: var(--line); }

/* Every interactive element gets the same visible focus ring (rust = your hand). */
button:focus-visible, select:focus-visible, input:focus-visible,
a:focus-visible, [tabindex]:focus-visible {
  outline: 2px solid var(--rust);
  outline-offset: 2px;
}

@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { transition: none !important; animation: none !important; }
}

/* ----- App shell: masthead / tab rail / content / status strip ------------- */
.app {
  display: flex;
  flex-direction: column;
  height: 100vh;
  background: var(--chassis);
}

/* Masthead: the instrument's nameplate row. Decorations are off, so this is
   also the titlebar: drag region + window controls (see Shell). */
.masthead {
  flex: none;
  display: flex; align-items: center; gap: 18px;
  height: 54px;
  padding: 0 14px 0 26px;
  border-bottom: 1px solid var(--line-soft);
  background: #171512;
  -webkit-user-select: none; user-select: none;
}
.brand { display: flex; align-items: center; gap: 11px; }
.brand-mark {
  width: 10px; height: 10px; flex: none;
  background: var(--rust);
  /* A machined square, not a logo blob. */
  border-radius: 2px;
}
.brand-name {
  font-family: var(--mono);
  font-size: 13.5px; font-weight: 700; letter-spacing: 0.08em;
  color: var(--ink);
}
.brand-name .accent { color: var(--rust); }
.brand-type {
  font-family: var(--mono);
  font-size: 8.5px; letter-spacing: 0.22em;
  color: var(--mute);
  margin-top: 1px;
}

.masthead-spacer { flex: 1; }

/* Link readout: an annunciator chip, not a pill. LED = device talking. */
.ann {
  display: inline-flex; align-items: center; gap: 8px;
  padding: 5px 11px;
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
  background: var(--panel-0);
  font-family: var(--mono); font-size: 10.5px; letter-spacing: 0.08em;
  color: var(--dim);
  white-space: nowrap;
}
.ann .strong { color: var(--ink); font-weight: 600; }
.ann.warn { border-color: rgba(217,164,74,0.4); color: var(--amber); background: var(--amber-wash); }

.led {
  width: 7px; height: 7px; border-radius: 50%; flex: none;
  background: var(--mute);
}
.led.live { background: var(--live); box-shadow: 0 0 6px rgba(127,191,63,0.55); }
.led.amber { background: var(--amber); box-shadow: 0 0 6px rgba(217,164,74,0.5); }
.led.red { background: var(--red); box-shadow: 0 0 6px rgba(208,87,76,0.5); }
.led.off { background: var(--mute); }

.profile-select {
  display: flex; align-items: center; gap: 9px;
  font-family: var(--mono); font-size: 10px; letter-spacing: 0.14em;
  text-transform: uppercase; color: var(--mute);
}
.profile-select select {
  appearance: none;
  background: var(--panel-1);
  color: var(--ink);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  padding: 6px 28px 6px 11px;
  font-family: var(--sans); font-size: 12.5px; font-weight: 500;
  letter-spacing: normal; text-transform: none;
  cursor: pointer;
  background-image:
    linear-gradient(45deg, transparent 50%, var(--dim) 50%),
    linear-gradient(135deg, var(--dim) 50%, transparent 50%);
  background-position: calc(100% - 15px) 50%, calc(100% - 10px) 50%;
  background-size: 5px 5px, 5px 5px;
  background-repeat: no-repeat;
}
.profile-select .save-profile { padding: 5px 12px; font-size: 11.5px; }

/* Window controls: machined CSD buttons standing in for the OS titlebar.
   Quiet until hovered; close earns the red wash (destructive). */
.winctl { display: flex; align-items: center; gap: 2px; margin-left: 2px; }
.wbtn {
  width: 32px; height: 28px;
  display: inline-flex; align-items: center; justify-content: center;
  background: transparent;
  border: none; border-radius: var(--radius-sm);
  color: var(--mute);
  font-family: var(--mono); font-size: 12px; line-height: 1;
  cursor: pointer;
  transition: background .12s, color .12s;
}
.wbtn:hover { background: var(--panel-2); color: var(--ink); }
.wbtn.close:hover { background: var(--red-wash); color: var(--red); }

/* Tab rail: the faceplate's section switches. */
.tab-rail {
  flex: none;
  display: flex; align-items: stretch; gap: 4px;
  height: 38px;
  padding: 0 16px;
  border-bottom: 1px solid var(--line-soft);
  background: var(--chassis);
}
.tab {
  position: relative;
  display: inline-flex; align-items: center;
  padding: 0 14px;
  font-family: var(--mono);
  font-size: 11px; font-weight: 500; letter-spacing: 0.12em;
  text-transform: uppercase;
  color: var(--mute);
  text-decoration: none;
  transition: color .12s;
}
.tab:hover { color: var(--dim); }
.tab.active { color: var(--ink); }
.tab.active::after {
  content: "";
  position: absolute; left: 10px; right: 10px; bottom: -1px;
  height: 2px;
  background: var(--rust);
}

/* Content area between the rails. */
.content {
  flex: 1;
  overflow-y: auto;
  padding: 24px 26px 30px;
}

/* Status strip: the persistent readout row. */
.statusbar {
  flex: none;
  display: flex; align-items: center; gap: 16px;
  height: 26px;
  padding: 0 26px;
  border-top: 1px solid var(--line-soft);
  background: #171512;
  font-family: var(--mono); font-size: 10px; letter-spacing: 0.08em;
  color: var(--mute);
  white-space: nowrap; overflow: hidden;
}
.statusbar .sb-item { display: inline-flex; align-items: center; gap: 7px; }
.statusbar .sb-spacer { flex: 1; }
.statusbar .sb-item.drift { color: var(--amber); }

/* ----- Page head ----------------------------------------------------------- */
/* The tab already names the screen; the head is a quiet context line, not a
   headline — faceplates don't have display type. */
.page-head {
  display: flex; align-items: baseline; gap: 14px;
  margin-bottom: 18px;
}
.page-title {
  font-family: var(--mono);
  font-size: 12px; font-weight: 700; letter-spacing: 0.16em;
  text-transform: uppercase;
  color: var(--ink);
  margin: 0;
}
.page-title::before {
  content: "";
  display: inline-block;
  width: 8px; height: 8px;
  margin-right: 9px;
  background: var(--rust);
  border-radius: 1px;
  vertical-align: baseline;
}
.page-sub { color: var(--dim); font-size: 12.5px; margin: 0; }

/* Two-column work area: stage + inspector panel. */
.work {
  display: grid;
  grid-template-columns: minmax(0, 1fr) clamp(290px, 27vw, 380px);
  gap: 22px;
  align-items: start;
}
@media (max-width: 1040px) {
  .work { grid-template-columns: minmax(0, 1fr); }
}

/* ----- Cards / panels ------------------------------------------------------ */
.card {
  background: var(--panel-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius);
  padding: 18px;
}
.card-title {
  font-family: var(--mono);
  font-size: 10px; letter-spacing: 0.16em; text-transform: uppercase;
  color: var(--mute);
  margin: 0 0 13px;
  display: flex; align-items: center; justify-content: space-between;
}
.card-title .val { color: var(--rust); letter-spacing: 0.04em; white-space: nowrap; }

/* Bind editor header: physical key chip → current function. */
.bind-head { display: flex; align-items: center; gap: 10px; margin: 0 0 16px; flex-wrap: wrap; }
.bind-head .bind-key {
  font-family: var(--mono); font-size: 15px; font-weight: 700; color: var(--ink);
  background: linear-gradient(180deg, var(--panel-2), var(--panel-1));
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  padding: 4px 12px; line-height: 1.3;
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.04), 0 2px 3px rgba(0,0,0,0.35);
}
.bind-head .bind-sep { color: var(--mute); font-size: 14px; }
.bind-head .bind-now { font-family: var(--mono); font-size: 13.5px; font-weight: 600; color: var(--rust); }

/* ----- Keypad stage: the bench --------------------------------------------- */
/* The stage is a query container: every keypad dimension below is expressed in
   `cqw` (% of the stage's inline width) so the WHOLE keypad — caps, gaps, thumb
   cluster, spacebar — scales together with the window. `--kp` is the master cap
   size; the thumb box and its absolutely-%-positioned children derive from it,
   preserving the device's cap:thumb proportions at any width. clamp() floors the
   scale so nothing collapses at ~900px and caps it so it doesn't bloat at ~1700px. */
.stage {
  container-type: inline-size;
  position: relative;
  /* Bench mat: a faint graph grid on the panel surface. */
  background:
    repeating-linear-gradient(0deg, rgba(236,232,223,0.022) 0 1px, transparent 1px 24px),
    repeating-linear-gradient(90deg, rgba(236,232,223,0.022) 0 1px, transparent 1px 24px),
    var(--panel-0);
  border: 1px solid var(--line-soft);
  border-radius: 8px;
  padding: 28px 26px 24px;
  /* Master scale tokens, all keyed to container width. */
  --kp:       clamp(42px, 13.5cqw, 66px);  /* one grid cap */
  --kp-gap:   clamp(7px, 2.2cqw, 11px);    /* inter-cap + inter-row gap */
  --kp-side:  clamp(20px, 6.5cqw, 34px);   /* grid <-> thumb-cluster gap */
  --kp-thumb: calc(var(--kp) * 2.71);      /* thumb box edge (168/62 ratio) */
  --kp-dir:   calc(var(--kp) * 0.74);      /* thumb direction cap (46/62) */
  --kp-r:     4px;                         /* cap corner radius: machined */
}
/* Registration marks: two L-shaped corner ticks, like a bench-mat print. */
.stage::before, .stage::after {
  content: "";
  position: absolute;
  width: 11px; height: 11px;
  border: 1px solid var(--line);
  pointer-events: none;
}
.stage::before { top: 9px; left: 9px; border-right: none; border-bottom: none; }
.stage::after  { bottom: 9px; right: 9px; border-left: none; border-top: none; }

/* Deck plate: one continuous plate behind both clusters — the physical device. */
.deck {
  position: relative;
  display: flex; flex-direction: column; align-items: center;
  width: fit-content;
  margin: 0 auto;
  padding: clamp(16px, 4cqw, 26px) clamp(18px, 4.5cqw, 30px) clamp(22px, 5cqw, 32px);
  background: linear-gradient(180deg, #232019, #1D1B16);
  border: 1px solid var(--line);
  border-radius: 12px;
  box-shadow:
    inset 0 1px 0 rgba(255,255,255,0.045),
    0 10px 26px rgba(0,0,0,0.42);
}
.deck-engrave {
  margin-top: clamp(10px, 2.6cqw, 16px);
  align-self: flex-end;
  font-family: var(--mono);
  font-size: 8px; letter-spacing: 0.26em;
  color: var(--mute);
  opacity: 0.75;
  user-select: none;
  pointer-events: none;
}

.keypad {
  display: flex;
  gap: var(--kp-side);
  align-items: flex-start;
  justify-content: center;
  max-width: 100%;
}
/* Calibration's analog-only board has no thumb cluster: drop the spacebar
   BELOW the grid (a column) instead of leaving a dead right-hand bay. */
.keypad.analog { flex-direction: column; align-items: center; }
.keypad-main { display: flex; flex-direction: column; gap: var(--kp-gap); }
/* Rows of 5/5/5/4: the short final row aligns LEFT so there's no phantom gap
   where a 5th cap would sit (no centering, no stagger). */
.kp-row { display: flex; gap: var(--kp-gap); justify-content: flex-start; }

.keycap {
  width: var(--kp); height: var(--kp);
  border-radius: var(--kp-r);
  background: linear-gradient(180deg, #2B2720, #1E1B16);
  border: 1px solid var(--line);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.05), 0 2px 3px rgba(0,0,0,0.45);
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
.keycap:hover { transform: translateY(-1px); border-color: #4A4336; }
.keycap.selected {
  border-color: var(--rust);
  box-shadow: 0 0 0 1px var(--rust), inset 0 1px 0 rgba(255,255,255,0.05);
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
  font-size: clamp(10px, 3.1cqw, 13px); font-weight: 600;
  color: var(--ink);
}
/* The cap's current-function label: readable, quiet (dim — persistent info, not
   selection), single-line and ellipsised so a long bind never overflows the cap. */
.keycap .cap-func,
.spacebar .cap-func {
  font-family: var(--mono);
  font-size: clamp(7.5px, 2.2cqw, 9.5px); font-weight: 500;
  color: var(--dim); letter-spacing: 0.02em;
  margin-top: 3px;
  max-width: calc(var(--kp) - 6px);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.keycap.selected .cap-func, .spacebar.selected .cap-func { color: var(--rust); }
.keycap.disabled { opacity: 0.4; }
.keycap.disabled .cap-legend { text-decoration: line-through; color: var(--mute); }
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
   lockstep with the caps. */
.keypad-side { display: flex; flex-direction: column; gap: calc(var(--kp-gap) * 1.6); align-items: center; }
.thumbpad {
  position: relative;
  width: var(--kp-thumb); height: var(--kp-thumb);
}
.thumb-dir {
  position: absolute;
  width: var(--kp-dir); height: var(--kp-dir);
  border-radius: var(--kp-r);
  background: linear-gradient(180deg, #2B2720, #1E1B16);
  border: 1px solid var(--line);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.05), 0 2px 3px rgba(0,0,0,0.45);
  display: flex; align-items: center; justify-content: center;
  font-family: var(--mono); font-size: clamp(9px, 2.6cqw, 11px); font-weight: 600;
  color: var(--dim);
  cursor: pointer;
  transition: border-color .12s, color .12s, transform .1s;
}
.thumb-dir:hover { color: var(--ink); border-color: #4A4336; transform: scale(1.03); }
.thumb-dir.selected {
  border-color: var(--rust); color: var(--ink);
  box-shadow: 0 0 0 1px var(--rust), inset 0 1px 0 rgba(255,255,255,0.05);
}
/* Static hub (Lighting/Pressure): a dashed circle dead-center of the box. */
.thumb-hub {
  position: absolute; left: 50%; top: 50%;
  transform: translate(-50%, -50%);
  width: var(--kp-dir); height: var(--kp-dir); border-radius: 50%;
  background: var(--panel-1);
  border: 1px dashed var(--line);
  display: flex; align-items: center; justify-content: center;
  font-family: var(--mono);
  font-size: clamp(7.5px, 2.1cqw, 9px); color: var(--mute); letter-spacing: 0.1em;
}
/* In Bind mode the hub is a single 4-way/8-way TOGGLE button. Width-capped to
   the central gap so it never overlaps the surrounding N/E/S/W caps. The `.four`
   state fills rust so the active mode reads at a glance. */
.thumb-hub.toggle {
  left: 50%; top: 50%;
  transform: translate(-50%, -50%);
  width: auto;
  max-width: calc(var(--kp-thumb) - 2 * var(--kp-dir) - 6px);
  height: auto; border-radius: var(--radius-sm);
  background: var(--panel-2); border: 1px solid var(--line);
  padding: 4px 8px; letter-spacing: 0.08em;
  font-family: var(--mono); font-size: clamp(6.5px, 1.7cqw, 8.5px); font-weight: 700;
  color: var(--dim); white-space: nowrap; cursor: pointer; overflow: hidden;
  transition: background .12s, color .12s, border-color .12s;
}
.thumb-hub.toggle:hover { color: var(--ink); border-color: #4A4336; }
.thumb-hub.toggle.four { background: var(--rust); color: #1C120A; border-color: var(--rust); }

.wheel {
  width: calc(var(--kp-thumb) * 0.54); height: calc(var(--kp) * 0.65); border-radius: 999px;
  background: linear-gradient(180deg, #2B2720, #1E1B16);
  border: 1px solid var(--line);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.05), 0 2px 3px rgba(0,0,0,0.45);
  display: flex; align-items: center; justify-content: space-around;
  font-family: var(--mono); font-size: clamp(10px, 2.8cqw, 12px); color: var(--dim);
}
.wheel .w-seg { cursor: pointer; padding: 4px 6px; border-radius: var(--radius-sm); }
.wheel .w-seg:hover { color: var(--ink); }
.wheel .w-seg.selected { color: var(--rust); }

/* The big thumb actuator (spacebar). `.thumb-button` shares the look so a
   separately-rendered thumb-button cap matches and carries the pressure fill. */
.spacebar, .thumb-button {
  width: var(--kp-thumb); height: var(--kp-dir); border-radius: var(--kp-r);
  background: linear-gradient(180deg, #2B2720, #1E1B16);
  border: 1px solid var(--line);
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.05), 0 2px 3px rgba(0,0,0,0.45);
  display: flex; align-items: center; justify-content: center;
  font-family: var(--mono); font-size: clamp(9px, 2.6cqw, 11px); letter-spacing: 0.16em;
  color: var(--dim); cursor: pointer;
  position: relative;
  /* No overflow:hidden — the spacebar hosts the Calibration popover. The
     pressure fill (`.pressed::after`) is bounded by its own border-radius. */
  transition: border-color .12s, color .12s;
}
.spacebar:hover, .thumb-button:hover { color: var(--ink); border-color: #4A4336; }
.spacebar.selected, .thumb-button.selected {
  border-color: var(--rust); color: var(--ink);
  box-shadow: 0 0 0 1px var(--rust), inset 0 1px 0 rgba(255,255,255,0.05);
}
/* The thumb button (ALT) is round — a distinct, thumb-sized disc, not a bar. */
.thumb-button {
  width: calc(var(--kp-dir) * 1.5); height: calc(var(--kp-dir) * 1.5);
  border-radius: 50%; letter-spacing: 0.1em;
}
/* The disc's label ellipsises so a bound value can't spill outside the circle. */
.thumb-button .tb-label {
  max-width: calc(var(--kp-dir) * 1.5 - 12px);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}

/* Spacebar in Bind mode stacks its legend over a readable current-function
   label (like the grid caps); the spacebar is bindable + analog, so it must
   reflect its bind, not read as inert. */
.spacebar { flex-direction: column; gap: 3px; }
.spacebar .cap-func { max-width: calc(var(--kp-thumb) - 16px); margin-top: 0; }
.spacebar.disabled, .thumb-button.disabled { opacity: 0.4; }
.spacebar.disabled .cap-legend,
.thumb-button.disabled .tb-label { text-decoration: line-through; color: var(--mute); }
.thumb-dir.disabled { opacity: 0.4; text-decoration: line-through; }

/* ----- Dual-layer keypad stack (Customize: Base + Hypershift) -------------- */
.keypad-stack { display: flex; flex-direction: column; gap: 26px; }
.keypad-layer { display: flex; flex-direction: column; gap: 12px; }
.keypad-layer + .keypad-layer { padding-top: 22px; border-top: 1px solid var(--line-soft); }
.layer-label {
  display: inline-flex; align-items: center; gap: 8px; align-self: center;
  font-family: var(--mono);
  font-size: 10px; letter-spacing: 0.16em; text-transform: uppercase; font-weight: 700;
  color: var(--mute);
}
.layer-label .dot {
  width: 7px; height: 7px; border-radius: 50%;
  background: var(--rust);
}
.layer-label.shift { color: var(--amber); }
.layer-label.shift .dot { background: var(--amber); }

/* ----- Inspector / bind editor --------------------------------------------- */
.inspector { display: flex; flex-direction: column; gap: 16px; }
.field { margin-bottom: 14px; }
.field:last-child { margin-bottom: 0; }
.field-label {
  display: block;
  font-family: var(--mono);
  font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase;
  color: var(--mute); margin-bottom: 7px;
}
.field-value { font-family: var(--mono); font-size: 13px; color: var(--ink); }

.select-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 6px; }
.chip {
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--panel-1);
  color: var(--dim);
  font-size: 12px; font-weight: 500;
  text-align: center;
  cursor: pointer;
  transition: border-color .12s, color .12s, background .12s;
}
.chip:hover { border-color: #4A4336; color: var(--ink); }
.chip.active {
  border-color: var(--rust);
  color: var(--ink);
  background: var(--rust-wash);
}

.text-input {
  width: 100%;
  background: var(--panel-1);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  color: var(--ink);
  padding: 9px 11px;
  font-family: var(--mono);
  font-size: 13px;
}
.text-input:focus { outline: none; border-color: var(--rust); }

.empty-inspector {
  display: flex; flex-direction: column; align-items: center; justify-content: center;
  height: 220px; text-align: center; color: var(--mute);
  border: 1px dashed var(--line); border-radius: var(--radius);
  gap: 8px;
  font-size: 12.5px;
}
.empty-inspector .big { font-family: var(--mono); font-size: 22px; opacity: 0.5; }

/* ----- Sliders: instrument faders ------------------------------------------ */
.slider-row { display: flex; align-items: center; gap: 14px; }
.slider {
  -webkit-appearance: none; appearance: none;
  flex: 1; height: 4px; border-radius: 2px;
  background: var(--panel-2);
  outline: none;
}
.slider::-webkit-slider-thumb {
  -webkit-appearance: none; appearance: none;
  width: 9px; height: 18px; border-radius: 2px;
  background: var(--rust);
  cursor: pointer;
  border: 1px solid #1C120A;
  box-shadow: 0 1px 3px rgba(0,0,0,0.5);
}
.slider.amber::-webkit-slider-thumb { background: var(--amber); }
.slider-val { font-family: var(--mono); font-size: 12.5px; min-width: 64px; text-align: right; color: var(--ink); }

/* ----- Toggle switch -------------------------------------------------------- */
.switch {
  width: 36px; height: 20px; border-radius: 999px;
  background: var(--panel-2); border: 1px solid var(--line);
  position: relative; cursor: pointer; flex: none;
  transition: background .14s, border-color .14s;
}
.switch.on { background: var(--rust-deep); border-color: var(--rust); }
.switch::after {
  content: ""; position: absolute; top: 2px; left: 2px;
  width: 14px; height: 14px; border-radius: 50%;
  background: var(--dim); transition: transform .14s, background .14s;
}
.switch.on::after { transform: translateX(16px); background: var(--ink); }
.switch-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; }

/* ----- Color picker --------------------------------------------------------- */
.swatch-grid { display: grid; grid-template-columns: repeat(6, 1fr); gap: 7px; }
.swatch {
  aspect-ratio: 1; border-radius: var(--radius-sm);
  border: 1px solid var(--line); cursor: pointer;
  transition: transform .1s;
}
.swatch:hover { transform: scale(1.06); }
.swatch.active { box-shadow: 0 0 0 2px var(--chassis), 0 0 0 4px var(--rust); }
.color-readout {
  display: flex; align-items: center; gap: 12px; margin-top: 14px;
}
.color-preview { width: 42px; height: 42px; border-radius: var(--radius-sm); border: 1px solid var(--line); }
.color-hex { font-family: var(--mono); font-size: 14px; }

/* ----- Banner: an annunciator bar, not a toast ----------------------------- */
.banner {
  display: flex; align-items: center; gap: 12px;
  padding: 11px 14px;
  background: var(--amber-wash);
  border: 1px solid rgba(217,164,74,0.35);
  border-left: 3px solid var(--amber);
  border-radius: var(--radius-sm);
  color: var(--amber);
  font-size: 12.5px;
  margin-bottom: 18px;
}
.banner .b-ico { font-size: 15px; }
.banner b { color: #EFCB8B; }

/* ----- Lists (macros / profiles) ------------------------------------------- */
.list { display: flex; flex-direction: column; gap: 6px; }
.list-item {
  display: flex; align-items: center; gap: 12px;
  padding: 12px 13px;
  background: var(--panel-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
  cursor: pointer;
  transition: border-color .12s, background .12s;
}
.list-item:hover { border-color: var(--line); background: var(--panel-1); }
.list-item.active { border-color: var(--rust); background: var(--rust-wash); }
.list-item .li-main { flex: 1; min-width: 0; }
.list-item .li-name { font-weight: 600; font-size: 13px; }
.li-meta { font-size: 11px; color: var(--mute); font-family: var(--mono); margin-top: 2px; }
.list-item .li-badge {
  font-family: var(--mono);
  font-size: 9.5px; padding: 3px 7px; border-radius: 3px;
  background: var(--panel-2); color: var(--dim); letter-spacing: 0.08em;
}
/* ACTIVE = what the daemon is running right now — device state, so live green. */
.list-item .li-badge.live { background: var(--live-wash); color: var(--live); }

/* ----- Buttons -------------------------------------------------------------- */
.btn {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 8px 14px; border-radius: var(--radius-sm);
  border: 1px solid var(--line); background: var(--panel-2);
  color: var(--ink); font-family: var(--sans); font-size: 12.5px; font-weight: 500;
  cursor: pointer; transition: border-color .12s, background .12s;
}
.btn:hover { border-color: #4A4336; background: #322D26; }
.btn.primary { background: var(--rust); color: #1C120A; border-color: var(--rust); font-weight: 600; }
.btn.primary:hover { background: var(--rust-hi); }
.btn.danger { color: var(--red); }
.btn.danger:hover { border-color: var(--red); background: var(--red-wash); }
.btn-row { display: flex; gap: 8px; flex-wrap: wrap; }

.divider { height: 1px; background: var(--line-soft); margin: 16px 0; }

/* ----- Analog calibration (live pressure) ---------------------------------- */
/* The live pressure track: phosphor fill (device talking) with threshold marks
   in the colors of who set them — actuation = rust (your setting), second step
   = amber (drift from single-action), release = mute. */
.press-bar {
  position: relative;
  height: 22px; border-radius: var(--radius-sm);
  background: var(--panel-1);
  border: 1px solid var(--line);
  overflow: hidden;
}
.press-fill {
  position: absolute; left: 0; top: 0; bottom: 0;
  background: linear-gradient(90deg, var(--live-dim), var(--live));
  transition: width .08s linear;
}
.press-mark {
  position: absolute; top: -1px; bottom: -1px;
  width: 2px; transform: translateX(-1px);
  z-index: 1;
}
.press-mark.actuate { background: var(--rust); }
.press-mark.release { background: var(--mute); }
.press-mark.second  { background: var(--amber); }
.press-readout {
  font-family: var(--mono); font-size: 11px; color: var(--mute);
  margin-top: 6px; text-align: right;
}

/* ========================================================================== */
/* Overlay primitives (modal / menu / pickers / chords / steps / etc.)         */
/* ========================================================================== */

/* ----- Backdrop ------------------------------------------------------------- */
.backdrop {
  position: fixed; inset: 0; z-index: 40;
  background: rgba(10,9,7,0.6);
  display: flex; align-items: center; justify-content: center;
}

/* ----- Kebab menu ----------------------------------------------------------- */
.menu {
  position: absolute; z-index: 30;
  min-width: 168px;
  background: var(--panel-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 12px 32px rgba(0,0,0,0.55);
  padding: 5px;
  display: flex; flex-direction: column; gap: 1px;
}
.menu-item {
  display: flex; align-items: center; gap: 10px;
  padding: 8px 10px;
  border-radius: var(--radius-sm);
  color: var(--dim);
  font-size: 12.5px; font-weight: 500;
  cursor: pointer; text-align: left;
  border: none; background: transparent; width: 100%;
  transition: background .12s, color .12s;
}
.menu-item:hover { background: var(--panel-2); color: var(--ink); }
.menu-item.danger { color: var(--red); }
.menu-item.danger:hover { background: var(--red-wash); color: var(--red); }
.menu-item.disabled { opacity: 0.45; cursor: default; }
.menu-item.disabled:hover { background: transparent; color: var(--dim); }
.menu-sep { height: 1px; background: var(--line-soft); margin: 4px 6px; }
.kebab {
  display: inline-flex; align-items: center; justify-content: center;
  width: 30px; height: 30px; flex: none;
  border: none; background: transparent;
  color: var(--mute); border-radius: var(--radius-sm);
  font-size: 16px; line-height: 1; cursor: pointer;
  transition: background .12s, color .12s;
}
.kebab:hover { background: var(--panel-2); color: var(--ink); }

/* ----- Key picker ----------------------------------------------------------- */
.key-picker { position: relative; }
.kp-trigger {
  display: inline-flex; align-items: center; justify-content: space-between; gap: 8px;
  width: 100%;
  padding: 9px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--panel-1);
  color: var(--ink);
  font-family: var(--mono); font-size: 12.5px;
  cursor: pointer; text-align: left;
  transition: border-color .12s, background .12s;
}
.kp-trigger:hover { border-color: #4A4336; background: var(--panel-2); }
.kp-panel {
  /* Above the picker's inline backdrop (fixed, z-index:50) so the panel paints
     over it and stays clickable instead of being covered by the catcher. */
  position: absolute; z-index: 60; top: calc(100% + 6px); left: 0;
  width: 280px; max-height: 340px;
  display: flex; flex-direction: column;
  background: var(--panel-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 14px 36px rgba(0,0,0,0.6);
  overflow: hidden;
}
.kp-search {
  flex: none;
  margin: 8px; padding: 8px 10px;
  background: var(--panel-2);
  border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  color: var(--ink);
  font-family: var(--sans); font-size: 12.5px;
}
.kp-search:focus { outline: none; border-color: var(--rust); }
.kp-list { flex: 1; overflow-y: auto; padding: 0 4px 6px; }
.kp-group { padding-top: 2px; }
.kp-group-head {
  position: sticky; top: 0; z-index: 1;
  padding: 7px 9px 5px;
  background: var(--panel-1);
  font-family: var(--mono);
  font-size: 9.5px; letter-spacing: 0.14em; text-transform: uppercase;
  color: var(--mute);
}
.kp-entry {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  padding: 7px 9px;
  border-radius: var(--radius-sm);
  font-size: 12.5px; color: var(--dim);
  cursor: pointer;
}
.kp-entry:hover { background: var(--panel-2); color: var(--ink); }
.kp-raw { font-family: var(--mono); font-size: 10.5px; color: var(--mute); flex: none; }
.kp-entry.highlighted {
  background: var(--rust-wash);
  color: var(--ink);
  box-shadow: inset 0 0 0 1px var(--rust-deep);
}
.kp-empty {
  padding: 22px 12px; text-align: center;
  font-size: 12px; color: var(--mute);
}

/* ----- Customize: chord editor + segmented control -------------------------- */
.chord-pills { display: flex; flex-wrap: wrap; align-items: center; gap: 7px; }
.pill {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 6px 8px 6px 11px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: linear-gradient(180deg, var(--panel-2), var(--panel-1));
  box-shadow: inset 0 1px 0 rgba(255,255,255,0.04);
  color: var(--ink);
  font-family: var(--mono); font-size: 12px; font-weight: 600;
}
.pill-x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 16px; height: 16px; flex: none;
  border-radius: 3px;
  color: var(--mute); cursor: pointer;
  font-size: 13px; line-height: 1;
  transition: background .12s, color .12s;
}
.pill-x:hover { background: var(--red-wash); color: var(--red); }
.pill-add {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 6px 11px;
  border-radius: var(--radius-sm);
  border: 1px dashed var(--line);
  background: transparent;
  color: var(--dim);
  font-family: var(--sans); font-size: 12px; font-weight: 600;
  cursor: pointer;
  transition: border-color .12s, color .12s;
}
.pill-add:hover { border-color: var(--rust); color: var(--ink); }

/* Reusable segmented control (Customize layers, Lighting, thumb toggle). */
.seg {
  display: inline-flex; padding: 3px;
  background: var(--panel-1); border: 1px solid var(--line);
  border-radius: var(--radius-sm);
  gap: 2px;
}
.seg button {
  border: none; background: transparent; color: var(--dim);
  padding: 5px 13px; border-radius: 3px; cursor: pointer;
  font-family: var(--mono); font-size: 11px; font-weight: 600;
  letter-spacing: 0.06em; text-transform: uppercase;
  transition: background .12s, color .12s;
}
.seg button:hover { color: var(--ink); }
.seg button.active { background: var(--rust); color: #1C120A; }

/* ----- Macros page ----------------------------------------------------------- */
.work.macros { grid-template-columns: 230px 1fr; }
.macro-toolbar {
  display: flex; align-items: center; gap: 10px;
  padding: 9px 12px;
  background: var(--panel-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
}
/* Step chips: the ONE place numbering is earned — steps are a real sequence. */
.step-flow { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
.step-chip {
  display: inline-flex; align-items: center; gap: 7px;
  padding: 6px 10px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--panel-1);
  color: var(--ink);
  font-family: var(--mono); font-size: 12px;
  cursor: pointer;
  transition: border-color .12s, box-shadow .12s;
}
.step-chip .num {
  display: inline-flex; align-items: center; justify-content: center;
  min-width: 16px; height: 16px;
  border-radius: 3px;
  background: var(--panel-2);
  color: var(--mute);
  font-size: 10px; font-family: var(--mono); font-weight: 700;
}
.step-chip.press   { border-color: var(--line); background: var(--panel-2); }
.step-chip.press .num { background: var(--rust-wash); color: var(--rust); }
.step-chip.release { background: var(--panel-1); color: var(--dim); }
.step-chip.delay {
  border-color: rgba(217,164,74,0.4);
  background: var(--amber-wash);
  color: var(--amber);
  padding: 5px 9px; font-size: 11.5px;
}
.step-chip.editing { border-color: var(--rust); box-shadow: 0 0 0 1px var(--rust); }
.chip-x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 15px; height: 15px; flex: none;
  border-radius: 3px;
  color: var(--mute); cursor: pointer;
  font-size: 12px; line-height: 1;
  transition: background .12s, color .12s;
}
.chip-x:hover { background: var(--red-wash); color: var(--red); }

/* ----- Calibration: per-key pressure fill + floating modal ------------------ */
/* Bottom-rising phosphor overlay, behind the legend (cf. .keycap.lit). The grid
   caps and the spacebar — the only analog actuators (the thumbpad, wheel, and
   thumb button are digital, with no pressure byte) — share the same fill so
   analog pressure reads identically. `--press` is the 0..100% height. */
.keycap.pressed::after,
.spacebar.pressed::after {
  content: "";
  position: absolute; left: 0; right: 0; bottom: 0;
  height: var(--press, 0%);
  /* Match the cap's corner radius so the bottom-rising fill follows the rounded
     rect instead of poking past it — the caps no longer overflow:hidden (that
     would clip the Calibration popover). At low fill the rounded top corners sit
     on a low-opacity sliver and read flush. */
  border-radius: var(--kp-r);
  background: linear-gradient(0deg, var(--live), var(--live-dim));
  opacity: 0.3;
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
  background: var(--panel-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 14px 36px rgba(0,0,0,0.6);
  padding: 14px;
}
.cal-modal.open-left { left: auto; right: 105%; }
.cal-modal-head {
  display: flex; align-items: center; justify-content: space-between; gap: 10px;
  margin-bottom: 12px;
  font-family: var(--mono); font-size: 12.5px; font-weight: 600;
}

/* Caps whose actuation deviates from the defaults carry a small amber marker
   (drift) so customized keys read at a glance on the Calibration board. */
.cal-dot {
  position: absolute; top: 5px; right: 5px;
  width: 5px; height: 5px; border-radius: 50%;
  background: var(--amber);
  pointer-events: none;
  z-index: 1;
}

/* Calibration page wrapper: while the Edit Mode drawer is open, shift the whole
   page (toolbar included) left of it — the fixed drawer must never cover the
   "Exit Edit Mode" button, the mode's only exit. Matches the drawer's slide. */
.cal-page { transition: margin-right .22s cubic-bezier(.2, .8, .3, 1); }
.cal-page.panel-open { margin-right: 332px; }

/* Page-level calibration actions ("Reset all" + the Edit Mode toggle), a thin
   right-aligned row between the banner area and the stage. */
.cal-toolbar {
  display: flex; justify-content: flex-end; align-items: center; gap: 10px;
  margin-bottom: 12px;
}
/* The Edit Mode toggle while ON: a rust ring, same accent as the selected caps,
   so the mode reads as "armed" at a glance. */
.cal-toolbar .edit-toggle.on { border-color: var(--rust); color: var(--rust); }

/* Calibration multi-select: caps in the selection set get a rust ring (drag
   or Shift+click). Distinct from the single `.selected` ring. */
.keycap.multi, .spacebar.multi {
  border-color: var(--rust);
  box-shadow: inset 0 0 0 1px var(--rust);
  background: linear-gradient(180deg, #322A20, #241E16);
}

/* Edit Mode drawer — the bulk actuation editor, docked to the right edge and
   slid in on mount. Deliberately NO backdrop: the caps must stay clickable so
   the selection can be reshaped while the panel is open. Below the 54px
   masthead; scrolls internally if the viewport is short. */
.cal-panel {
  position: fixed; z-index: 80;
  top: 70px; right: 16px; bottom: 16px;
  width: 300px; max-width: calc(100vw - 48px);
  overflow-y: auto;
  background: var(--panel-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 20px 50px rgba(0,0,0,0.65);
  padding: 18px;
  animation: cal-panel-in .22s cubic-bezier(.2, .8, .3, 1);
}
@keyframes cal-panel-in {
  from { transform: translateX(28px); opacity: 0; }
  to   { transform: translateX(0);    opacity: 1; }
}

/* ----- Profiles page --------------------------------------------------------- */
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
  padding: 11px 13px;
  background: var(--panel-0);
  border: 1px solid var(--line-soft);
  border-radius: var(--radius-sm);
  transition: border-color .12s, background .12s;
}
.profile-row:hover { border-color: var(--line); background: var(--panel-1); }
/* .is-default before .active (equal specificity) so a row that is BOTH active
   and default shows the ACTIVE treatment, not the default surface. */
.profile-row.is-default { border-color: var(--line); background: var(--panel-1); }
.profile-row.active { border-color: var(--rust); background: var(--rust-wash); }
/* .li-badge is descendant-scoped to .list-item upstream; re-base it for rows. */
.profile-row .li-badge {
  font-family: var(--mono);
  font-size: 9.5px; padding: 3px 7px; border-radius: 3px;
  background: var(--panel-2); color: var(--dim); letter-spacing: 0.08em;
}
.profile-row .li-badge.live { background: var(--live-wash); color: var(--live); }
.li-badge.default { background: var(--panel-2); color: var(--dim); }
.li-name-input {
  width: 100%;
  background: transparent;
  border: 1px solid transparent;
  border-radius: var(--radius-sm);
  color: var(--ink);
  padding: 6px 8px;
  font-family: var(--sans); font-size: 13.5px; font-weight: 600;
  transition: border-color .12s, background .12s;
}
.li-name-input:hover { border-color: var(--line-soft); }
.li-name-input:focus { outline: none; border-color: var(--rust); background: var(--panel-1); }
.link-chip {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 4px 9px;
  border-radius: var(--radius-sm);
  border: 1px solid var(--line);
  background: var(--panel-1);
  color: var(--dim);
  font-family: var(--mono); font-size: 11px;
}
.link-chip .glyph { font-size: 11px; opacity: 0.85; }
.link-x {
  display: inline-flex; align-items: center; justify-content: center;
  width: 14px; height: 14px; flex: none;
  border-radius: 3px;
  color: var(--mute); cursor: pointer;
  font-size: 11px; line-height: 1;
  transition: background .12s, color .12s;
}
.link-x:hover { background: var(--red-wash); color: var(--red); }
.link-chip.disabled { opacity: 0.45; }
.link-add {
  display: inline-flex; align-items: center; gap: 5px;
  padding: 4px 10px;
  border-radius: var(--radius-sm);
  border: 1px dashed var(--line);
  background: transparent;
  color: var(--dim);
  font-size: 11px; font-weight: 600;
  cursor: pointer;
  transition: border-color .12s, color .12s;
}
.link-add:hover { border-color: var(--rust); color: var(--ink); }
.link-add.disabled { opacity: 0.45; cursor: default; }
.link-add.disabled:hover { border-color: var(--line); color: var(--dim); }

/* Dialog overlay (profile export/import): a centered machined card over a
   dimmed chassis. z-index above the row menus (30) and the lifted caps (70). */
.overlay {
  position: fixed; inset: 0; z-index: 90;
  background: rgba(10, 9, 7, 0.62);
  display: flex; align-items: center; justify-content: center;
}
.dialog {
  width: min(480px, calc(100vw - 48px));
  background: var(--panel-1);
  border: 1px solid var(--line);
  border-radius: var(--radius);
  box-shadow: 0 18px 48px rgba(0,0,0,0.6);
  padding: 18px 20px;
  display: flex; flex-direction: column; gap: 12px;
}
.dialog-title {
  font-family: var(--mono); font-size: 12px; letter-spacing: 0.14em;
  text-transform: uppercase; color: var(--ink);
}
.dialog-sub { font-size: 12px; color: var(--dim); line-height: 1.5; }
.dialog-error { font-size: 12px; color: var(--red); line-height: 1.5; }
.dialog-actions {
  display: flex; justify-content: flex-end; gap: 8px; margin-top: 2px;
}
"#;
