//! Transport-layer errors for the hidraw control channel.

use std::path::PathBuf;

use tartarus_protocol::ProtocolError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HidError {
    /// No hidraw node matching `1532:0244` interface 2 was found under sysfs.
    #[error("no Tartarus Pro control interface (1532:0244, interface 2) found")]
    DeviceNotFound,

    /// Opening or doing I/O on the hidraw node failed. `EACCES` here usually
    /// means the udev rule granting non-root access has not been installed yet.
    #[error("hidraw I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A raw ioctl (`HIDIOCSFEATURE` / `HIDIOCGFEATURE`) failed.
    #[error("ioctl {op} failed: {source}")]
    Ioctl {
        op: &'static str,
        #[source]
        source: nix::errno::Errno,
    },

    /// The device returned a failing status after all retries.
    #[error("device returned status 0x{status:02x} after {attempts} attempt(s)")]
    DeviceStatus { status: u8, attempts: u32 },

    /// The response header (class/id/remaining) never matched the request.
    #[error("response did not match request after {attempts} attempt(s)")]
    ResponseMismatch { attempts: u32 },

    /// A `GET_REPORT` (HIDIOCGFEATURE) returned fewer bytes than a full report.
    /// A control-IN can short-return as a *positive* count (not an ioctl error);
    /// the unread tail would otherwise stay zero-filled and parse as a
    /// wrong-but-plausible report. `read` is the byte count the device
    /// transferred; `needed` is the minimum we require.
    #[error("short control-IN read: device transferred {read} bytes, need >= {needed}")]
    ShortRead { read: usize, needed: usize },

    /// A (de)serialization error from the protocol layer.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

impl HidError {
    /// True if this is a permission error (`EACCES`) — the expected failure
    /// before the udev rule is installed.
    pub fn is_permission_denied(&self) -> bool {
        match self {
            HidError::Io { source, .. } => source.kind() == std::io::ErrorKind::PermissionDenied,
            HidError::Ioctl { source, .. } => *source == nix::errno::Errno::EACCES,
            _ => false,
        }
    }

    /// True if this is an `EPIPE` (USB STALL) on a feature-report ioctl — the
    /// transient failure seen occasionally on the `HIDIOCSFEATURE` send phase
    /// (e.g. kernel-module contention on the node). `Transport::command` does
    /// **not** retry it, so callers doing reliability-sensitive writes should
    /// retry once (~20ms) when this is true. Lets downstream crates detect the
    /// stall without taking their own `nix` dependency to name `Errno`.
    pub fn is_epipe_stall(&self) -> bool {
        matches!(
            self,
            HidError::Ioctl {
                source: nix::errno::Errno::EPIPE,
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::errno::Errno;

    #[test]
    fn epipe_stall_detection() {
        let stall = HidError::Ioctl {
            op: "HIDIOCSFEATURE",
            source: Errno::EPIPE,
        };
        assert!(stall.is_epipe_stall());
        assert!(!stall.is_permission_denied());

        let eacces = HidError::Ioctl {
            op: "HIDIOCSFEATURE",
            source: Errno::EACCES,
        };
        assert!(!eacces.is_epipe_stall());
        assert!(eacces.is_permission_denied());

        assert!(!HidError::DeviceNotFound.is_epipe_stall());
    }

    #[test]
    fn permission_denied_detection() {
        let io = HidError::Io {
            path: std::path::PathBuf::from("/dev/hidraw0"),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert!(io.is_permission_denied());
        assert!(!io.is_epipe_stall());
    }
}
