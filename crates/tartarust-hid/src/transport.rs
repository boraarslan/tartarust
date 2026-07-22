//! hidraw transport: send/receive 90-byte Razer feature reports over a control
//! interface, with the OpenRazer busy/retry handshake.
//!
//! ## hidraw feature-report framing (the one thing untestable without hardware)
//!
//! `HIDIOCSFEATURE` / `HIDIOCGFEATURE` use a buffer whose **first byte is the
//! report id** (0 for this device's unnumbered control report), with the report
//! body following. The USB hidraw path is symmetric for report-id 0: for both
//! SET and GET the buffer is `1 + 90 = 91` bytes, `buf[0] = 0x00`, and the
//! 90-byte report lives at `buf[1..91]`.
//!
//! Source: kernel `Documentation/hid/hidraw.rst` (SET: "set the first byte to
//! the report number ... data begins in the second byte ... len one more than
//! the report"); GET is made symmetric by `usbhid_get_raw_report`, which for
//! `report_number == 0` advances the buffer so the device bytes land at offset 1.
//!
//! If this offset were wrong, `status` would parse as the report-id byte (0) and
//! every field would shift by one. Two cheap runtime checks catch that on the
//! first probe instead of silently corrupting data:
//!   1. the response `command_class`/`command_id` must echo the request
//!      (also the OpenRazer retry discriminator, `razerkbd_driver.c:424-430`);
//!   2. a valid response `status` is `0x01` (busy) or `0x02` (successful).

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tartarust_protocol::{REPORT_SIZE, RazerReport};

use crate::discover::discover_control_node;
use crate::error::HidError;

/// hidraw feature-report buffer length: report-id byte + 90-byte report.
const FEATURE_BUF_LEN: usize = REPORT_SIZE + 1;

/// Delay between SET and GET, matching OpenRazer's per-device wait (~600µs is a
/// safe value across Razer keyboards; `razerkbd_driver.c` uses device-specific
/// waits, the Tartarus family sits at the low end).
const SET_GET_DELAY: Duration = Duration::from_micros(600);

/// Maximum command attempts (OpenRazer loops `for (retry = 5; retry > 0; ...)`).
const MAX_ATTEMPTS: u32 = 5;

/// Delay between retries (`fsleep(10000)` = 10ms, `razerkbd_driver.c:443`).
const RETRY_DELAY: Duration = Duration::from_millis(10);

// hidraw ioctls. Both HIDIOCSFEATURE (nr 0x06) and HIDIOCGFEATURE (nr 0x07) are
// `_IOC(_IOC_WRITE | _IOC_READ, 'H', nr, len)` — same direction bits — so both
// use the read/write buffer variant. The length is supplied at call time.
#[allow(unsafe_code)]
mod ioctl {
    use nix::ioctl_readwrite_buf;

    // HIDIOCSFEATURE(len): _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x06, len)
    ioctl_readwrite_buf!(hidiocsfeature, b'H', 0x06, u8);
    // HIDIOCGFEATURE(len): _IOC(_IOC_WRITE|_IOC_READ, 'H', 0x07, len)
    // Both macros share the same direction bits; do NOT switch the SET side to a
    // write-only variant — the wrong direction bits would yield -ENOTTY.
    ioctl_readwrite_buf!(hidiocgfeature, b'H', 0x07, u8);
}

/// An open handle to the Tartarus Pro control hidraw node.
pub struct Transport {
    file: File,
    path: PathBuf,
}

impl Transport {
    /// Discover and open the control interface (interface 2) for `1532:0244`.
    pub fn open() -> Result<Self, HidError> {
        let path = discover_control_node()?;
        Self::open_path(&path)
    }

    /// Open a specific hidraw node (e.g. for testing against a known path).
    pub fn open_path(path: &Path) -> Result<Self, HidError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| HidError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        tracing::debug!(node = %path.display(), "opened control interface");
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// The opened device path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Send one report via `HIDIOCSFEATURE` (no response read).
    pub fn send(&self, report: &RazerReport) -> Result<(), HidError> {
        let bytes = report.to_bytes()?;
        let mut buf = [0u8; FEATURE_BUF_LEN];
        buf[0] = 0x00; // report id (unnumbered)
        buf[1..].copy_from_slice(&bytes);

        // SAFETY: `buf` is a valid, writable `FEATURE_BUF_LEN`-byte buffer and
        // the fd is an open hidraw node; the ioctl reads/writes within `buf`.
        #[allow(unsafe_code)]
        let res = unsafe { ioctl::hidiocsfeature(self.file.as_raw_fd(), &mut buf) };
        res.map_err(|source| HidError::Ioctl {
            op: "HIDIOCSFEATURE",
            source,
        })?;
        Ok(())
    }

    /// Read one report via `HIDIOCGFEATURE`. The 90-byte report is returned in
    /// `buf[1..91]` (the report-id byte stays at `buf[0]`).
    ///
    /// The ioctl returns a *positive* count of the bytes the device transferred;
    /// a control-IN can short-return without an ioctl error. We require at least
    /// a full report body (`REPORT_SIZE`), so a truncated read becomes a
    /// [`HidError::ShortRead`] instead of parsing with a zero-filled tail (which
    /// would yield wrong-but-plausible firmware/serial). We do **not** require
    /// `== FEATURE_BUF_LEN`: an unnumbered report-id-0 GET may legitimately
    /// return 90 body bytes (no report-id prefix counted), per the framing note
    /// at the top of this module — a `== 91` check would reject every valid read.
    pub fn recv(&self) -> Result<RazerReport, HidError> {
        let mut buf = [0u8; FEATURE_BUF_LEN];
        buf[0] = 0x00; // report id to request (unnumbered)

        // SAFETY: as in `send` — valid buffer, open hidraw fd.
        #[allow(unsafe_code)]
        let res = unsafe { ioctl::hidiocgfeature(self.file.as_raw_fd(), &mut buf) };
        let count = res.map_err(|source| HidError::Ioctl {
            op: "HIDIOCGFEATURE",
            source,
        })?;

        check_get_feature_len(count)?;

        let mut report_bytes = [0u8; REPORT_SIZE];
        report_bytes.copy_from_slice(&buf[1..FEATURE_BUF_LEN]);
        Ok(RazerReport::from_bytes(&report_bytes)?)
    }

