//! Analog input source: DRIVER-mode pressure → key events for the grid.
//!
//! In analog mode the keypad is switched out of normal HID into DRIVER mode (see
//! [`tartarust_hid::AnalogReader`]), where the GRID streams raw per-key pressure
//! instead of keystrokes. This module turns that pressure stream into the SAME
//! [`EngineMsg::Input`] events the digital reader ([`crate::pipeline::spawn_reader`])
//! produces — so the engine, binds, Hypershift, and macros all work on top of
//! analog with zero engine changes.
//!
//! Analog mode is HYBRID, not a replacement for the evdev grab: DRIVER mode
//! silences only the grid digitally — the thumb-area keys (thumb button =
//! `KEY_LEFTALT`, thumbpad arrows) keep emitting classic HID reports on the boot
//! keyboard interface. So the daemon ALSO grabs every keyboard node and reads
//! the boot one (`main.rs`'s analog arm, [`crate::input::plan_grabs_analog`]);
//! this source and those digital readers COEXIST, feeding the same engine
//! channel — lifecycle code (unplug/POLLERR teardown, shutdown) must account for
//! both. The keys this source synthesizes are published in a shared
//! [`ClaimedKeys`] set the digital readers drop, so a key emitting on both paths
//! (the spacebar: analog byte 19 and possibly a digital boot-report key) never
//! double-feeds the engine.
//!
//! Two layers, deliberately split so the actuation logic is unit-testable with
//! no hardware:
//!
//!   * [`Actuator`] — the pure per-key state machine. Given a frame's pressures
//!     it returns the key-down/up events that frame's transitions imply. It holds
//!     each key's last logical state between frames, so a key stays DOWN across an
//!     idle gap (the device goes silent when pressure is steady) until a frame
//!     shows its pressure fell below the release point.
//!   * [`spawn`] — the thread that owns the [`AnalogReader`], polls it, feeds each
//!     frame to the [`Actuator`], and forwards the resulting events to the engine.
//!
//! ## Actuation semantics
//!
//! Per key, edge-triggered with hysteresis to prevent chatter:
//!   * rising across `actuation_point` (key currently up) → primary key DOWN;
//!   * falling below `release_point` (key currently down) → primary key UP.
//!
//! Dual-step ("second function"): an optional deeper `second_actuation_point`
//! with its own `second_action` key. Crossing the deeper point fires the second
//! key DOWN **in addition to** the primary (the primary stays held) — a layered,
//! not exclusive, model: a light press is the primary action, pressing harder
//! adds the secondary. The second key releases when pressure falls back below
//! `second_actuation_point - hysteresis`. Each step targets a distinct logical
//! evdev key, so the engine refcounts them independently and a chord on either
//! resolves that key's own bind.
//!
//! ## Restoring NORMAL (the hard invariant)
//!
//! DRIVER mode disables normal typing, so leaving the device there makes it mute
//! until replug. Closing the fd does NOT undo it — only an explicit NORMAL
//! command does. The [`AnalogReader`]'s `Drop` sends NORMAL (with re-enumeration
//! recovery). The daemon guarantees that Drop actually runs on shutdown by
//! COOPERATIVELY JOINING this thread before `process::exit` (poll has a timeout,
//! so unlike the digital reader this thread is joinable). For the panic/abort
//! path — where Drop is skipped — the daemon's panic hook calls
//! [`tartarust_hid::restore_normal_best_effort`] before aborting.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use evdev::KeyCode;
use tartarust_hid::AnalogReader;
use tartarust_protocol::{ANALOG_KEY_COUNT, AnalogFrame};

use crate::engine::parse_key;
use crate::ipc_server::EngineMsg;
use crate::state::{AnalogConfig, AnalogKey};

/// A live actuation update destined for the running actuator (from
/// `SetActuation`). The byte index plus the new settings; the source loop drains
/// these each poll iteration and applies them in place.
pub struct ActuationUpdate {
    /// Analog byte index to reconfigure.
    pub index: usize,
    /// The new settings for that key.
    pub key: AnalogKey,
}

/// The key codes the analog source CURRENTLY synthesizes from pressure, shared
/// with the hybrid-mode digital readers, which drop those codes (see
/// [`crate::pipeline::spawn_reader`]) so a key emitting on both paths never
/// double-feeds the engine.
///
/// The set is LIVE, not startup-fixed: `SetActuation` can change or remove a
/// `second_action` key name at runtime, and a stale set would keep dropping a
/// digital key nothing synthesizes anymore — killing it until restart. Single
/// writer: this module ([`spawn`] seeds it from the built [`Actuator`] before
/// any reader exists; the run loop republishes after every applied update).
/// Both sites publish through [`publish_claimed`], which subtracts the
/// thumb-area codes ([`THUMB_AREA_CODES`]) so a config collision cannot kill a
/// physical thumb key. Readers take the lock only on key-DOWN edges.
pub type ClaimedKeys = Arc<RwLock<HashSet<u16>>>;

/// The evdev codes the thumb-area physical keys emit digitally in DRIVER mode:
/// the thumb button (`KEY_LEFTALT`) plus the four thumbpad arrows (named only
/// in prose elsewhere in the daemon — this is their single in-code authority).
///
/// In DRIVER mode the grid is digitally SILENT, so these are the only codes
/// the boot-node reader can ever see from a DIFFERENT physical key than an
/// analog grid key. If a grid entry claims one of them (e.g. `second_action =
/// "KEY_LEFTALT"` — a natural "deep press = modifier" config), a naive drop
/// set would make the reader drop the real thumb key's every event: a
/// silently, deterministically dead physical key. [`publish_claimed`]
/// therefore subtracts these codes from the published set (with a warning).
/// Excluding them cannot re-introduce double-feeding — no grid key can emit
/// them digitally while DRIVER mode holds. (`KEY_SPACE`, the one genuine
/// same-physical-key dupe candidate, is not thumb-area and stays claimed.)
const THUMB_AREA_CODES: [KeyCode; 5] = [
    KeyCode::KEY_LEFTALT,
    KeyCode::KEY_UP,
    KeyCode::KEY_DOWN,
    KeyCode::KEY_LEFT,
    KeyCode::KEY_RIGHT,
];

/// Publish the actuator's claimed codes into the shared drop set, minus
/// [`THUMB_AREA_CODES`] (warned per collision — the user should know the
/// analog binding shares a code with a physical thumb key). Caveat of keeping
/// the thumb key alive: the engine tracks held state by input code, so holding
/// the colliding analog key AND the physical thumb key simultaneously aliases
/// (the first UP releases the pair) — mild, and far better than a dead key.
fn publish_claimed(actuator: &Actuator, claimed: &ClaimedKeys) {
    let mut codes = actuator.claimed_codes();
    for thumb in THUMB_AREA_CODES {
        if codes.remove(&thumb.0) {
            tracing::warn!(
                key = ?thumb,
                "analog config claims a thumb-area key code; the physical thumb \
                 key's digital events will still flow (holding both keys at once \
                 aliases: the first release ends the pair)"
            );
        }
    }
    if let Ok(mut set) = claimed.write() {
        *set = codes;
    }
}

