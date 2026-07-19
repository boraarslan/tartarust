# rusty-tartarus

A from-scratch Linux driver and control app for the **Razer Tartarus Pro** keypad
(`USB 1532:0244`), written in Rust, with a [Dioxus](https://dioxuslabs.com)
desktop UI styled after Razer Synapse (an original design — not a clone).

No kernel module, no OpenRazer. A userspace daemon owns the device over `hidraw`
feature reports + `evdev`/`uinput`, and the UI/CLI drive it over a local IPC
socket.

> Status: early development. See **Status** below for what is built and verified
> vs. in progress. Reverse-engineered from the OpenRazer protocol + on-device
> validation; see [`docs/research/`](docs/research/).

## Features (target = Synapse parity)

- **Remapping + Hypershift** — grab the keypad and remap every input to keys,
  mouse buttons, macros, profile switches, program launches, etc., with a
  hold-to-shift secondary layer.
- **Chroma lighting** — brightness + effects (static / breathing / spectrum /
  wave / reactive / starlight) and per-key custom frames.
- **Profiles** — multiple software profiles with optional per-application
  auto-switching (the Pro has no onboard profile memory).
- **Macros** — recorded key/button/delay sequences.
- **Analog actuation** — adjustable actuation points / dual-step *(gated: the
  config protocol is undocumented; see [`docs/research/03-analog-actuation.md`](docs/research/03-analog-actuation.md))*.

## Architecture

A Cargo workspace; core is cleanly split from UI.

| Crate | Role |
|---|---|
| `tartarus-protocol` | The 90-byte Razer HID report (`deku`) + typed command set (device info, lighting). No I/O. `#![forbid(unsafe_code)]`. |
| `tartarus-hid` | `hidraw` transport — `SET`/`GET` feature reports with the busy/EPIPE retry handshake. |
| `tartarus-ipc` | The frozen daemon↔client wire schema (serde / newline-delimited JSON) + shared domain types. |
| `tartarus-daemon` | The background service: **sole device owner**. Runs the `evdev`→`uinput` remap engine and serves the IPC socket. |
| `tartarus-cli` | `tartarus` — scriptable client + diagnostics. |
| `tartarus-ui` | The Dioxus desktop app — a thin IPC client. |

Exactly one process (the daemon) holds the device; the UI and CLI never touch it
directly. The daemon exposes its state over a Unix socket at
`$XDG_RUNTIME_DIR/rusty-tartarus.sock`.

## Build

```sh
cargo build --release        # all three binaries (the UI needs webkit2gtk-4.1 + gtk3)
cargo test --workspace       # unit + integration tests
cargo install --path .       # installs tartarus-daemon, tartarus, tartarus-ui
```

The bin targets live in the root `rusty-tartarus` package (thin shims over the
member crates), so a single `cargo install` ships all three binaries.

System deps (Arch): `webkit2gtk-4.1 gtk3 libusb hidapi` plus `cage` + `grim` for
headless UI dev.

## Install

```sh
cargo install --path .       # installs tartarus-daemon, tartarus, tartarus-ui
```

Upgrading from the old per-crate installs (`cargo install --path
crates/tartarus-cli` etc.)? Cargo refuses to overwrite binaries owned by a
different package — run it once with `--force` to migrate them to the root
`rusty-tartarus` package.

Before first use, set up device permissions (udev rule / `input` group) — see
**Device access** below; the daemon cannot grab the keypad without them.

The installed `tartarus-ui` integrates itself with the desktop on launch:

- **Desktop entry + icon** — the first launch writes
  `~/.local/share/applications/tartarus-ui.desktop` (named after the binary so
  window managers match the window to the entry) and a scalable icon
  (refreshed automatically if the binary moves). To (re)install explicitly
  without opening a window: `tartarus-ui install-desktop`.
- **Daemon autostart** — if no daemon answers on the control socket, the UI
  spawns `tartarus-daemon run` detached and connects once it is up. The
  daemon's output goes to `~/.local/state/rusty-tartarus/daemon.log`. If the
  daemon is missing or fails to start, the UI stays in its offline mode.

Opt-outs (set to `1`): `TARTARUS_UI_NO_DESKTOP_INSTALL` skips the entry/icon
install, `TARTARUS_UI_NO_AUTOSTART` skips the daemon probe/spawn (the headless
screenshot harness sets both).

## Device access

The control `hidraw` node, the keypad's `/dev/input/event*` nodes, and
`/dev/uinput` are root-owned by default. Install the shipped udev rule to grant
your user (via `uaccess`/`plugdev`) access:

```sh
sudo ./scripts/install-udev.sh     # installs docs/packaging/99-rusty-tartarus.rules
```

On some systems `systemd-logind` already grants the seat user an ACL on these
nodes, in which case the daemon runs without the rule.

## Usage

```sh
# Inspect (safe — does not grab the device)
tartarus discover            # list the Tartarus input nodes
tartarus info                # firmware, serial, connection, active profile

# Map your physical keys -> evdev names (for building binds)
tartarus discover-keys       # grabs the keypad; prints KEY_* per press; Ctrl-C

# Run the daemon (grabs the device, applies your profile, serves IPC)
tartarus run [--config PATH] # config defaults to $XDG_CONFIG_HOME/rusty-tartarus/config.toml

# Drive a running daemon
tartarus profile list
tartarus profile switch "MMO Grind"
tartarus brightness 200
tartarus autoswitch on
```

See [`docs/examples/config.toml`](docs/examples/config.toml) for the config
format (binds, a Hypershift layer, macros, lighting).

## UI development — headless only

**The UI must never spawn a window on your physical displays during development.**
It is run exclusively inside a headless [`cage`](https://github.com/cage-kiosk/cage)
compositor and screenshotted with `grim`:

```sh
cargo build --bin tartarus-ui
./scripts/headless-screenshot.sh ./target/debug/tartarus-ui   # -> screenshots/ui.png
```

Never run `dx serve` or the UI binary bare. See
[`docs/research/04-dioxus-and-headless.md`](docs/research/04-dioxus-and-headless.md).

## Status

Built & hardware-validated: the protocol/HID layer (firmware read + all Chroma
effects accepted by the device), the daemon as device-owner + IPC server, and the
full UI wired to the daemon (with a graceful "daemon offline" path).

In progress: per-application auto-switch (M4), end-to-end validation of the remap
pipeline, and hardening. Analog actuation config is gated on protocol
reverse-engineering. Track milestones in [`docs/PLAN.md`](docs/PLAN.md).

## License

GPL-3.0-or-later.
