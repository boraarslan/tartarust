//! Safety: the daemon must NEVER `EVIOCGRAB` a device that is not the Tartarus
//! Pro (`1532:0244`). Grabbing the wrong node — the user's main keyboard, a
//! mouse, any other peripheral — would take that device away from the whole
//! system and lock the user out of it (the reported bug: "I can't type `h` with
//! my main keyboard while the daemon runs").
//!
//! `input::grab` therefore re-reads the device's USB ids and refuses unless they
//! are exactly the Tartarus Pro's, immediately before the grab. This test proves
//! that guard against a REAL device: a synthetic uinput device has USB ids
//! `0000:0000` (it is not a Tartarus), so the production `grab` must refuse it,
//! while the explicitly-unchecked `grab_unchecked` (the loopback test seam) must
//! still succeed on the same node. That difference is the whole guarantee.
//!
//! Gated: needs a writable `/dev/uinput`. If absent (e.g. CI), the test prints
//! `SKIPPED` and returns green rather than failing.

use std::path::PathBuf;
use std::time::Duration;

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, KeyCode};

use tartarus_daemon::input;

/// Can we create uinput devices here? If not, the test is skipped (green).
fn uinput_available() -> bool {
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::KEY_A);
    VirtualDevice::builder()
        .and_then(|b| b.name("tartarus-guard-probe").with_keys(&keys))
        .and_then(|b| b.build())
        .is_ok()
}

/// A fake keyboard device that is NOT the Tartarus Pro. A uinput device created
/// without an explicit USB id reports vendor/product `0x0000` — exactly the
/// "some other keyboard" case the guard must reject.
fn build_non_tartarus_keyboard() -> VirtualDevice {
    let mut keys = AttributeSet::<KeyCode>::new();
    for k in [KeyCode::KEY_H, KeyCode::KEY_A, KeyCode::KEY_B] {
        keys.insert(k);
    }
    VirtualDevice::builder()
        .unwrap()
        .name("definitely-not-a-tartarus")
        .with_keys(&keys)
        .unwrap()
        .build()
        .unwrap()
}

/// Resolve a virtual device's `/dev/input/eventN` node, retrying for the brief
/// window before the kernel populates sysfs.
fn node_path(dev: &mut VirtualDevice) -> PathBuf {
    for _ in 0..100 {
        if let Ok(nodes) = dev.enumerate_dev_nodes_blocking()
            && let Some(p) = nodes.flatten().next()
        {
            return p;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("device event node never appeared in sysfs");
}

/// Wait until a freshly-created uinput node is openable (udev applies the
/// `input` group + 0660 a beat after the node appears as root:root 0600).
fn wait_openable(path: &std::path::Path) {
    for _ in 0..200 {
        match Device::open(path) {
            Ok(_) => return,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("opening {}: {e}", path.display()),
        }
    }
    panic!("{} stayed permission-denied", path.display());
}

#[test]
fn grab_refuses_a_non_tartarus_device_but_unchecked_grabs_it() {
    if !uinput_available() {
        eprintln!("SKIPPED device_guard: /dev/uinput not writable here");
        return;
    }
    eprintln!("RAN device_guard: /dev/uinput available");

    let mut fake = build_non_tartarus_keyboard();
    let path = node_path(&mut fake);
    wait_openable(&path);

    // The node opens fine; it is just not the Tartarus Pro.
    let mut node = input::open_node(&path).expect("open the fake node");

    // Sanity: this device is not recognized as the Tartarus.
    assert!(
        !input::is_tartarus(&node.device),
        "a uinput device with USB ids 0000:0000 must not be seen as the Tartarus Pro",
    );

    // THE GUARANTEE: the production grab path REFUSES to EVIOCGRAB it. Were the
    // guard absent, this would grab (and lock out) a non-Tartarus device.
    let err = input::grab(&mut node)
        .expect_err("grab MUST refuse a non-Tartarus device");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not the Tartarus Pro") || msg.contains("refusing"),
        "refusal error should explain it is not the Tartarus Pro; got: {msg}",
    );

    // The explicitly-unchecked seam (used only by the loopback test) DOES grab
    // the same node — proving the refusal above is the Tartarus guard's doing,
    // not some uinput quirk that would block every grab. If the guard were the
    // only thing standing between the daemon and grabbing a non-Tartarus device,
    // this is the exact line that would otherwise have locked out the device.
    input::grab_unchecked(&mut node)
        .expect("grab_unchecked must still grab the node (no Tartarus guard)");
    node.device.ungrab().expect("ungrab the now-grabbed node");

    std::mem::forget(fake);
}
