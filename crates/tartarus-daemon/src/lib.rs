//! `tartarus-daemon` — the background service for the Razer Tartarus Pro.
//!
//! The daemon is the **single owner** of the device: it grabs the Tartarus Pro
//! input nodes (evdev) and re-emits remapped events through a virtual uinput
//! device, AND it holds the one hidraw control transport for lighting/brightness.
//! Clients (UI, CLI) never touch the device directly — they drive it over the
//! `tartarus-ipc` Unix-socket API.
//!
//! Architecture (single-owner actors, no shared locks on the hot path):
//!   * [`engine`] — the state authority. One thread owns all profiles/macros/
//!     active-profile + the live key state. Two entry points mutate it on that
//!     thread: physical key events (`handle_key`) and IPC requests (`apply`).
//!   * [`output`] — owns the uinput virtual device; fed emit/macro commands.
//!   * [`device`] — owns the hidraw transport; fed lighting/brightness commands.
//!   * [`input`] — discover + grab the physical nodes.
//!   * [`ipc_server`] — async Unix-socket server; forwards requests to the engine
//!     via a channel and awaits the reply.
//!   * [`state`] — the on-disk config (the shared model serialized to TOML).
//!
//! The pure logic (binding resolution, request handling, macro lowering, config
//! parse) is unit-tested here without a device or root.

// evdev (and the uinput ioctls underneath it) need no `unsafe` at our layer.
#![forbid(unsafe_code)]

pub mod analog_source;
pub mod appfocus;
pub mod cli;
pub mod device;
pub mod engine;
pub mod input;
pub mod ipc_server;
pub mod macros;
pub mod output;
pub mod pipeline;
pub mod state;
pub mod tray;
