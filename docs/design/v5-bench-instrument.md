# UI v5 — "Bench Instrument"

The design brief in one line: **the faceplate of a precision test instrument
for a device you reverse-engineered**, not a gaming-software skin.

tartarust exists because one person put a Tartarus Pro on the bench,
captured its USB traffic, and wrote the driver Razer wouldn't. The UI should
look like the tool that story deserves: U.S. Graphics / IBM industrial
documentation, Elektron-grade restraint, annunciator lights instead of glow.

v4 was the generic dark-tool template (near-black, one acid-green accent,
SaaS sidebar, Inter). Every axis of that template is deliberately replaced.

## Palette — warm iron + four signals

Ground is warm (brown-grey iron, not blue-black). Signal colors are an
annunciator convention: each color has exactly ONE meaning, and nothing is
colored for decoration.

| token        | hex                   | job |
|--------------|-----------------------|-----|
| `--chassis`  | `#131110`             | page ground |
| `--panel-0/1/2` | `#1A1815` `#211E1A` `#2A2620` | stepped surfaces |
| `--line` / `--line-soft` | `#39332B` / `#2A2621` | hairlines |
| `--ink` / `--dim` / `--mute` | `#ECE8DF` / `#A39C8D` / `#6F695C` | text |
| `--rust`     | `#D96A2B`             | **your hand** — selection, primary action, editing focus |
| `--live`     | `#7FBF3F`             | **the device talking** — link LED, live analog pressure (phosphor) |
| `--amber`    | `#D9A44A`             | **drift** — unsaved session, custom actuation markers |
| `--red`      | `#D0574C`             | destructive, macro record |

Rust-orange is the brand: the project is tarta*rust*, written in Rust.
Green is never "selected" again — v4 overloaded it (selected AND connected AND
pressure); v5 splits those meanings across rust/live/amber.

## Type — TX-02 carries the identity

- **Display + data**: `TX-02` (fallback Berkeley Mono) — tab labels, panel
  titles (10px, +0.14em tracking, uppercase), key legends, every number,
  hex, evdev name. The mono IS the personality; the user owns this font.
- **Controls + prose**: `IBM Plex Sans` — buttons, chips, descriptions,
  hints. Keeps the instrument voice from tipping into terminal kitsch.
- Scale: 13px Plex base; micro-labels 10px; page context line 12.5px;
  no display-size headlines anywhere — faceplates don't have them.

## Layout — faceplate, not admin panel

Sidebar deleted. Vertical stack, edge-to-edge hairlines:

```
┌──────────────────────────────────────────────────────────────┐
│ TARTARUST   TYPE RT-1 │ ● LINK 1532:0244 │ PROFILE ▾ SAVE│  masthead 54px
├──────────────────────────────────────────────────────────────┤
│ CUSTOMIZE   LIGHTING   CALIBRATION   MACROS   PROFILES       │  tab rail 38px
├──────────────────────────────────────────────────────────────┤
│                                                              │
│   stage (device on bench)              inspector panel       │  content
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ ● DAEMON LIVE · profile FPS — Default · v0.0.0    UNSAVED ●  │  status strip 24px
└──────────────────────────────────────────────────────────────┘
```

Content gains the sidebar's 232px — the stage (which scales with container
width) gets visibly bigger. Active tab: rust underline bar + ink text.
Status strip is the instrument's persistent readout row (mono, 10.5px).

## Signature — the device on the bench

The keypad stage is the one place boldness is spent:

- Stage surface = bench mat: faint 24px graph grid + corner registration
  ticks, edge-lit top.
- The keypad sits on a **deck plate** (one continuous rounded plate behind
  both clusters, like the physical device) engraved `TARTARUS PRO` /
  `USB 1532:0244` in mute mono.
- Caps are sculpted (top-light gradient, hard 4px radius, deep base shadow),
  legends in TX-02. Selected cap = rust ring, NOT green glow.
- Live pressure = phosphor-green fill rising in the cap — scope trace, with
  the raw 0..255 readout in mono.

## Discipline

- Radii: 4px caps/controls, 6px panels. Nothing pill-shaped except LEDs.
- Numbering appears ONLY where sequence is real (macro steps).
- Motion: functional only (pressure fill 80ms linear, tab underline slide);
  `prefers-reduced-motion` kills all transitions.
- Focus: 2px rust outline, 2px offset, on every interactive element.
- Copy: labels engrave (uppercase mono), prose speaks (sentence-case Plex,
  plain verbs, no apologies). Offline is a state readout ("NO LINK — start
  tartarust-daemon"), not a mood.

## Migration rule

Every v4 class name stays alive in the new stylesheet (screens keep
compiling/rendering mid-migration); meaning changes, names don't. New shell
classes: `.masthead`, `.tab-rail`, `.tab`, `.statusbar`, `.ann` (annunciator
chip), `.led`, `.deck`, `.deck-engrave`.