/// A snapshot of live analog state shared between the source thread (writer) and
/// the engine/IPC handler (reader), for `GetAnalogState`.
///
/// `active` means "the source is running and DRIVER mode is held" — NOT "a frame
/// arrived recently". The device goes silent when keys are steady, so an
/// untouched-but-active keypad must still read active. `pressures` holds the LAST
/// frame (never cleared on idle), mirroring the actuator's own held state.
#[derive(Debug, Default)]
pub struct AnalogSnapshot {
    /// Whether the analog source is running with DRIVER mode held.
    active: AtomicBool,
    /// The latest per-key pressures. Held across idle gaps.
    pressures: Mutex<[u8; ANALOG_KEY_COUNT]>,
}

impl AnalogSnapshot {
    /// A fresh, inactive snapshot (all pressures zero).
    pub fn new() -> Arc<Self> {
        Arc::new(AnalogSnapshot::default())
    }

    /// Whether analog input is currently active (source running, DRIVER held).
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// The latest per-key pressures as a `Vec` (for the IPC response). Empty
    /// `Vec` is the caller's job when inactive — this always returns 23 bytes.
    pub fn pressures(&self) -> Vec<u8> {
        self.pressures
            .lock()
            .map(|p| p.to_vec())
            .unwrap_or_default()
    }

    /// Overwrite the held pressures with the latest frame (source thread only).
    fn store_frame(&self, frame: &AnalogFrame) {
        if let Ok(mut p) = self.pressures.lock() {
            *p = frame.pressures;
        }
    }

    /// Test-only: mark the snapshot active (simulates the source entering DRIVER).
    #[cfg(test)]
    pub fn mark_active_for_test(&self) {
        self.active.store(true, Ordering::SeqCst);
    }

    /// Test-only: publish a single-key pressure frame.
    #[cfg(test)]
    pub fn store_frame_for_test(&self, index: usize, pressure: u8) {
        if let Ok(mut p) = self.pressures.lock() {
            p[index] = pressure;
        }
    }
}

/// Engine input value for a key-down event (matches evdev / the digital path).
const VALUE_DOWN: i32 = 1;
/// Engine input value for a key-up event.
const VALUE_UP: i32 = 0;

/// One key's resolved actuation parameters and live logical state.
struct KeyActuator {
    /// Logical key the primary step maps to (engine resolves its bind).
    primary: KeyCode,
    /// Rising-edge threshold for the primary step.
    actuation_point: u8,
    /// Falling-edge threshold for the primary step (`< actuation_point`).
    release_point: u8,
    /// Optional deeper second step: `(threshold, release, key)`. `release` is
    /// `threshold - hysteresis`, precomputed at build time.
    second: Option<SecondStep>,

    /// Whether the primary step is currently emitting DOWN.
    primary_down: bool,
    /// Whether the second step is currently emitting DOWN.
    second_down: bool,
}

/// A resolved dual-step ("second function") configuration.
struct SecondStep {
    /// Logical key the second step maps to.
    key: KeyCode,
    /// Rising-edge threshold (the deeper actuation point).
    actuation_point: u8,
    /// Falling-edge threshold (`actuation_point - hysteresis`).
    release_point: u8,
}

impl KeyActuator {
    /// Apply this key's pressure for one frame, pushing any resulting
    /// `(KeyCode, value)` transitions onto `out`. Edge-triggered with hysteresis;
    /// emits every step crossed THIS frame (a fast press can cross both the
    /// primary and the second point between two frames, a fast release both back).
    fn apply(&mut self, pressure: u8, out: &mut Vec<(KeyCode, i32)>) {
        // Primary step.
        if self.primary_down {
            if pressure < self.release_point {
                self.primary_down = false;
                out.push((self.primary, VALUE_UP));
            }
        } else if pressure >= self.actuation_point {
            self.primary_down = true;
            out.push((self.primary, VALUE_DOWN));
        }

        // Second step (independent of the primary's emit; layered on top of it).
        if let Some(second) = &self.second {
            if self.second_down {
                if pressure < second.release_point {
                    self.second_down = false;
                    out.push((second.key, VALUE_UP));
                }
            } else if pressure >= second.actuation_point {
                self.second_down = true;
                out.push((second.key, VALUE_DOWN));
            }
        }
    }

    /// The down keys to release if the source tears down mid-press, so nothing
    /// stays stuck on the virtual device.
    fn held_keys(&self) -> impl Iterator<Item = KeyCode> + '_ {
        let primary = self.primary_down.then_some(self.primary);
        let second = self
            .second
            .as_ref()
            .filter(|_| self.second_down)
            .map(|s| s.key);
        primary.into_iter().chain(second)
    }
}

/// The pure actuation state machine over all configured analog keys.
///
/// Build it once from an [`AnalogConfig`]; feed it [`AnalogFrame`]s with
/// [`Actuator::on_frame`]. It owns each key's logical state so the daemon thread
/// stays a thin I/O loop. Indexing is by analog byte index (`AnalogFrame`'s
/// `pressures` order) — entries with an out-of-range index or an unparseable key
/// name are dropped at build time (warned), so `on_frame` is allocation-cheap.
pub struct Actuator {
    /// Byte-index → actuator. Only configured, valid indices are present.
    keys: Vec<(usize, KeyActuator)>,
}

/// Resolve a config entry's actuation parameters: parse the primary/second key
/// names, clamp the release point, and derive the second-step release.
/// Returns `None` (with a warning) if the byte index or primary key name is
/// invalid; an invalid second step is downgraded to no second step.
///
/// Shared by [`Actuator::from_config`] and the live `SetActuation` path so the
/// startup and runtime semantics are identical.
fn resolve_actuator(index: usize, key: &AnalogKey) -> Option<KeyActuator> {
    if index >= ANALOG_KEY_COUNT {
        tracing::warn!(
            index,
            max = ANALOG_KEY_COUNT,
            "analog key index out of range; skipping"
        );
        return None;
    }
    let primary = parse_key(&key.key).or_else(|| {
        tracing::warn!(name = %key.key, index, "analog key has unknown evdev name; skipping");
        None
    })?;

    // Anti-latch clamps. Release fires on `pressure < release_point` and
    // pressure is a u8, so a release_point of 0 can never fire: both points are
    // clamped to at least 1 (an actuation_point of 0 would otherwise press on
    // the first frame and latch down forever). Beyond that, release_point must
    // sit below actuation_point for hysteresis; if a user inverts them, clamp
    // release just under actuation so the key can still release.
    let actuation_point = key.actuation_point.max(1);
    let release_point = key
        .release_point
        .min(actuation_point.saturating_sub(1))
        .max(1);

    let second = match (key.second_actuation_point, key.second_action.as_deref()) {
        (Some(point), Some(name)) => match parse_key(name) {
            Some(second_key) => {
                let point = point.max(1);
                let hysteresis = actuation_point.saturating_sub(release_point);
                Some(SecondStep {
                    key: second_key,
                    actuation_point: point,
                    release_point: point.saturating_sub(hysteresis).max(1),
                })
            }
            None => {
                tracing::warn!(
                    name,
                    index,
                    "analog second_action has unknown evdev name; ignoring second step"
                );
                None
            }
        },
        (Some(_), None) => {
            tracing::warn!(
                index,
                "analog key sets second_actuation_point without second_action; ignoring second step"
            );
            None
        }
        _ => None,
    };

    Some(KeyActuator {
        primary,
        actuation_point,
        release_point,
        second,
        primary_down: false,
        second_down: false,
    })
}

