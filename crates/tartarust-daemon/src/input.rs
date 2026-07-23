//! Physical device discovery and grabbing.
//!
//! The Tartarus Pro (USB `1532:0244`) presents more than one `/dev/input`
//! event node — a keyboard node and a mouse/thumbpad node. We enumerate
//! `/dev/input`, keep the ones whose USB ids match, and grab each so the OS
//! stops seeing the raw keys (`EVIOCGRAB`). The daemon then re-emits remapped
//! events through the virtual device built in [`crate::output`].

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use evdev::{Device, KeyCode, RelativeAxisCode};

use tartarust_protocol::{PRODUCT_ID, VENDOR_ID};

/// USB `bInterfaceNumber` of the Tartarus Pro's **boot** keyboard — a classic
/// 6-key HID array (6-key rollover). Reading this node caps simultaneous key
/// presses at 6.
pub const BOOT_INTERFACE: u8 = 0x00;
/// USB `bInterfaceNumber` of the Tartarus Pro's **real** keyboard — a 22-slot
/// keycode array (full N-key rollover). This is the node to read for NKRO.
pub const NKRO_INTERFACE: u8 = 0x01;

/// The single source of truth for "is this evdev device the Tartarus Pro?".
///
/// Every place that decides to open, grab, or read a physical node MUST go
/// through this — there is no other vendor/product test in the daemon. A device
/// is the Tartarus Pro only if BOTH its USB ids match exactly (`1532:0244`); a
/// vendor-only match (any other Razer device, e.g. a Razer keyboard or mouse)
/// is explicitly rejected.
pub fn is_tartarus(device: &Device) -> bool {
    let id = device.input_id();
    id.vendor() == VENDOR_ID && id.product() == PRODUCT_ID
}

/// A discovered Tartarus Pro input node, ungrabbed.
pub struct DeviceNode {
    /// The `/dev/input/eventN` path.
    pub path: PathBuf,
    /// The opened (not yet grabbed) device.
    pub device: Device,
    /// What the node looks like — used only for logging and for `--discover`.
    pub kind: NodeKind,
    /// The USB `bInterfaceNumber` backing this evdev node, resolved from sysfs.
    /// `None` if it could not be determined (e.g. a synthetic uinput device, or
    /// sysfs unavailable). Distinguishes the 6KRO boot keyboard ([`BOOT_INTERFACE`])
    /// from the NKRO keyboard ([`NKRO_INTERFACE`]).
    pub interface: Option<u8>,
}

/// The grab decision for a set of discovered nodes — which keyboard nodes the
/// daemon should attempt to grab, and whether to warn that rollover may be capped.
///
/// `grab` is a per-node mask, index-aligned with the input slice: `true` means
/// "try to grab this node" (always still subject to the `is_tartarus` re-check in
/// [`grab`]). `warn_rollover` is set when a keyboard node will be grabbed but no
/// node was confirmed to be the N-key-rollover (interface 1) keyboard, so
/// simultaneous presses may cap at 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrabPlan {
    /// Per-node "attempt to grab" mask, index-aligned with the input.
    pub grab: Vec<bool>,
    /// True if a keyboard node will be grabbed but rollover may be capped at 6
    /// (no confirmed NKRO node — either only the boot node exists, or the node
    /// topology could not be resolved from sysfs).
    pub warn_rollover: bool,
}

