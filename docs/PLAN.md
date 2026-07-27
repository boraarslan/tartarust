# tartarust — master plan

A from-scratch Linux driver + control app for the **Razer Tartarus Pro** keypad
(`USB 1532:0244`), written in Rust, with a Dioxus desktop UI styled after Razer
Synapse (original design, feature-parity — not a 1:1 clone).

> Status doc. The research synthesis lives in `docs/research/00-overview.md`;
> this file is the authoritative engineering plan and is reconciled with it.

## Product decisions (from the user)

- **First usable milestone = Key remapping + Hypershift.** The remap pipeline is
  OS-side (evdev grab → uinput emit) and needs *no* Razer HID protocol, so it
  runs fully in parallel with the lighting/protocol work.
- **v1 must-haves:** Lighting/Chroma · Remap+Hypershift+Macros · Profiles+per-app
  · **Analog actuation** (the Pro's headline feature — the gate is resolved: the
  pressure protocol is reverse-engineered and the byte→key map is a
  device-family constant, built into the daemon).

## Hard constraints

- **The UI must never spawn a window on the user's physical displays.** During
  development it is run **only** inside a headless `cage` compositor on a private
  `WAYLAND_DISPLAY`, with screenshots captured via `grim`/wlr-screencopy.
  **Never** spawn headless Hyprland monitors.
- Device protocol modeled with **`deku`**. UI built with **`dioxus`** (0.7.x).
- Cargo **workspace**, core cleanly split from UI.

## Device facts (verified on this machine)

- `1532:0244` Tartarus Pro, 3 HID interfaces:
  - iface 0 → `hidraw8` (keyboard, proto 01)
  - iface 1 → `hidraw9` (vendor/control, proto 01) — *candidate control channel,
    confirm against OpenRazer source for 0x0244*
  - iface 2 → `hidraw11` (mouse/thumbpad, proto 02)
- hidraw nodes are `root`-owned; the login user is in `plugdev` → ship a udev rule
  (`docs/research/06-remap-and-udev.md`) granting `plugdev`/`uaccess` to the
  control hidraw node, the device's `/dev/input/event*` nodes, and `/dev/uinput`.

## Workspace layout

```
crates/
  tartarust-protocol  # deku 90-byte report + typed command set (no I/O)
  tartarust-hid       # hidraw transport: SET/GET feature reports + retry
  tartarust-ipc       # FROZEN client<->daemon schema (serde/JSON) — the seam
  tartarust-daemon    # owns device; evdev->uinput remap; IPC server
  tartarust-cli       # scriptable client + diagnostics
  tartarust-ui        # Dioxus desktop (thin IPC client)
```

## Milestones & concurrency

`tartarust-ipc` is the contract that unlocks parallelism: once it is frozen,
daemon, UI, and CLI teams build against it independently.

- **M0 — Skeleton & contracts** *(in progress)*
  Workspace compiles; `tartarust-ipc` schema scaffolded; deku `RazerReport` type
  + XOR checksum landed; headless-cage render chain proven with a hello-world.
  *Gate for everything below.*

- **M1 — Remap pipeline** *(user's first deliverable; parallel with M2)*
  Discover + grab the Tartarus input nodes; virtual uinput device; binding engine
  (per-key actions: key/mouse/macro/profile-switch/launch/disable); **Hypershift**
  hold-layer; **macro** record/playback; config persistence; CLI to drive it.
  Needs: a device-keymap discovery pass (what each physical key emits by default).

- **M2 — Device control / lighting** *(parallel with M1)*
  `tartarust-hid` transport; first **safe read** (firmware/serial) to validate
  report+CRC+transaction_id+interface; brightness; Chroma effects (static/
  breathing/spectrum/wave/reactive/starlight) + custom per-key matrix frame.

- **M3 — UI** *(parallel once M0 + IPC seam land; integrates M1/M2 as they ship)*
  Dioxus desktop shell + dark theme + nav; Customize (bind grid + Hypershift),
  Lighting, Macros, Profiles, Calibration screens; wired to the daemon over IPC.
  Every run goes through the headless-cage harness.

- **M4 — Profiles & per-app**
  Multiple profiles, focused-application auto-switch, onboard-memory slots.

- **M5 — Analog actuation** *(shipped)*
  DRIVER-mode pressure reads, per-key actuation points + dual-step, calibration
  UI. The byte→key map is a device-family constant (hardware-confirmed,
  identical across units) and is built into the daemon — no per-unit discovery.

### Parallel workstreams after M0
1. Daemon/remap (M1) · 2. Protocol+HID/lighting (M2) · 3. UI shell+screens (M3)
· 4. CLI · 5. Analog RE investigation (M5, async).

## Open questions / risks
- ~~Analog-actuation protocol availability~~ — resolved: protocol
  reverse-engineered, byte→key map confirmed constant across units.
- webkit2gtk rendering correctly under nested headless Wayland (render chain).
- Exact control interface + `transaction_id` for `0x0244` (confirm from source).
- Grabbing input nodes vs. the device's onboard default keymap interactions.