impl Actuator {
    /// Build the actuator from the analog config. Invalid entries (bad byte
    /// index, unknown key name, a second step missing its key) are skipped with a
    /// warning rather than failing the daemon — analog stays best-effort.
    pub fn from_config(config: &AnalogConfig) -> Self {
        let mut keys = Vec::new();
        for (index_str, key) in &config.keys {
            // TOML table keys are strings; the analog map is keyed by the byte
            // index rendered as a string. Parse it back; a non-numeric key is a
            // config typo — warn and skip rather than failing the daemon.
            let Ok(index) = index_str.parse::<usize>() else {
                tracing::warn!(key = %index_str, "analog key index is not a number; skipping");
                continue;
            };
            if let Some(actuator) = resolve_actuator(index, key) {
                keys.push((index, actuator));
            }
        }
        // The config map is `BTreeMap<String, _>`, which iterates in
        // lexicographic STRING order ("10" < "2"); sort by the parsed numeric
        // index so `on_frame`'s documented byte-index emission order holds
        // (a same-frame modifier+letter crossing depends on it).
        keys.sort_by_key(|&(i, _)| i);
        tracing::info!(configured = keys.len(), "analog actuator built");
        Actuator { keys }
    }

    /// Apply a live actuation update (from `SetActuation`) to the running
    /// actuator: replace the thresholds for `index` in place, PRESERVING the
    /// current `primary_down`/`second_down` so a key held during an edit doesn't
    /// glitch. If the index isn't present yet (a freshly-configured key), it's
    /// inserted. Invalid params (out-of-range index, unknown key name) are a
    /// no-op (warned in `resolve_actuator`). Returns the key-up events for any
    /// step that was held but is no longer present after the update (e.g. the
    /// second step was removed while held) so nothing sticks on the sink.
    pub fn apply_update(&mut self, index: usize, key: &AnalogKey) -> Vec<(KeyCode, i32)> {
        let Some(mut next) = resolve_actuator(index, key) else {
            return Vec::new();
        };
        let mut releases = Vec::new();
        if let Some(slot) = self.keys.iter_mut().find(|(i, _)| *i == index) {
            let prev = &mut slot.1;
            // Carry the live held-state across the threshold swap.
            next.primary_down = prev.primary_down;
            // The primary key identity may have changed; if it did and was held,
            // release the OLD key (the new one re-actuates on the next frame).
            if prev.primary_down && prev.primary != next.primary {
                releases.push((prev.primary, VALUE_UP));
                next.primary_down = false;
            }
            // Second step: preserve held-state only if a second step still exists
            // AND maps to the same key; otherwise release the old held second key.
            match (&prev.second, &next.second) {
                (Some(ps), Some(ns)) if prev.second_down && ps.key == ns.key => {
                    next.second_down = true;
                }
                (Some(ps), _) if prev.second_down => {
                    releases.push((ps.key, VALUE_UP));
                }
                _ => {}
            }
            *prev = next;
        } else {
            // Keep `keys` sorted by byte index (see `from_config`) so a
            // freshly-inserted entry doesn't break `on_frame`'s ordering.
            let pos = self.keys.partition_point(|(i, _)| *i < index);
            self.keys.insert(pos, (index, next));
        }
        releases
    }

    /// The key codes this actuator can synthesize RIGHT NOW: every configured
    /// entry's primary code plus its second-step code where a valid second step
    /// exists. This is the single authority the hybrid drop set ([`ClaimedKeys`])
    /// is built from (via [`publish_claimed`], which additionally subtracts the
    /// thumb-area codes). Deriving from the BUILT actuator — not the raw config — is
    /// load-bearing: the set inherits every inclusion check
    /// [`Actuator::from_config`]/[`Actuator::apply_update`] perform, so an entry
    /// the actuator skipped (non-numeric or out-of-range index, unknown key
    /// name, `second_action` without its `second_actuation_point`) claims
    /// nothing, and its digital events keep flowing instead of being dropped
    /// with no analog replacement (a dead key).
    pub fn claimed_codes(&self) -> HashSet<u16> {
        self.keys
            .iter()
            .flat_map(|(_, k)| {
                std::iter::once(k.primary.0).chain(k.second.as_ref().map(|s| s.key.0))
            })
            .collect()
    }

    /// Run one frame through every configured key, returning the `(KeyCode,
    /// value)` transitions it produced (in byte-index order). An empty result
    /// means no key changed logical state this frame.
    pub fn on_frame(&mut self, frame: &AnalogFrame) -> Vec<(KeyCode, i32)> {
        let mut out = Vec::new();
        for (index, actuator) in &mut self.keys {
            actuator.apply(frame.pressures[*index], &mut out);
        }
        out
    }

    /// Every output key currently held down, as key-up `(KeyCode, value)` pairs.
    /// Used on teardown so a key down at unplug doesn't stick on the sink.
    pub fn release_all(&mut self) -> Vec<(KeyCode, i32)> {
        let mut out = Vec::new();
        for (_, actuator) in &mut self.keys {
            for code in actuator.held_keys() {
                out.push((code, VALUE_UP));
            }
            actuator.primary_down = false;
            actuator.second_down = false;
        }
        out
    }
}

