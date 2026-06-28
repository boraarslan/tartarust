//! The device-control thread: the single owner of the hidraw `Transport`.
//!
//! Exactly one thread holds the control-interface transport and serializes all
//! lighting / brightness / info traffic to the device. The engine and IPC
//! layers never touch hidraw directly — they post [`DeviceCmd`]s here. This
//! mirrors the uinput output thread: one owner, fed by a channel.
//!
//! ## Brightness needs no "acquire" step
//!
//! An earlier theory held that a bare `set_brightness` stalls with `EPIPE`
//! unless a lighting effect is sent first ("driver mode" acquire). device-core
//! disproved this on real hardware: a fresh open → effect → brightness-to-a-
//! different-value both return status `0x02` SUCCESS with no acquire, and
//! OpenRazer itself changes Pro brightness directly. The earlier `EPIPE` was
//! transient USB contention (the OpenRazer kernel module also poking hidraw),
//! not an onboard-mode lockout. So we apply effects/brightness directly.
//!
//! The one robustness measure: `Transport::command()` does NOT retry an `EPIPE`
//! STALL on the `HIDIOCSFEATURE` (send) phase. For reliability-sensitive writes
//! (lighting/brightness) we wrap the command in a single `EPIPE` retry with a
//! short backoff — see [`send`].

use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tartarus_hid::Transport;
use tartarus_ipc::model::{Effect, LightingState, Rgb, WaveDirection};
use tartarus_ipc::DeviceInfo;
use tartarus_protocol::{device, lighting, RazerReport};

/// Backoff before retrying a write that failed with a transient `EPIPE` STALL.
const EPIPE_RETRY_DELAY: Duration = Duration::from_millis(20);

/// A command for the device thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCmd {
    /// Apply a full lighting state (effect + brightness).
    ApplyLighting(LightingState),
    /// Set just the brightness. Applied directly — no acquire step needed.
    SetBrightness(u8),
    /// Read firmware + serial + connection; reply on the channel.
    GetInfo,
}

/// Handle to the device thread. Drop to stop it (closes the transport).
pub struct DeviceHandle {
    tx: Sender<Job>,
    join: Option<JoinHandle<()>>,
}

/// Internal job: a command plus an optional reply slot for queries.
enum Job {
    Cmd(DeviceCmd),
    Query(std::sync::mpsc::Sender<DeviceInfo>),
}

impl DeviceHandle {
    /// Post a fire-and-forget command (lighting / brightness).
    pub fn send(&self, cmd: DeviceCmd) {
        let _ = self.tx.send(Job::Cmd(cmd));
    }

    /// Read device info synchronously (blocks until the device thread replies).
    /// Returns a disconnected `DeviceInfo` if the thread is gone.
    pub fn query_info(&self) -> DeviceInfo {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if self.tx.send(Job::Query(reply_tx)).is_err() {
            return DeviceInfo {
                firmware: None,
                serial: None,
                connected: false,
            };
        }
        reply_rx.recv().unwrap_or(DeviceInfo {
            firmware: None,
            serial: None,
            connected: false,
        })
    }