/// Decide which nodes to grab, given each node's `(kind, interface)`.
///
/// This is the load-bearing NKRO node-selection logic, factored out of
/// [`crate::main`]'s `run` so it can be unit-tested without a physical device
/// (the discriminating inputs are just `NodeKind` + the sysfs-resolved interface
/// number; a real `DeviceNode` can't be hand-built because it owns an evdev
/// `Device`).
///
/// Rules:
/// - Only `Keyboard` nodes are ever grabbed; `Pointer`/`Other` pass through.
/// - ALL keyboard nodes are grabbed — including the boot keyboard (interface 0)
///   even when an NKRO node exists. A passive capture on real hardware
///   (fw v1.02, 2026-07-23) proved the firmware SPLITS the keys across the two
///   keyboard interfaces: the 20-key grid arrives on the NKRO keyboard
///   (interface 1) while the entire thumb cluster — 8-way pad arrows and the
///   thumb button's modifier bit — arrives ONLY on the boot keyboard. The old
///   rule skipped the boot node ("it just double-emits interface 1"), which
///   made every thumb-area key invisible to the engine (thumb remaps dead) and
///   leaked them — with kernel autorepeat — past the grab to the focused
///   window. Reading both is duplicate-safe: the engine ignores a press for an
///   already-held key and a release for a non-held key, so a firmware state
///   that really did double-emit would no-op.
/// - A node counts as a *real* NKRO keyboard only if its interface RESOLVED to a
///   non-boot number (`Some(i)`, `i != BOOT_INTERFACE`); `None` (synthetic node
///   or unreadable sysfs) is NOT treated as NKRO.
/// - `warn_rollover` is set whenever a keyboard node will be grabbed but no real
///   NKRO node was found (grid rollover may cap at 6).
pub fn plan_grabs(nodes: &[(NodeKind, Option<u8>)]) -> GrabPlan {
    let have_real_nkro = nodes.iter().any(|&(kind, iface)| {
        kind == NodeKind::Keyboard && iface.is_some_and(|i| i != BOOT_INTERFACE)
    });

    let mut grab = Vec::with_capacity(nodes.len());
    let mut any_keyboard_grabbed = false;
    for &(kind, _iface) in nodes {
        let do_grab = kind == NodeKind::Keyboard;
        if do_grab {
            any_keyboard_grabbed = true;
        }
        grab.push(do_grab);
    }

    GrabPlan {
        grab,
        // Warn whenever we will grab a keyboard but couldn't confirm an NKRO node.
        // This deliberately fires in the all-`None` case (unknown topology, grab
        // the boot node) — the one situation where rollover genuinely may be
        // capped — instead of silently suppressing it.
        warn_rollover: any_keyboard_grabbed && !have_real_nkro,
    }
}

/// The ANALOG-mode (DRIVER) grab decision — which nodes to grab and which of
/// the grabbed nodes to actually READ, both index-aligned with the input slice
/// like [`GrabPlan`].
///
/// Grab and read are split because grab-without-read is legitimate here: a
/// grabbed-but-unread node is a pure dupe suppressor — its events reach neither
/// the OS (grabbed) nor the engine (unread).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalogGrabPlan {
    /// Per-node "attempt to grab" mask, index-aligned with the input.
    pub grab: Vec<bool>,
    /// Per-node "spawn a reader" mask, index-aligned with the input. `true`
    /// implies `grab` at the same index.
    pub read: Vec<bool>,
}

/// Decide the grab/read surface for ANALOG (DRIVER) mode, given each node's
/// `(kind, interface)`. Pure + unit-tested with hand-built tuples, like
/// [`plan_grabs`].
///
/// In DRIVER mode the grid is digitally SILENT (pressure only, synthesized by
/// the analog source), but the thumb-area keys (thumb button = `KEY_LEFTALT`,
/// thumbpad arrows) keep emitting classic HID boot reports on the boot keyboard
/// interface ([`BOOT_INTERFACE`]). Rules:
///
/// - Grab EVERY keyboard node: the boot node because we read it, any other
///   keyboard node (NKRO) as a free dupe suppressor — if it also mirrors the
///   thumb events, ungrabbed it would leak them raw to the OS. `Pointer`/`Other`
///   are never grabbed or read — same pass-through rationale as digital mode.
/// - Read ONLY keyboard nodes whose interface RESOLVED to the boot number
///   (`Some(BOOT_INTERFACE)`) — the interface carrying the thumb events.
/// - Degraded fallback: if NO boot-confirmed keyboard node exists (unknown or
///   unreadable sysfs topology), read every grabbed keyboard node. Unknown
///   topology must not leave the thumb keys dead; the possible cross-node dupe
///   this admits is the lesser evil, and the reader's claimed-key drop filter
///   still protects the analog-owned keys either way.
/// - No rollover warning: the boot node's 6-slot cap is irrelevant in analog
///   mode — at most ~3 digital keys are ever in flight (two thumbpad arrows on
///   a diagonal + the thumb button, which rides a modifier bit, not a slot).
pub fn plan_grabs_analog(nodes: &[(NodeKind, Option<u8>)]) -> AnalogGrabPlan {
    let have_boot = nodes
        .iter()
        .any(|&(kind, iface)| kind == NodeKind::Keyboard && iface == Some(BOOT_INTERFACE));

    let mut grab = Vec::with_capacity(nodes.len());
    let mut read = Vec::with_capacity(nodes.len());
    for &(kind, iface) in nodes {
        let is_keyboard = kind == NodeKind::Keyboard;
        grab.push(is_keyboard);
        // Read the boot node; with no confirmed boot node, read everything we
        // grab (degraded fallback — see the doc comment above).
        read.push(is_keyboard && (!have_boot || iface == Some(BOOT_INTERFACE)));
    }
    AnalogGrabPlan { grab, read }
}

