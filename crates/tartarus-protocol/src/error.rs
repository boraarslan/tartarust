//! Protocol-level errors.

use thiserror::Error;

/// Errors raised while (de)serializing reports, building commands, or
/// interpreting a device response. Transport (hidraw I/O) errors live in
/// `tartarus-hid`; this crate stays transport-agnostic.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// A `deku` (de)serialization failure for the 90-byte report.
    #[error("report (de)serialization failed: {0}")]
    Codec(#[from] deku::DekuError),

    /// The device replied with a non-success status byte. The `u8` is the raw
    /// status; `Status` decodes the known values (see [`crate::report::Status`]).
    #[error("device reported failure status 0x{0:02x}")]
    DeviceStatus(u8),

    /// A command argument was out of range (e.g. a custom-frame row longer than
    /// the 80-byte argument area, or an invalid column span).
    #[error("invalid command argument: {0}")]
    InvalidArgument(&'static str),

    /// A response payload could not be parsed (e.g. a non-UTF-8 serial string).
    #[error("could not parse device response: {0}")]
    BadResponse(&'static str),
}
