# tartarust — project instructions

A from-scratch userspace driver suite for the **Razer Tartarus Pro** keypad
(`USB 1532:0244`): a device-owning daemon + a CLI + a GPUI desktop UI, all in
one Cargo workspace. See [`README.md`](README.md) for the user-facing overview
and [`docs/PLAN.md`](docs/PLAN.md) for milestones.

## Workspace layout

Root package `tartarust` ships three thin bin shims (`src/bin/*.rs`) so a single
`cargo install --path .` installs all binaries. All real code lives in the member
crates:

| Crate | Binary / lib | Role |
|---|---|---|
| `tartarust-protocol` | lib | 90-byte HID report + typed command set, no I/O |
| `tartarust-hid` | lib | `hidraw` transport (SET/GET feature reports) |
| `tartarust-ipc` | lib | daemon↔client wire schema + shared domain types |
| `tartarust-daemon` | `tartarust-daemon` | sole device owner; evdev→uinput remap engine + IPC server |
| `tartarust-cli` | `tartarust` | scriptable client + diagnostics |
| `tartarust-ui` | `tartarust-ui` | GPUI desktop app (thin IPC client) |

## Versioning

- **One version for the entire workspace.** It is defined in exactly one place —
  `version` under `[workspace.package]` in the root `Cargo.toml` — and the root
  package plus all six member crates inherit it with `version.workspace = true`.
  Do not add a literal `version = "x.y.z"` to any member crate; that would let
  them drift.
- **Current version: `0.2.0`.** To release a new version, edit that single
  `[workspace.package].version` field and everything moves together.
- **Semver intent:** pre-1.0, treat `0.MINOR.PATCH` as breaking-allowed on
  `MINOR`. The `tartarust-ipc` wire schema is the compatibility surface between a
  running daemon and its clients — a breaking IPC change is a `MINOR` bump, and a
  running daemon rejects request variants it does not know, so restart the daemon
  after IPC changes.

## Common commands

```sh
cargo build --release        # all binaries (UI: gpui — needs libxkbcommon + fontconfig)
cargo test --workspace       # unit + integration tests
cargo install --path .       # installs tartarust-daemon, tartarust, tartarust-ui
cargo build --bin tartarust-ui && ./scripts/headless-screenshot.sh ./target/debug/tartarust-ui
```

## Hard rules

- **The UI is headless-only in dev.** Never spawn a `tartarust-ui` window on a
  physical display — run it exclusively inside the headless `cage` harness
  (`scripts/headless-screenshot.sh`). The harness sets
  `TARTARUST_UI_NO_AUTOSTART=1` and `TARTARUST_UI_NO_DESKTOP_INSTALL=1`.
- **The daemon is the single device owner.** Exactly one process holds the
  device (`hidraw` + EVIOCGRAB on the keypad's NKRO node); the UI and CLI only
  talk to it over the control socket (`$XDG_RUNTIME_DIR/tartarust.sock`, override
  `TARTARUST_SOCKET`). Never grab a non-Tartarus device.
- CI (`.github/workflows/ci.yml`) gates on `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `cargo test --workspace`. Keep the tree rustfmt-clean and clippy-clean; run
  those three locally before pushing.
