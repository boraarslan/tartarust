//! `tartarust-protocol` — Razer Tartarus Pro (USB `1532:0244`) wire protocol.
//!
//! Pure data + (de)serialization for the 90-byte Razer HID feature report and
//! the typed command set (device info, lighting/Chroma, profiles). Deliberately
//! transport-agnostic so it is reused by the daemon, the CLI, and unit tests.
//!
//! The concrete report layout, the per-device `transaction_id` for product
//! `0x0244`, and the command tables are filled in once the research docs land
//! (see `docs/research/01-razer-protocol.md` and `02-tartarus-pro-device.md`).

#![forbid(unsafe_code)]

/// USB vendor id — Razer USA, Ltd.
pub const VENDOR_ID: u16 = 0x1532;
/// USB product id — Tartarus Pro (`RZ07-0311`).
pub const PRODUCT_ID: u16 = 0x0244;

/// Total size of a Razer HID feature report, in bytes.
pub const REPORT_SIZE: usize = 90;

pub mod analog; // DRIVER-mode per-key pressure frame decode (report 0x06)
pub mod command; // command_class / command_id / args builders + transaction_id rule
pub mod device; // firmware, serial, device-mode, brightness
pub mod error; // ProtocolError
pub mod lighting; // Chroma effects + custom matrix frames
pub mod report; // 90-byte `RazerReport` (deku) + XOR checksum
// pub mod profile; // onboard memory / profile slots — not exposed for the Pro (M4)

pub use analog::{AnalogFrame, ANALOG_FRAME_LEN, ANALOG_KEY_COUNT, ANALOG_REPORT_ID};
pub use error::ProtocolError;
pub use report::{RazerReport, Status};