/// A rough classification of a Tartarus event node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Carries keyboard keys (`KEY_*`). The primary remap target.
    Keyboard,
    /// Carries relative axes (mouse movement / wheel) — the thumbpad/scroll.
    Pointer,
    /// Matches the device but exposes neither — usually a consumer-control node.
    Other,
}

/// Enumerate `/dev/input` and return every node belonging to the Tartarus Pro.
///
/// Returns an error only on a hard failure; an empty vec means the device was
/// not found (not plugged in, or permissions deny enumeration).
pub fn discover() -> anyhow::Result<Vec<DeviceNode>> {
    let mut nodes = Vec::new();
    for (path, device) in evdev::enumerate() {
        if is_tartarus(&device) {
            let kind = classify(&device);
            let interface = interface_number(&path);
            tracing::info!(
                path = %path.display(),
                name = device.name().unwrap_or("<unnamed>"),
                ?kind,
                ?interface,
                "found Tartarus Pro node",
            );
            nodes.push(DeviceNode {
                path,
                device,
                kind,
                interface,
            });
        }
    }
    Ok(nodes)
}

/// Discover and require at least one node, erroring with a helpful message
/// (including the likely permissions fix) if none are accessible.
pub fn discover_required() -> anyhow::Result<Vec<DeviceNode>> {
    let nodes = discover().context("enumerating /dev/input")?;
    if nodes.is_empty() {
        bail!(
            "no Tartarus Pro ({VENDOR_ID:#06x}:{PRODUCT_ID:#06x}) input nodes found.\n\
             Either the device is unplugged, or /dev/input/event* is not readable by your user.\n\
             Install the udev rule: sudo scripts/install-udev.sh (then re-login if prompted)."
        );
    }
    Ok(nodes)
}

/// Open an explicit `/dev/input/eventN` node (no vendor/product filter) and
/// classify it. This is the test seam: the synthetic-loopback integration test
/// points the engine at a fake source device by path. Production code uses
/// [`discover`] instead.
pub fn open_node(path: impl AsRef<std::path::Path>) -> anyhow::Result<DeviceNode> {
    let path = path.as_ref().to_path_buf();
    let device = Device::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let kind = classify(&device);
    let interface = interface_number(&path);
    Ok(DeviceNode {
        path,
        device,
        kind,
        interface,
    })
}

/// Resolve the USB `bInterfaceNumber` backing an evdev node (e.g.
/// `/dev/input/event15`) by walking its sysfs ancestors. Mirrors the hidraw
/// resolution in `tartarust-hid::discover`. Returns `None` for a non-USB or
/// synthetic device (a uinput node has no `bInterfaceNumber`), or if sysfs is
/// unavailable — callers treat `None` as "unknown, don't assume it's the boot
/// node".
fn interface_number(event_path: &Path) -> Option<u8> {
    let name = event_path.file_name()?; // e.g. "event15"
    // /sys/class/input/eventN/device -> the input device; the USB interface dir
    // (holding bInterfaceNumber) is a few levels up.
    let sys = Path::new("/sys/class/input").join(name).join("device");
    let resolved = std::fs::canonicalize(sys).ok()?;
    let mut dir: &Path = &resolved;
    for _ in 0..6 {
        if let Ok(s) = std::fs::read_to_string(dir.join("bInterfaceNumber")) {
            return u8::from_str_radix(s.trim(), 16).ok();
        }
        dir = dir.parent()?;
    }
    None
}

/// Classify a node by the event types it advertises.
fn classify(device: &Device) -> NodeKind {
    let has_keys = device
        .supported_keys()
        .is_some_and(|keys| keys.iter().any(is_keyboard_key));
    let has_rel = device
        .supported_relative_axes()
        .is_some_and(|axes| axes.contains(RelativeAxisCode::REL_X));

    match (has_keys, has_rel) {
        (true, _) => NodeKind::Keyboard,
        (false, true) => NodeKind::Pointer,
        (false, false) => NodeKind::Other,
    }
}

/// Heuristic: does this key code look like a real keyboard key (vs only mouse
/// buttons)? Keyboard `KEY_*` codes live below the button range, which starts
/// at `BTN_0` / `BTN_MISC` == `0x100` in `input-event-codes.h`.
const BTN_RANGE_START: u16 = 0x100;
fn is_keyboard_key(code: KeyCode) -> bool {
    code.0 < BTN_RANGE_START
}