    /// Stop the device thread and wait for it to exit.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let (dead, _rx) = std::sync::mpsc::channel();
        self.tx = dead;
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for DeviceHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the device thread. Opening the transport is attempted but not required:
/// if the control interface is unavailable, the thread still runs and reports
/// `connected: false`, so the daemon's remap engine + IPC work without lighting.
pub fn spawn() -> DeviceHandle {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    let join = thread::Builder::new()
        .name("tartarus-device".into())
        .spawn(move || device_loop(rx))
        .expect("spawning device thread");
    DeviceHandle {
        tx,
        join: Some(join),
    }
}

fn device_loop(rx: Receiver<Job>) {
    let mut transport = match Transport::open() {
        Ok(t) => {
            tracing::info!(path = %t.path().display(), "opened device control interface");
            Some(t)
        }
        Err(err) => {
            tracing::warn!(?err, "device control interface unavailable; lighting disabled");
            None
        }
    };

    for job in rx {
        match job {
            Job::Cmd(DeviceCmd::ApplyLighting(state)) => {
                if let Some(t) = transport.as_mut() {
                    apply_lighting(t, &state);
                }
            }
            Job::Cmd(DeviceCmd::SetBrightness(level)) => {
                if let Some(t) = transport.as_mut() {
                    send(t, device::set_brightness(level), "brightness");
                }
            }
            Job::Cmd(DeviceCmd::GetInfo) => {
                // GetInfo with no reply slot: just refresh nothing.
            }
            Job::Query(reply) => {
                let info = read_info(transport.as_ref());
                let _ = reply.send(info);
            }
        }
    }
    tracing::debug!("device thread exiting; transport closed");
}

/// Apply a full lighting state: send the effect, then the brightness. No
/// "acquire" step — the device accepts both directly (see the module docs).
fn apply_lighting(t: &Transport, state: &LightingState) {
    send(t, effect_report(&state.effect), "effect");
    send(t, device::set_brightness(state.brightness), "brightness");
}

/// Read firmware + serial + connection state.
///
/// `connected` reflects a LIVE probe — whether the firmware `command()` actually
/// succeeds right now — not mere transport presence. After an unplug the open fd
/// lingers but commands fail, so this returns `connected: false`, keeping a
/// `query_info()` (and thus `DaemonStatus.device_connected`) honest instead of
/// stuck at the startup value.
fn read_info(transport: Option<&Transport>) -> DeviceInfo {
    let Some(t) = transport else {
        return DeviceInfo {
            firmware: None,
            serial: None,
            connected: false,
        };
    };
    // The firmware read doubles as the liveness probe.
    let firmware = t
        .command(&device::get_firmware_version())
        .ok()
        .map(|resp| device::parse_firmware_version(&resp).to_string());
    let connected = firmware.is_some();
    let serial = t
        .command(&device::get_serial())
        .ok()
        .and_then(|resp| device::parse_serial(&resp).ok());
    DeviceInfo {
        firmware,
        serial,
        connected,
    }
}

/// Map a shared-model [`Effect`] to its `tartarus-protocol` report builder.
fn effect_report(effect: &Effect) -> RazerReport {
    match effect {
        Effect::None => lighting::effect_none(),
        Effect::Spectrum => lighting::effect_spectrum(),
        Effect::Static { color } => lighting::effect_static(rgb(*color)),
        Effect::Wave { direction } => lighting::effect_wave(wave_dir(*direction)),
        Effect::Reactive { speed, color } => lighting::effect_reactive(*speed, rgb(*color)),
        Effect::Breathing { colors } => match colors.as_slice() {
            [] => lighting::effect_breathing_random(),
            [c] => lighting::effect_breathing_single(rgb(*c)),
            [c1, c2, ..] => lighting::effect_breathing_dual(rgb(*c1), rgb(*c2)),
        },
        Effect::Starlight { speed, colors } => match colors.as_slice() {
            [] => lighting::effect_starlight_random(*speed),
            [c] => lighting::effect_starlight_single(*speed, rgb(*c)),
            [c1, c2, ..] => lighting::effect_starlight_dual(*speed, rgb(*c1), rgb(*c2)),
        },
        // Custom frames need the per-key matrix command; for M-this-integration
        // we set the custom-frame effect. Painting the matrix is a follow-up
        // (set_custom_frame) once the UI sends per-key colors.
        Effect::Custom { .. } => lighting::effect_custom_frame(),
    }
}

fn rgb(c: Rgb) -> lighting::Rgb {
    lighting::Rgb::new(c.r, c.g, c.b)
}

fn wave_dir(d: WaveDirection) -> lighting::WaveDirection {
    match d {
        // Protocol enum names confirmed from lighting.rs (WaveDirection).
        WaveDirection::Left => lighting::WaveDirection::Left,
        WaveDirection::Right => lighting::WaveDirection::Right,
    }
}

/// Send one report, with a single `EPIPE`-STALL retry, logging (not
/// propagating) any final error — a flaky device must not take down the daemon.
///
/// `Transport::command()` does not retry an `EPIPE` STALL on the
/// `HIDIOCSFEATURE` (send) phase, which can occur under transient USB contention
/// (e.g. the OpenRazer kernel module also touching the device). One short-backoff
/// retry clears it; a second failure is logged and dropped.
fn send(t: &Transport, report: RazerReport, what: &str) {
    match t.command(&report) {
        Ok(_) => {}
        Err(err) if err.is_epipe_stall() => {
            tracing::debug!(%what, "EPIPE stall on send; retrying once after backoff");
            thread::sleep(EPIPE_RETRY_DELAY);
            if let Err(err) = t.command(&report) {
                tracing::warn!(%what, ?err, "device command failed after EPIPE retry");
            }
        }
        Err(err) => tracing::warn!(%what, ?err, "device command failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tartarus_hid::HidError;

    // The effect->builder mapping is the device-facing logic we can test without
    // hardware: every Effect variant must map to a report without panicking, and
    // the color-count branches must pick the right builder.
    #[test]
    fn every_effect_maps_to_a_report() {
        let effects = [
            Effect::None,
            Effect::Spectrum,
            Effect::Static {
                color: Rgb::new(1, 2, 3),
            },
            Effect::Wave {
                direction: WaveDirection::Left,
            },
            Effect::Reactive {
                speed: 2,
                color: Rgb::new(4, 5, 6),
            },
            Effect::Breathing { colors: vec![] },
            Effect::Breathing {
                colors: vec![Rgb::new(1, 1, 1)],
            },
            Effect::Breathing {
                colors: vec![Rgb::new(1, 1, 1), Rgb::new(2, 2, 2)],
            },
            Effect::Starlight {
                speed: 1,
                colors: vec![],
            },
            Effect::Starlight {
                speed: 1,
                colors: vec![Rgb::new(9, 9, 9)],
            },
            Effect::Custom {
                frame: vec![Rgb::new(0, 0, 0); 21],
            },
        ];
        for e in &effects {
            // Builders return a valid report; just ensure no panic and the CRC
            // is set (report is constructed).
            let report = effect_report(e);
            assert!(report.verify_crc(), "effect {e:?} produced an invalid report");
        }
    }

    /// The daemon's `Effect` -> report mapping (distinct code from device-core's
    /// example) must produce the right command bytes. The "matrix effect"
    /// commands (everything except Custom) are class 0x0F, id 0x02, with
    /// args = [VARSTORE=0x01, BACKLIGHT_LED=0x05, effect_id, ...]. effect_id:
    /// none=0x00, static=0x01, breathing=0x02, spectrum=0x03, wave=0x04,
    /// reactive=0x05, starlight=0x07. Custom uses a separate frame command.
    #[test]
    fn matrix_effect_maps_to_correct_command_bytes() {
        // (effect, expected effect_id, expected color triple to find in args)
        let cases: &[(Effect, u8, Option<[u8; 3]>)] = &[
            (Effect::None, 0x00, None),
            (Effect::Spectrum, 0x03, None),
            (
                Effect::Static {
                    color: Rgb::new(10, 20, 30),
                },
                0x01,
                Some([10, 20, 30]),
            ),
            (
                Effect::Wave {
                    direction: WaveDirection::Right,
                },
                0x04,
                None,
            ),
            (
                Effect::Breathing {
                    colors: vec![Rgb::new(7, 8, 9)],
                },
                0x02,
                Some([7, 8, 9]),
            ),
            (
                Effect::Starlight {
                    speed: 3,
                    colors: vec![],
                },
                0x07,
                None,
            ),
        ];
        for (effect, expected_id, expected_color) in cases {
            let r = effect_report(effect);
            assert_eq!(r.command_class, 0x0F, "effect {effect:?}: wrong class");
            assert_eq!(r.command_id, 0x02, "effect {effect:?}: wrong id");
            assert_eq!(r.arguments[0], 0x01, "effect {effect:?}: arg0 != VARSTORE");
            assert_eq!(
                r.arguments[1], 0x05,
                "effect {effect:?}: arg1 != BACKLIGHT_LED"
            );
            assert_eq!(
                r.arguments[2], *expected_id,
                "effect {effect:?}: wrong effect_id byte"
            );
            if let Some(color) = expected_color {
                // The exact color offset varies per effect; assert the RGB
                // triple appears contiguously in the args.
                let found = r.arguments.windows(3).any(|w| w == color);
                assert!(
                    found,
                    "effect {effect:?}: color {color:?} not found in args {:?}",
                    &r.arguments[..12]
                );
            }
        }
    }

    /// `Custom` maps to the custom-frame command (a different command than the
    /// matrix-effect setters). M-this-task only sets the effect; per-key frame
    /// painting (`set_custom_frame`) is a documented follow-up.
    #[test]
    fn custom_effect_maps_to_custom_frame_command() {
        let r = effect_report(&Effect::Custom {
            frame: vec![Rgb::new(0, 0, 0); 21],
        });
        assert!(r.verify_crc());
        // effect_custom_frame's identifying effect_id byte is 0x08.
        assert_eq!(r.arguments[2], 0x08, "custom effect_id byte");
    }

    #[test]
    fn brightness_command_carries_the_level() {
        // set_brightness lays [VARSTORE, ZERO_LED, level]; assert the level byte.
        let r = device::set_brightness(173);
        assert_eq!(r.arguments[2], 173, "brightness level byte mismatch");
    }

    #[test]
    fn non_epipe_error_is_not_a_retriable_stall() {
        // EPIPE detection itself lives in (and is tested in) tartarus-hid via
        // HidError::is_epipe_stall(); here we just confirm a non-ioctl error
        // does not look like a retriable stall to our send() path.
        let io = HidError::Io {
            path: std::path::PathBuf::from("/dev/hidraw0"),
            source: std::io::Error::from(std::io::ErrorKind::BrokenPipe),
        };
        assert!(!io.is_epipe_stall());
    }
}
