//! `tartarust-hid` — transport for the Tartarus Pro control interface.
//!
//! Opens the device's vendor/control HID interface and exchanges 90-byte
//! `tartarust-protocol` reports via `SET_REPORT` / `GET_REPORT` feature reports
//! (hidraw `HIDIOCSFEATURE` / `HIDIOCGFEATURE`). The exact control interface and
//! the busy/retry handshake are pinned from `docs/research/01-razer-protocol.md`.

// `deny` (not `forbid`) so the ioctl call sites in `transport` can opt back in
// with a localized `#[allow(unsafe_code)]`. Everything else stays unsafe-free.
#![deny(unsafe_code)]

pub mod analog; // DRIVER-mode enable + live per-key pressure reader (interface 1)
pub mod discover; // locate the Tartarus Pro control hidraw node via sysfs
pub mod error; // HidError
pub mod transport; // open hidraw node, send/recv feature report, retry-on-busy

pub use analog::{AnalogReader, DriverModeGuard, restore_normal_best_effort};
pub use discover::{discover_control_node, discover_node_for_interface};
pub use error::HidError;
pub use transport::Transport;