    /// Run a full command: SET → wait → GET, with the OpenRazer retry semantics.
    ///
    /// - Up to 5 attempts, 10ms apart.
    /// - A response is accepted when its `command_class` and `command_id` echo
    ///   the request **and** its status is success/busy (busy is treated as
    ///   success). Mismatched header or failing status triggers a retry.
    /// - After exhausting retries, returns [`HidError::DeviceStatus`] or
    ///   [`HidError::ResponseMismatch`].
    pub fn command(&self, request: &RazerReport) -> Result<RazerReport, HidError> {
        let mut last_status: Option<u8> = None;
        let mut mismatched = false;

        for attempt in 1..=MAX_ATTEMPTS {
            if attempt > 1 {
                std::thread::sleep(RETRY_DELAY);
            }

            self.send(request)?;
            std::thread::sleep(SET_GET_DELAY);
            let response = self.recv()?;

            // Header echo check — also our offset-correctness discriminator.
            if response.command_class != request.command_class
                || response.command_id != request.command_id
            {
                tracing::warn!(
                    attempt,
                    req_class = request.command_class,
                    req_id = request.command_id,
                    resp_class = response.command_class,
                    resp_id = response.command_id,
                    resp_status = response.status,
                    "response header did not echo request (possible framing offset error)"
                );
                mismatched = true;
                continue;
            }

            // Second framing guard: a correct CRC corroborates the header echo.
            // Kept NON-FATAL on purpose — Razer devices do not reliably set the
            // response CRC (OpenRazer never verifies it), so a mismatch is logged
            // but does not reject an otherwise-valid response. The ShortRead
            // check in `recv()` is the load-bearing truncation guard.
            if !response.verify_crc() {
                tracing::warn!(
                    attempt,
                    class = response.command_class,
                    id = response.command_id,
                    "response CRC did not verify (non-fatal; device may not set it)"
                );
            }

            match response.status() {
                Some(s) if s.is_success() => return Ok(response),
                other => {
                    last_status = Some(response.status);
                    tracing::debug!(
                        attempt,
                        status = response.status,
                        ?other,
                        "device returned non-success status; retrying"
                    );
                }
            }
        }

        if let Some(status) = last_status {
            Err(HidError::DeviceStatus {
                status,
                attempts: MAX_ATTEMPTS,
            })
        } else if mismatched {
            Err(HidError::ResponseMismatch {
                attempts: MAX_ATTEMPTS,
            })
        } else {
            // Should be unreachable: every attempt either returns, sets
            // last_status, or sets mismatched.
            Err(HidError::ResponseMismatch {
                attempts: MAX_ATTEMPTS,
            })
        }
    }
}

/// Validate the byte count a `HIDIOCGFEATURE` ioctl reports against the minimum
/// for a full report. Returns [`HidError::ShortRead`] for a truncated read.
///
/// Accepts `>= REPORT_SIZE` (90) — i.e. a full report body — so both legitimate
/// outcomes pass: 90 (body only, report-id prefix not counted) and 91 (prefix
/// counted). A negative count (shouldn't happen after the `Ok`/`Err` split) is
/// clamped to 0 and rejected.
fn check_get_feature_len(count: nix::libc::c_int) -> Result<(), HidError> {
    let read = count.max(0) as usize;
    if read < REPORT_SIZE {
        return Err(HidError::ShortRead {
            read,
            needed: REPORT_SIZE,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_report_lengths_accepted() {
        // 90 (body only, no prefix counted) and 91 (prefix counted) are the two
        // legitimate outcomes — both must pass.
        assert!(check_get_feature_len(REPORT_SIZE as nix::libc::c_int).is_ok());
        assert!(check_get_feature_len(FEATURE_BUF_LEN as nix::libc::c_int).is_ok());
    }

    #[test]
    fn short_read_rejected() {
        // A truncated control-IN: header present but payload cut off.
        let err = check_get_feature_len(8).unwrap_err();
        match err {
            HidError::ShortRead { read, needed } => {
                assert_eq!(read, 8);
                assert_eq!(needed, REPORT_SIZE);
            }
            other => panic!("expected ShortRead, got {other:?}"),
        }
        // Zero bytes (e.g. a NAK that still returns Ok(0)) is also rejected.
        assert!(matches!(
            check_get_feature_len(0),
            Err(HidError::ShortRead { read: 0, .. })
        ));
        // One byte short of a full body is rejected.
        assert!(check_get_feature_len((REPORT_SIZE - 1) as nix::libc::c_int).is_err());
    }

    #[test]
    fn negative_count_rejected() {
        // Shouldn't occur after the Ok/Err split, but must not panic or pass.
        assert!(matches!(
            check_get_feature_len(-1),
            Err(HidError::ShortRead { read: 0, .. })
        ));
    }
}