/// Grab a device node (`EVIOCGRAB`) so the OS no longer receives its events.
/// Grab *after* the virtual output device exists, to avoid a window where
/// physical keys are dropped on the floor.
///
/// SAFETY GUARD (defense in depth): a grab takes a device away from the whole
/// system, so grabbing the wrong node (the user's MAIN keyboard, a mouse, any
/// non-Tartarus peripheral) would lock the user out of that device. Before
/// issuing `EVIOCGRAB` we therefore RE-READ the device's USB ids and refuse
/// unless they are *exactly* `1532:0244`. This holds even on the explicit-node
/// path ([`open_node`], no enumeration filter) and even if [`discover`]'s filter
/// ever regressed — the daemon can NEVER grab a device that is not the Tartarus
/// Pro. (The synthetic-loopback test, whose source is a uinput device with no
/// real USB ids, uses [`grab_unchecked`] instead — see its docs.)
pub fn grab(node: &mut DeviceNode) -> anyhow::Result<()> {
    if !is_tartarus(&node.device) {
        let id = node.device.input_id();
        tracing::error!(
            path = %node.path.display(),
            vendor = format_args!("{:#06x}", id.vendor()),
            product = format_args!("{:#06x}", id.product()),
            name = node.device.name().unwrap_or("<unnamed>"),
            "refusing to grab: not the Tartarus Pro ({VENDOR_ID:#06x}:{PRODUCT_ID:#06x}); \
             the daemon only ever grabs the Tartarus, never another device",
        );
        bail!(
            "refusing to EVIOCGRAB {}: USB id {:#06x}:{:#06x} is not the Tartarus Pro \
             ({VENDOR_ID:#06x}:{PRODUCT_ID:#06x})",
            node.path.display(),
            id.vendor(),
            id.product(),
        );
    }
    grab_unchecked(node)
}