/// A handle to the running analog source thread.
///
/// Hold it for as long as analog input should run. On shutdown call
/// [`AnalogSource::stop_and_join`], which signals the loop, waits for it to exit,
/// and thereby lets the [`AnalogReader`] drop on the source thread's own stack —
/// running its NORMAL-restore. The daemon MUST call this before `process::exit`,
/// since `exit` skips Drop.
pub struct AnalogSource {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl AnalogSource {
    /// Signal the loop to stop and join the thread, restoring NORMAL mode (the
    /// reader's Drop runs as the thread unwinds its stack). Returns once the
    /// device is back to NORMAL (or the reader's Drop has exhausted its recovery).
    pub fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for AnalogSource {
    fn drop(&mut self) {
        // If the source was dropped without an explicit stop (e.g. an early `?`
        // return during setup before the daemon takes ownership), still signal +
        // join so the reader's Drop runs and NORMAL is restored.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Spawn the analog source: open the [`AnalogReader`] (entering DRIVER mode) and
/// run the poll → actuate → forward loop on its own thread.
///
/// The reader is opened ON the spawned thread so it is OWNED there — that is what
/// makes the cooperative-join shutdown restore NORMAL deterministically (the
/// reader drops on the thread's stack when the loop exits). Opening can fail
/// (device missing, permissions, control busy); that failure is reported through
/// `ready_rx`, so the caller learns of it synchronously instead of the daemon
/// silently running with no input.
///
/// `armed` is a process-global flag set true just before the reader enters
/// DRIVER mode (pre-armed, so the panic-hook net covers the open itself) and
/// cleared once NORMAL is restored; the daemon's panic hook reads it to decide
/// whether to send a best-effort NORMAL before aborting. It is a `&'static`
/// (not an `Arc`) precisely so the panic hook — which can fire from any thread at
/// any time — can read the very same flag without a shared owner.
///
/// `snapshot` is the live pressure/active view the IPC handler reads for
/// `GetAnalogState`; the loop writes each frame into it and sets `active` for the
/// source's lifetime. `updates` carries live `SetActuation` edits the loop drains
/// each poll and applies to the running actuator in place. `claimed` is the
/// shared drop set for the hybrid digital readers; it is seeded here (before the
/// worker even starts) and republished by the loop after every applied update.
pub fn spawn(
    config: AnalogConfig,
    tx: Sender<EngineMsg>,
    armed: &'static AtomicBool,
    snapshot: Arc<AnalogSnapshot>,
    updates: Receiver<ActuationUpdate>,
    claimed: ClaimedKeys,
) -> Result<AnalogSource, tartarust_hid::HidError> {
    let stop = Arc::new(AtomicBool::new(false));
    let poll_timeout = Duration::from_millis(config.poll_interval_ms.max(1));

    // Build the actuator HERE (caller's thread), not on the worker: the claimed
    // set must be seeded before `spawn` returns, because `main` spawns the
    // digital readers right after — a reader must never run against an unseeded
    // set (it would forward a digital dupe of an analog key). Seeding from the
    // BUILT actuator makes the set exactly mirror what can be synthesized; see
    // [`Actuator::claimed_codes`].
    let mut actuator = Actuator::from_config(&config);
    publish_claimed(&actuator, &claimed);

    // Open the reader on a oneshot so `spawn` returns the open Result while the
    // reader stays owned by the worker thread (the ownership is what guarantees
    // its Drop — and the NORMAL restore — runs on cooperative join).
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), tartarust_hid::HidError>>();
    let thread_stop = stop.clone();
    let thread_armed = armed;
    let thread_snapshot = snapshot;

    let join = thread::Builder::new()
        .name("tartarust-analog".into())
        .spawn(move || {
            // Pre-arm the panic hook BEFORE open(): open() sends the DRIVER
            // command internally, so arming only after it returns would leave a
            // window where a panic on any other thread aborts (skipping every
            // Drop) with DRIVER active but `armed` false — the hook would skip
            // its NORMAL restore and the keypad would stay mute until replug.
            // Over-arming is safe (the hook's NORMAL send is idempotent and
            // harmless if DRIVER was never entered); under-arming is not. This
            // narrows rather than fully closes the race — the hook's NORMAL and
            // an in-flight DRIVER command are unordered — but the exposed window
            // is now the USB round-trip itself, not the whole open().
            thread_armed.store(true, Ordering::SeqCst);
            let mut reader = match AnalogReader::open() {
                Ok(r) => {
                    // Mark active only once DRIVER is held, so GetAnalogState
                    // reflects "source running" rather than "ever attempted".
                    thread_snapshot.active.store(true, Ordering::SeqCst);
                    let _ = ready_tx.send(Ok(()));
                    r
                }
                Err(e) => {
                    // open() failed: its internal guard already restored NORMAL
                    // on the way out (or DRIVER was never entered) — disarm.
                    thread_armed.store(false, Ordering::SeqCst);
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            tracing::info!(node = %reader.input_path().display(), "analog source running (DRIVER mode)");

            run_loop(
                &mut reader,
                &mut actuator,
                &tx,
                &thread_stop,
                poll_timeout,
                &thread_snapshot,
                &updates,
                &claimed,
            );

            // The source is exiting: un-claim every code so the still-running
            // hybrid digital readers stop dropping them. This matters on the
            // error exit (a hidraw failure while the evdev nodes stay alive —
            // e.g. an interface-level unbind): the daemon keeps serving, the
            // reader drop below restores NORMAL, and the grid resumes digital
            // 6KRO reports — with a stale set, every analog-configured key
            // would stay silently dead until daemon restart. Cleared BEFORE
            // the reader drop so no digital event can race a stale set; no
            // double-feed is possible, since this thread synthesizes nothing
            // anymore. (On clean shutdown the process exits right after, so
            // clearing is harmless.)
            if let Ok(mut set) = claimed.write() {
                set.clear();
            }

            // Clear `active` so a post-shutdown GetAnalogState reads inactive.
            // Order vs the reader drop doesn't affect safety (active is advisory
            // UI state, not the NORMAL-restore invariant — that's `armed`).
            thread_snapshot.active.store(false, Ordering::SeqCst);

            // Dropping `reader` here (end of thread) restores NORMAL via the
            // guard's Drop. Clear `armed` only AFTER the drop returns, so the
            // panic-hook net covers the ENTIRE DRIVER-active window — including
            // the guard's own NORMAL send. If another thread panics during that
            // send, the hook still fires a best-effort NORMAL (idempotent with the
            // guard's, so harmless); clearing earlier would leave a narrow window
            // where an abort could strand the device in DRIVER.
            drop(reader);
            thread_armed.store(false, Ordering::SeqCst);
            tracing::debug!("analog source thread exiting");
        })
        .expect("spawning analog source thread");

    // Wait for the open Result. The thread always sends exactly once before doing
    // any work, so this resolves promptly.
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(AnalogSource {
            stop,
            join: Some(join),
        }),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            // Thread vanished before reporting — treat as a join and surface a
            // generic failure (should not happen; the thread always reports).
            let _ = join.join();
            Err(tartarust_hid::HidError::DeviceNotFound)
        }
    }
}

/// The poll → actuate → forward loop. Extracted so it reads as one unit; runs
/// until `stop` is set, the engine channel closes, or the reader errors. Each
/// iteration also drains live `SetActuation` updates (republishing `claimed`
/// when any applied) and publishes the latest frame into `snapshot` for
/// `GetAnalogState`.
#[allow(clippy::too_many_arguments)]
fn run_loop(
    reader: &mut AnalogReader,
    actuator: &mut Actuator,
    tx: &Sender<EngineMsg>,
    stop: &AtomicBool,
    poll_timeout: Duration,
    snapshot: &AnalogSnapshot,
    updates: &Receiver<ActuationUpdate>,
    claimed: &ClaimedKeys,
) {
    while !stop.load(Ordering::SeqCst) {
        // Apply any live actuation edits before this poll. A threshold change can
        // orphan a held step (different key / removed second step); forward those
        // releases so nothing sticks on the sink.
        let mut applied_any = false;
        while let Ok(update) = updates.try_recv() {
            for (code, value) in actuator.apply_update(update.index, &update.key) {
                if tx.send(EngineMsg::Input(code.0, value)).is_err() {
                    return; // engine gone
                }
            }
            applied_any = true;
        }
        if applied_any {
            // Republish the claimed-key drop set: `SetActuation` CAN change or
            // remove a `second_action` key name, and the digital readers must
            // stop dropping a code the actuator no longer synthesizes (else that
            // key is dead until restart) — and start dropping a newly claimed
            // one. Recomputing from the actuator (the authority) is idempotent,
            // so a rejected/no-op update republishing is harmless. The readers'
            // down-edge latching keeps a key held ACROSS this swap consistent.
            publish_claimed(actuator, claimed);
        }

        match reader.poll(poll_timeout) {
            Ok(Some(frame)) => {
                // Publish the raw frame for the live pressure view first, then
                // derive key events. The snapshot holds the last frame across
                // idle gaps because the device only emits on change.
                snapshot.store_frame(&frame);
                for (code, value) in actuator.on_frame(&frame) {
                    if tx.send(EngineMsg::Input(code.0, value)).is_err() {
                        return; // engine gone
                    }
                }
            }
            // Idle/timeout/EINTR: loop and re-check `stop`. The device goes silent
            // when pressure is steady, so this is the common case — no busy-spin
            // because `poll` blocked up to `poll_timeout`.
            Ok(None) => {}
            Err(err) => {
                // The node went away (unplug) or read failed. Mirror the digital
                // reader: ask the engine to release any keys this source was
                // holding so none stay stuck-down, then stop. (Clean shutdown via
                // `stop` skips this — the keys are released by the engine teardown
                // and the sink dies with the process.)
                tracing::error!(?err, "analog read error; source stopping");
                let releases = actuator.release_all();
                for (code, value) in releases {
                    let _ = tx.send(EngineMsg::Input(code.0, value));
                }
                let _ = tx.send(EngineMsg::ReleaseAll);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AnalogKey, DEFAULT_ACTUATION_POINT, DEFAULT_HYSTERESIS};
    use std::collections::BTreeMap;

    /// A frame with `pressure` at byte `index` and zero everywhere else.
    fn frame_at(index: usize, pressure: u8) -> AnalogFrame {
        let mut pressures = [0u8; ANALOG_KEY_COUNT];
        pressures[index] = pressure;
        AnalogFrame { pressures }
    }

    /// Every entry of the built-in analog map must resolve to a live actuator:
    /// index in range, evdev name known. Guards the shipped map against typos —
    /// `resolve_actuator` only warns and skips a bad entry, which here would
    /// silently kill a grid key (DRIVER mode silences the grid digitally).
    #[test]
    fn builtin_map_resolves_every_actuator() {
        let keys = crate::state::default_analog_keys();
        assert_eq!(keys.len(), 20);
        for (index_str, key) in &keys {
            let index: usize = index_str.parse().expect("built-in index is numeric");
            assert!(
                resolve_actuator(index, key).is_some(),
                "built-in map entry {index} ({}) must resolve",
                key.key
            );
        }
    }

    /// Single-key analog config: byte `index` -> `key`, default thresholds.
    fn one_key(index: u8, key: &str) -> AnalogConfig {
        let mut keys = BTreeMap::new();
        keys.insert(
            index.to_string(),
            AnalogKey {
                key: key.into(),
                actuation_point: DEFAULT_ACTUATION_POINT,
                release_point: DEFAULT_ACTUATION_POINT - DEFAULT_HYSTERESIS,
                second_actuation_point: None,
                second_action: None,
            },
        );
        AnalogConfig {
            poll_interval_ms: 50,
            keys,
        }
    }

    /// Dual-step config: byte `index` primary -> `key`, deeper -> `second`.
    fn dual_step(index: u8, key: &str, second: &str) -> AnalogConfig {
        let mut keys = BTreeMap::new();
        keys.insert(
            index.to_string(),
            AnalogKey {
                key: key.into(),
                actuation_point: 0x60,
                release_point: 0x50, // hysteresis margin 0x10
                second_actuation_point: Some(0xC0),
                second_action: Some(second.into()),
            },
        );
        AnalogConfig {
            poll_interval_ms: 50,
            keys,
        }
    }

    #[test]
    fn rising_across_actuation_emits_down() {
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        // Below actuation: nothing.
        assert!(a.on_frame(&frame_at(5, 0x40)).is_empty());
        // Cross the actuation point: KEY_Q down.
        assert_eq!(
            a.on_frame(&frame_at(5, DEFAULT_ACTUATION_POINT)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
    }

    #[test]
    fn falling_below_release_emits_up_with_hysteresis() {
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        a.on_frame(&frame_at(5, 0xFF)); // press fully -> down
        // Drop to just BELOW actuation but ABOVE release: still held (hysteresis).
        let between = DEFAULT_ACTUATION_POINT - 1; // above release (act - 0x10)
        assert!(
            a.on_frame(&frame_at(5, between)).is_empty(),
            "between release and actuation must NOT release (hysteresis)"
        );
        // Drop below the release point: KEY_Q up.
        let below = DEFAULT_ACTUATION_POINT - DEFAULT_HYSTERESIS - 1;
        assert_eq!(
            a.on_frame(&frame_at(5, below)),
            vec![(KeyCode::KEY_Q, VALUE_UP)]
        );
    }

    #[test]
    fn no_chatter_around_threshold() {
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        // Hover right at the actuation point and one below it repeatedly. After
        // the initial down, oscillating in the hysteresis band emits nothing.
        assert_eq!(
            a.on_frame(&frame_at(5, DEFAULT_ACTUATION_POINT)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
        for _ in 0..10 {
            // Within [release_point, actuation_point): no events either way.
            assert!(a.on_frame(&frame_at(5, DEFAULT_ACTUATION_POINT)).is_empty());
            assert!(
                a.on_frame(&frame_at(5, DEFAULT_ACTUATION_POINT - 1))
                    .is_empty()
            );
        }
    }

    #[test]
    fn held_key_stays_down_across_idle_gap() {
        // The device emits nothing while pressure is steady. Between two frames
        // that both show the key held, no frame arrives at all — the actuator
        // simply isn't called, and its state persists. We model "no frame" as the
        // absence of an on_frame call; the key must remain logically down.
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        assert_eq!(
            a.on_frame(&frame_at(5, 0xFF)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
        // ... a long idle gap with zero frames ...
        // The next frame that DOES arrive still shows it held: no new event
        // (already down), and crucially no spurious release happened in between.
        assert!(a.on_frame(&frame_at(5, 0xFF)).is_empty());
        // Releasing later still works.
        assert_eq!(
            a.on_frame(&frame_at(5, 0x00)),
            vec![(KeyCode::KEY_Q, VALUE_UP)]
        );
    }

    #[test]
    fn dual_step_crossing_emits_both_in_order() {
        let mut a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTSHIFT"));
        // Light press: primary only.
        assert_eq!(
            a.on_frame(&frame_at(5, 0x60)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
        // Press deeper past the second point: second fires, primary stays held.
        assert_eq!(
            a.on_frame(&frame_at(5, 0xC0)),
            vec![(KeyCode::KEY_LEFTSHIFT, VALUE_DOWN)]
        );
        // Ease back above the second release point but still deep: nothing.
        assert!(a.on_frame(&frame_at(5, 0xB8)).is_empty());
        // Below second release (0xC0 - 0x10 = 0xB0): second up, primary held.
        assert_eq!(
            a.on_frame(&frame_at(5, 0xAF)),
            vec![(KeyCode::KEY_LEFTSHIFT, VALUE_UP)]
        );
        // Full release: primary up.
        assert_eq!(
            a.on_frame(&frame_at(5, 0x00)),
            vec![(KeyCode::KEY_Q, VALUE_UP)]
        );
    }

    #[test]
    fn fast_press_crosses_both_steps_in_one_frame() {
        // A press fast enough that the device's next emitted frame already shows
        // pressure past BOTH points: primary-down AND second-down, same frame.
        let mut a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTSHIFT"));
        assert_eq!(
            a.on_frame(&frame_at(5, 0xFF)),
            vec![
                (KeyCode::KEY_Q, VALUE_DOWN),
                (KeyCode::KEY_LEFTSHIFT, VALUE_DOWN)
            ]
        );
    }

    #[test]
    fn fast_release_drops_both_steps_in_one_frame() {
        let mut a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTSHIFT"));
        a.on_frame(&frame_at(5, 0xFF)); // both down
        // One frame straight to zero: both release this frame, second before primary
        // is fine — order is "second step checked after primary" so primary-up first.
        let out = a.on_frame(&frame_at(5, 0x00));
        assert!(out.contains(&(KeyCode::KEY_Q, VALUE_UP)));
        assert!(out.contains(&(KeyCode::KEY_LEFTSHIFT, VALUE_UP)));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn multiple_keys_independent() {
        let mut keys = BTreeMap::new();
        keys.insert(
            "3".to_string(),
            AnalogKey {
                key: "KEY_A".into(),
                actuation_point: DEFAULT_ACTUATION_POINT,
                release_point: DEFAULT_ACTUATION_POINT - DEFAULT_HYSTERESIS,
                second_actuation_point: None,
                second_action: None,
            },
        );
        keys.insert(
            "7".to_string(),
            AnalogKey {
                key: "KEY_B".into(),
                actuation_point: DEFAULT_ACTUATION_POINT,
                release_point: DEFAULT_ACTUATION_POINT - DEFAULT_HYSTERESIS,
                second_actuation_point: None,
                second_action: None,
            },
        );
        let mut a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });
        // Press both in one frame.
        let mut pressures = [0u8; ANALOG_KEY_COUNT];
        pressures[3] = 0xFF;
        pressures[7] = 0xFF;
        let out = a.on_frame(&AnalogFrame { pressures });
        assert!(out.contains(&(KeyCode::KEY_A, VALUE_DOWN)));
        assert!(out.contains(&(KeyCode::KEY_B, VALUE_DOWN)));
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn eight_keys_in_one_frame_all_actuate_then_release() {
        // Actuator NKRO regression guard: a single frame with 8+ distinct byte
        // indices all fully pressed must emit a DOWN for every one of them (no
        // rollover cap), and a single all-zero frame must release all of them.
        // Eight distinct keys (so each is its own transition; shared keys would
        // refcount-merge and the count would lie).
        //
        // SCOPE: this proves the analog Actuator is structurally NKRO (one byte
        // per key, no array width to overflow) — it does NOT exercise the digital
        // device-node-selection fix (`input::plan_grabs`), which is a different code
        // path. Analog mode bypasses HID rollover entirely; this guards that the
        // actuator stays per-key independent.
        const INDICES: [(u8, &str); 8] = [
            (0, "KEY_1"),
            (1, "KEY_2"),
            (2, "KEY_3"),
            (3, "KEY_4"),
            (4, "KEY_5"),
            (5, "KEY_6"),
            (6, "KEY_7"),
            (7, "KEY_8"),
        ];
        let mut keys = BTreeMap::new();
        for (index, name) in INDICES {
            keys.insert(
                index.to_string(),
                AnalogKey {
                    key: name.into(),
                    actuation_point: DEFAULT_ACTUATION_POINT,
                    release_point: DEFAULT_ACTUATION_POINT - DEFAULT_HYSTERESIS,
                    second_actuation_point: None,
                    second_action: None,
                },
            );
        }
        let mut a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });

        // One frame, all eight bytes fully pressed -> eight DOWNs.
        let mut pressures = [0u8; ANALOG_KEY_COUNT];
        for (index, _) in INDICES {
            pressures[index as usize] = 0xFF;
        }
        let down = a.on_frame(&AnalogFrame { pressures });
        assert_eq!(down.len(), 8, "all 8 keys must actuate in one frame (NKRO)");
        for (_, name) in INDICES {
            let code = name.parse::<KeyCode>().unwrap();
            assert!(
                down.contains(&(code, VALUE_DOWN)),
                "{name} missing from the 8-key DOWN frame"
            );
        }

        // One frame, all back to zero -> eight UPs, no stuck keys.
        let up = a.on_frame(&AnalogFrame {
            pressures: [0u8; ANALOG_KEY_COUNT],
        });
        assert_eq!(up.len(), 8, "all 8 keys must release in one frame");
        for (_, name) in INDICES {
            let code = name.parse::<KeyCode>().unwrap();
            assert!(
                up.contains(&(code, VALUE_UP)),
                "{name} missing from the 8-key UP frame"
            );
        }
    }

    #[test]
    fn release_all_emits_up_for_held_keys() {
        let mut a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTSHIFT"));
        a.on_frame(&frame_at(5, 0xFF)); // primary + second down
        let mut got = a.release_all();
        got.sort_by_key(|(c, _)| c.0);
        assert!(got.contains(&(KeyCode::KEY_Q, VALUE_UP)));
        assert!(got.contains(&(KeyCode::KEY_LEFTSHIFT, VALUE_UP)));
        // After release_all, state is clear: a second call emits nothing.
        assert!(a.release_all().is_empty());
    }

    #[test]
    fn out_of_range_index_skipped() {
        // ANALOG_KEY_COUNT == 23, so index 99 is invalid and must be dropped at
        // build time (no panic when frames arrive).
        let a = Actuator::from_config(&one_key(99, "KEY_Q"));
        assert_eq!(a.keys.len(), 0);
    }

    #[test]
    fn unknown_key_name_skipped() {
        let a = Actuator::from_config(&one_key(5, "KEY_DEFINITELY_NOT_A_KEY"));
        assert_eq!(a.keys.len(), 0);
    }

    #[test]
    fn inverted_release_point_is_clamped_not_latched() {
        // A user who sets release_point >= actuation_point would otherwise latch
        // the key down forever. We clamp release just under actuation so it can
        // still release.
        let mut keys = BTreeMap::new();
        keys.insert(
            "5".to_string(),
            AnalogKey {
                key: "KEY_Q".into(),
                actuation_point: 0x60,
                release_point: 0x80, // invalid: above actuation
                second_actuation_point: None,
                second_action: None,
            },
        );
        let mut a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });
        assert_eq!(
            a.on_frame(&frame_at(5, 0x60)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
        // Drops below the clamped release (0x5F): releases.
        assert_eq!(
            a.on_frame(&frame_at(5, 0x00)),
            vec![(KeyCode::KEY_Q, VALUE_UP)]
        );
    }

    #[test]
    fn zero_actuation_point_is_clamped_not_latched() {
        // actuation_point = 0 would press on the first frame (any pressure >= 0)
        // and never release (pressure is a u8, so it can't go below a clamped
        // release_point of 0). Both points are clamped to at least 1.
        let mut keys = BTreeMap::new();
        keys.insert(
            "5".to_string(),
            AnalogKey {
                key: "KEY_Q".into(),
                actuation_point: 0,
                release_point: 0,
                second_actuation_point: Some(0),
                second_action: Some("KEY_W".into()),
            },
        );
        let mut a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });
        // An untouched key (pressure 0) must NOT actuate.
        assert_eq!(a.on_frame(&frame_at(5, 0x00)), vec![]);
        // Any real pressure crosses both clamped points (1)...
        assert_eq!(
            a.on_frame(&frame_at(5, 0x40)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN), (KeyCode::KEY_W, VALUE_DOWN)]
        );
        // ...and a full release (pressure 0 < clamped release 1) lets go again.
        assert_eq!(
            a.on_frame(&frame_at(5, 0x00)),
            vec![(KeyCode::KEY_Q, VALUE_UP), (KeyCode::KEY_W, VALUE_UP)]
        );
    }

    #[test]
    fn second_point_without_action_ignored() {
        let mut keys = BTreeMap::new();
        keys.insert(
            "5".to_string(),
            AnalogKey {
                key: "KEY_Q".into(),
                actuation_point: 0x60,
                release_point: 0x50,
                second_actuation_point: Some(0xC0),
                second_action: None, // missing -> second step ignored
            },
        );
        let mut a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });
        // Full press: only the primary fires (no second step configured).
        assert_eq!(
            a.on_frame(&frame_at(5, 0xFF)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
    }

    // ---- live SetActuation updates (apply_update) ----

    fn key_cfg(key: &str, act: u8, rel: u8) -> AnalogKey {
        AnalogKey {
            key: key.into(),
            actuation_point: act,
            release_point: rel,
            second_actuation_point: None,
            second_action: None,
        }
    }

    #[test]
    fn apply_update_changes_thresholds_in_place() {
        // Start with actuation 0x60; a pressure of 0x50 must NOT actuate.
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        assert!(a.on_frame(&frame_at(5, 0x50)).is_empty());
        // Lower the actuation point to 0x40 live; now 0x50 actuates.
        let rel = a.apply_update(5, &key_cfg("KEY_Q", 0x40, 0x30));
        assert!(rel.is_empty());
        assert_eq!(
            a.on_frame(&frame_at(5, 0x50)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
    }

    #[test]
    fn apply_update_preserves_held_state_across_threshold_edit() {
        // Hold the key down, then edit thresholds (same key). The held state must
        // carry across so we don't emit a spurious re-press, and a later release
        // still fires exactly one key-up.
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        assert_eq!(
            a.on_frame(&frame_at(5, 0xFF)),
            vec![(KeyCode::KEY_Q, VALUE_DOWN)]
        );
        // Edit thresholds while held: no events, key stays logically down.
        assert!(a.apply_update(5, &key_cfg("KEY_Q", 0x40, 0x30)).is_empty());
        // Still held: a frame above the new release emits nothing.
        assert!(a.on_frame(&frame_at(5, 0xFF)).is_empty());
        // Drop below the NEW release point (0x30): exactly one key-up.
        assert_eq!(
            a.on_frame(&frame_at(5, 0x00)),
            vec![(KeyCode::KEY_Q, VALUE_UP)]
        );
    }

    #[test]
    fn apply_update_releases_old_key_when_primary_changes_while_held() {
        // If the primary key identity changes while the key is held, the OLD key
        // must be released (no stuck key); the new one re-actuates next frame.
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        a.on_frame(&frame_at(5, 0xFF)); // KEY_Q down
        let rel = a.apply_update(5, &key_cfg("KEY_W", 0x60, 0x50));
        assert_eq!(rel, vec![(KeyCode::KEY_Q, VALUE_UP)]);
        // The new key re-actuates on the next frame (it was not carried as held).
        assert_eq!(
            a.on_frame(&frame_at(5, 0xFF)),
            vec![(KeyCode::KEY_W, VALUE_DOWN)]
        );
    }

    #[test]
    fn apply_update_releases_held_second_step_when_removed() {
        // Hold both steps, then remove the second step via an update: the held
        // second key must be released so it doesn't stick.
        let mut a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTSHIFT"));
        a.on_frame(&frame_at(5, 0xFF)); // primary + second down
        let rel = a.apply_update(5, &key_cfg("KEY_Q", 0x60, 0x50)); // no second step
        assert_eq!(rel, vec![(KeyCode::KEY_LEFTSHIFT, VALUE_UP)]);
        // Primary stays held (carried across); full release fires only KEY_Q up.
        assert_eq!(
            a.on_frame(&frame_at(5, 0x00)),
            vec![(KeyCode::KEY_Q, VALUE_UP)]
        );
    }

    #[test]
    fn apply_update_inserts_new_index() {
        // An update for an index not yet present is inserted (a freshly mapped
        // key), so it actuates immediately.
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        assert!(a.apply_update(8, &key_cfg("KEY_W", 0x60, 0x50)).is_empty());
        assert_eq!(
            a.on_frame(&frame_at(8, 0xFF)),
            vec![(KeyCode::KEY_W, VALUE_DOWN)]
        );
    }

    #[test]
    fn apply_update_invalid_index_is_noop() {
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        assert!(a.apply_update(99, &key_cfg("KEY_W", 0x60, 0x50)).is_empty());
        assert_eq!(a.keys.len(), 1); // unchanged
    }

    // ---- claimed_codes: the hybrid drop-set authority ----

    #[test]
    fn claimed_codes_covers_primary_and_valid_second_action() {
        let a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTSHIFT"));
        let set = a.claimed_codes();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&KeyCode::KEY_Q.0));
        assert!(set.contains(&KeyCode::KEY_LEFTSHIFT.0));
    }

    /// Every entry the actuator SKIPS must claim nothing: a claimed-but-never-
    /// synthesized code would make the hybrid digital readers drop a key that
    /// has no analog replacement — a silently dead key (review finding). The
    /// adversarial cases: `second_action` without `second_actuation_point` (the
    /// thumb button's `KEY_LEFTALT` would go dead — its ONLY emission path in
    /// analog mode is the digital boot node), an out-of-range index, a
    /// non-numeric index, and unparseable names.
    #[test]
    fn claimed_codes_excludes_entries_the_actuator_skips() {
        let mut keys = BTreeMap::new();
        // second_action WITHOUT its point: second step ignored -> KEY_LEFTALT
        // must NOT be claimed (KEY_Q still is).
        keys.insert(
            "0".to_string(),
            AnalogKey {
                key: "KEY_Q".into(),
                actuation_point: 0x60,
                release_point: 0x50,
                second_actuation_point: None,
                second_action: Some("KEY_LEFTALT".into()),
            },
        );
        // Out-of-range index: whole entry skipped -> KEY_SPACE not claimed.
        keys.insert("99".to_string(), key_cfg("KEY_SPACE", 0x60, 0x50));
        // Non-numeric index: whole entry skipped -> KEY_A not claimed.
        keys.insert("grid".to_string(), key_cfg("KEY_A", 0x60, 0x50));
        // Unparseable primary: whole entry skipped.
        keys.insert("1".to_string(), key_cfg("KEY_NOPE", 0x60, 0x50));
        // Unparseable second_action: second step ignored -> only KEY_B claimed.
        keys.insert(
            "2".to_string(),
            AnalogKey {
                key: "KEY_B".into(),
                actuation_point: 0x60,
                release_point: 0x50,
                second_actuation_point: Some(0xC0),
                second_action: Some("KEY_ALSO_NOPE".into()),
            },
        );
        let a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });
        let set = a.claimed_codes();
        assert_eq!(
            set,
            HashSet::from([KeyCode::KEY_Q.0, KeyCode::KEY_B.0]),
            "only synthesizable codes may be claimed; got {set:?}"
        );
    }

    /// The claimed set must track live `SetActuation` edits (the run loop
    /// republishes `claimed_codes` after each applied update): changing or
    /// removing a `second_action` un-claims the old code, and adding one claims
    /// the new code.
    #[test]
    fn claimed_codes_follows_apply_update() {
        let mut a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTALT"));
        assert!(a.claimed_codes().contains(&KeyCode::KEY_LEFTALT.0));

        // Swap the second action LEFTALT -> LEFTSHIFT.
        a.apply_update(
            5,
            &AnalogKey {
                key: "KEY_Q".into(),
                actuation_point: 0x60,
                release_point: 0x50,
                second_actuation_point: Some(0xC0),
                second_action: Some("KEY_LEFTSHIFT".into()),
            },
        );
        let set = a.claimed_codes();
        assert!(
            !set.contains(&KeyCode::KEY_LEFTALT.0),
            "old second unclaimed"
        );
        assert!(
            set.contains(&KeyCode::KEY_LEFTSHIFT.0),
            "new second claimed"
        );

        // Remove the second step entirely: only the primary stays claimed.
        a.apply_update(5, &key_cfg("KEY_Q", 0x60, 0x50));
        assert_eq!(a.claimed_codes(), HashSet::from([KeyCode::KEY_Q.0]));
    }

    /// The PUBLISHED drop set must exclude the thumb-area codes: in DRIVER
    /// mode those codes can only arrive digitally from a DIFFERENT physical
    /// key (the thumb button / thumbpad arrows, whose sole emission path is
    /// the boot node), so claiming them would kill that physical key —
    /// deterministically and silently. Ordinary claims still publish.
    #[test]
    fn publish_claimed_excludes_thumb_area_codes() {
        // `second_action = "KEY_LEFTALT"` — the natural "deep press = Alt
        // modifier" config that collides with the thumb button's code.
        let a = Actuator::from_config(&dual_step(5, "KEY_Q", "KEY_LEFTALT"));
        let claimed: ClaimedKeys = Arc::new(RwLock::new(HashSet::new()));
        publish_claimed(&a, &claimed);
        let set = claimed.read().unwrap();
        assert!(
            set.contains(&KeyCode::KEY_Q.0),
            "ordinary claim must publish"
        );
        assert!(
            !set.contains(&KeyCode::KEY_LEFTALT.0),
            "thumb-area code must never be published (it would kill the \
             physical thumb button)"
        );
    }

    /// A thumbpad-arrow collision on the PRIMARY key is excluded the same way,
    /// and the spacebar — the one genuine same-physical-key dupe — stays
    /// claimed.
    #[test]
    fn publish_claimed_excludes_arrows_but_keeps_space() {
        let mut keys = BTreeMap::new();
        keys.insert("3".to_string(), key_cfg("KEY_UP", 0x60, 0x50));
        keys.insert("19".to_string(), key_cfg("KEY_SPACE", 0x60, 0x50));
        let a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });
        let claimed: ClaimedKeys = Arc::new(RwLock::new(HashSet::new()));
        publish_claimed(&a, &claimed);
        let set = claimed.read().unwrap();
        assert_eq!(*set, HashSet::from([KeyCode::KEY_SPACE.0]));
    }

    // ---- on_frame emission order (byte-index, not lexicographic) ----

    /// `BTreeMap<String, _>` iterates `"10"` before `"2"`; the actuator must
    /// sort by the numeric byte index so a same-frame crossing emits byte 2
    /// (here a modifier) before byte 10 (a letter) — the documented order.
    #[test]
    fn on_frame_emits_in_byte_index_order_across_digit_widths() {
        let mut keys = BTreeMap::new();
        keys.insert("10".to_string(), key_cfg("KEY_A", 0x60, 0x50));
        keys.insert("2".to_string(), key_cfg("KEY_LEFTSHIFT", 0x60, 0x50));
        let mut a = Actuator::from_config(&AnalogConfig {
            poll_interval_ms: 50,
            keys,
        });
        let mut pressures = [0u8; ANALOG_KEY_COUNT];
        pressures[2] = 0xFF;
        pressures[10] = 0xFF;
        assert_eq!(
            a.on_frame(&AnalogFrame { pressures }),
            vec![
                (KeyCode::KEY_LEFTSHIFT, VALUE_DOWN),
                (KeyCode::KEY_A, VALUE_DOWN)
            ],
            "byte 2 must emit before byte 10 (byte-index order)"
        );
    }

    /// A live insert via `apply_update` keeps the byte-index order.
    #[test]
    fn apply_update_insert_keeps_byte_index_order() {
        let mut a = Actuator::from_config(&one_key(5, "KEY_Q"));
        assert!(
            a.apply_update(2, &key_cfg("KEY_LEFTSHIFT", 0x60, 0x50))
                .is_empty()
        );
        let mut pressures = [0u8; ANALOG_KEY_COUNT];
        pressures[2] = 0xFF;
        pressures[5] = 0xFF;
        assert_eq!(
            a.on_frame(&AnalogFrame { pressures }),
            vec![
                (KeyCode::KEY_LEFTSHIFT, VALUE_DOWN),
                (KeyCode::KEY_Q, VALUE_DOWN)
            ],
            "an inserted lower index must emit before existing higher indices"
        );
    }

    #[test]
    fn snapshot_holds_last_frame_and_active_flag() {
        let snap = AnalogSnapshot::new();
        assert!(!snap.is_active());
        assert_eq!(snap.pressures(), vec![0u8; ANALOG_KEY_COUNT]);
        snap.store_frame(&frame_at(3, 0x7F));
        let p = snap.pressures();
        assert_eq!(p[3], 0x7F);
        assert_eq!(p.len(), ANALOG_KEY_COUNT);
    }
}