/// Issue `EVIOCGRAB` WITHOUT the Tartarus vendor/product guard.
///
/// This is the test seam: the synthetic-loopback integration test grabs a fake
/// uinput SOURCE device (USB ids `0000:0000`) that the guard in [`grab`] would —
/// correctly — refuse. Production code MUST use [`grab`]; nothing in the daemon
/// binary calls this. Kept separate (and loudly named) so the production grab
/// path stays unconditionally guarded.
pub fn grab_unchecked(node: &mut DeviceNode) -> anyhow::Result<()> {
    node.device
        .grab()
        .with_context(|| format!("grabbing {}", node.path.display()))?;
    tracing::info!(path = %node.path.display(), "grabbed device");
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the NKRO node-selection logic ([`plan_grabs`]) — the
    //! load-bearing production change. These run with no device (the inputs are
    //! just `(NodeKind, Option<u8>)`), so they exercise the actual selection the
    //! daemon's `run` loop uses, unlike the loopback/actuator tests which only
    //! prove the host pipeline carries >6 concurrent keys.

    use super::*;

    const KBD: NodeKind = NodeKind::Keyboard;

    #[test]
    fn boot_and_nkro_present_grabs_both() {
        // The real device topology: iface0 boot (6KRO) + iface1 NKRO. Grab
        // BOTH: the grid rides the NKRO node but the thumb cluster rides ONLY
        // the boot node (field capture, fw v1.02) — skipping boot kills thumb
        // remaps and leaks the thumb keys to the focused window. No warning.
        let plan = plan_grabs(&[(KBD, Some(BOOT_INTERFACE)), (KBD, Some(NKRO_INTERFACE))]);
        assert_eq!(plan.grab, vec![true, true]);
        assert!(!plan.warn_rollover);
    }

    #[test]
    fn boot_only_grabs_boot_and_warns() {
        // Only the 6KRO boot keyboard exists — grab it (rather than going inert)
        // but warn that rollover may be capped at 6.
        let plan = plan_grabs(&[(KBD, Some(BOOT_INTERFACE))]);
        assert_eq!(plan.grab, vec![true]);
        assert!(plan.warn_rollover);
    }

    #[test]
    fn all_interfaces_unknown_grabs_all_keyboards_and_warns() {
        // Degraded case (finding 2): sysfs interface resolution returned None for
        // every keyboard node. `None` is NOT treated as NKRO, so the boot node is
        // still grabbed AND the rollover warning fires (it is no longer
        // suppressed). Two keyboard nodes here both get grabbed.
        let plan = plan_grabs(&[(KBD, None), (KBD, None)]);
        assert_eq!(plan.grab, vec![true, true]);
        assert!(plan.warn_rollover);
    }

    #[test]
    fn no_keyboard_node_grabs_nothing_and_does_not_warn() {
        // Pointer + Other only: nothing to grab, and the rollover warning must
        // NOT fire (no keyboard is being grabbed). The separate `grabbed_any`
        // warning in `run` covers "found the device but no keyboard".
        let plan = plan_grabs(&[(NodeKind::Pointer, Some(0)), (NodeKind::Other, None)]);
        assert_eq!(plan.grab, vec![false, false]);
        assert!(!plan.warn_rollover);
    }

    #[test]
    fn nkro_present_leaves_pointer_and_other_ungrabbed() {
        // A full node set: boot kbd, NKRO kbd, pointer, other. Both keyboards
        // are grabbed (grid on NKRO, thumb cluster on boot); pointer/other pass
        // through so thumbpad mouse movement and consumer keys reach the OS.
        let plan = plan_grabs(&[
            (KBD, Some(BOOT_INTERFACE)),
            (KBD, Some(NKRO_INTERFACE)),
            (NodeKind::Pointer, Some(2)),
            (NodeKind::Other, Some(3)),
        ]);
        assert_eq!(plan.grab, vec![true, true, false, false]);
        assert!(!plan.warn_rollover);
    }

    #[test]
    fn nkro_node_without_boot_grabs_it_no_warning() {
        // An NKRO node present but no boot node (e.g. boot node not enumerable):
        // grab the NKRO node, no warning. Confirms the skip only targets the boot
        // interface, not "any other keyboard".
        let plan = plan_grabs(&[(KBD, Some(NKRO_INTERFACE))]);
        assert_eq!(plan.grab, vec![true]);
        assert!(!plan.warn_rollover);
    }

    // ---- plan_grabs_analog: the hybrid (DRIVER-mode) grab/read decision ----

    #[test]
    fn analog_boot_and_nkro_grabs_both_reads_boot_only() {
        // The real device topology: grab BOTH keyboards (the NKRO node is a
        // dupe suppressor), but read only the boot node — it carries the
        // thumb-area digital events in DRIVER mode.
        let plan = plan_grabs_analog(&[(KBD, Some(BOOT_INTERFACE)), (KBD, Some(NKRO_INTERFACE))]);
        assert_eq!(plan.grab, vec![true, true]);
        assert_eq!(plan.read, vec![true, false]);
    }

    #[test]
    fn analog_boot_only_grabs_and_reads_it() {
        // Only the boot keyboard enumerable: grab + read it. (No rollover
        // concern in analog mode — at most ~3 digital keys in flight.)
        let plan = plan_grabs_analog(&[(KBD, Some(BOOT_INTERFACE))]);
        assert_eq!(plan.grab, vec![true]);
        assert_eq!(plan.read, vec![true]);
    }

    #[test]
    fn analog_all_interfaces_unknown_grabs_and_reads_all_keyboards() {
        // Degraded fallback: sysfs resolution failed for every keyboard node,
        // so no node is boot-CONFIRMED. Read everything grabbed — unknown
        // topology must not leave the thumb keys dead. A cross-node dupe is
        // possible here (lesser evil); the reader's claimed-key drop filter
        // still protects the analog-owned keys.
        let plan = plan_grabs_analog(&[(KBD, None), (KBD, None)]);
        assert_eq!(plan.grab, vec![true, true]);
        assert_eq!(plan.read, vec![true, true]);
    }

    #[test]
    fn analog_nkro_only_falls_back_to_reading_it() {
        // A keyboard node exists but none is boot-confirmed (boot node not
        // enumerable): the degraded fallback reads the one we grabbed rather
        // than leaving the thumb keys dead.
        let plan = plan_grabs_analog(&[(KBD, Some(NKRO_INTERFACE))]);
        assert_eq!(plan.grab, vec![true]);
        assert_eq!(plan.read, vec![true]);
    }

    #[test]
    fn analog_no_keyboard_nodes_grabs_and_reads_nothing() {
        // Pointer + Other only: nothing to grab, nothing to read.
        let plan = plan_grabs_analog(&[(NodeKind::Pointer, Some(2)), (NodeKind::Other, None)]);
        assert_eq!(plan.grab, vec![false, false]);
        assert_eq!(plan.read, vec![false, false]);
    }

    #[test]
    fn analog_pointer_and_other_never_grabbed_or_read() {
        // Full node set: boot kbd, NKRO kbd, pointer, other. Keyboards are both
        // grabbed, only boot is read; pointer/other pass through untouched
        // (grabbing the pointer would suppress the thumbpad's mouse modes).
        let plan = plan_grabs_analog(&[
            (KBD, Some(BOOT_INTERFACE)),
            (KBD, Some(NKRO_INTERFACE)),
            (NodeKind::Pointer, Some(2)),
            (NodeKind::Other, Some(3)),
        ]);
        assert_eq!(plan.grab, vec![true, true, false, false]);
        assert_eq!(plan.read, vec![true, false, false, false]);
    }
}
