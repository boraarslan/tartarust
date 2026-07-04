//! Synthetic uinput loopback: the FIRST real end-to-end proof that the remap
//! plumbing works, with no physical Tartarus.
//!
//! Wiring (all real code — uses `pipeline::spawn_reader` + `pipeline::spawn_engine`
//! and the real `output` sink, the same code `main.rs` runs):
//!
//!   fake uinput SOURCE  --(grabbed by engine reader)-->  engine  --emit-->  real uinput SINK
//!        ^ inject here                                                          ^ test reads here
//!
//! Safety: the SINK is a real uinput device — without a grab, the remapped
//! events would be delivered to whatever window is focused. The test EVIOCGRABs
//! the sink's read fd so the compositor never sees them (and we get clean
//! reads), and it sequences every grab BEFORE the first injection so nothing
//! leaks. Verified on the dev box with a terminal focused: zero stray input.
//!
//! Gated: needs a writable `/dev/uinput`. If absent (e.g. CI), the test prints
//! `SKIPPED` and returns green rather than failing.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, EventType, KeyCode, KeyEvent};

use tartarus_daemon::engine::Engine;
use tartarus_daemon::ipc_server::EngineMsg;
use tartarus_daemon::state::{AnalogConfig, Config, InputMode};
use tartarus_daemon::{input, output, pipeline};

use tartarus_ipc::model::{BindAction, Macro, MacroStep, Profile};

const DAEMON_VERSION: &str = "test";

/// Can we create uinput devices here? If not, the test is skipped (green).
fn uinput_available() -> bool {
    // Writable /dev/uinput is the requirement; the cheapest probe is to try to
    // build a trivial virtual device.
    let mut keys = AttributeSet::<KeyCode>::new();
    keys.insert(KeyCode::KEY_A);
    VirtualDevice::builder()
        .and_then(|b| b.name("tartarus-uinput-probe").with_keys(&keys))
        .and_then(|b| b.build())
        .is_ok()
}

/// Build a fake keyboard SOURCE device that declares the keys we will inject.
/// It must declare real keyboard keys (< 0x100) so the engine classifies it as
/// `Keyboard` and reads it.
fn build_source() -> VirtualDevice {
    let mut keys = AttributeSet::<KeyCode>::new();
    for k in [
        KeyCode::KEY_A,
        KeyCode::KEY_B,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_F1,
        KeyCode::KEY_H,
        KeyCode::KEY_I,
        // The eight NKRO-test keys (KEY_1..KEY_8). Declaring extra keys is inert
        // for the other tests; only the NKRO test injects these.
        KeyCode::KEY_1,
        KeyCode::KEY_2,
        KeyCode::KEY_3,
        KeyCode::KEY_4,
        KeyCode::KEY_5,
        KeyCode::KEY_6,
        KeyCode::KEY_7,
        KeyCode::KEY_8,
    ] {
        keys.insert(k);
    }
    VirtualDevice::builder()
        .unwrap()
        .name("tartarus-test-source")
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
    panic!("source device event node never appeared in sysfs");
}

/// Open an event node for reading, retrying on `EACCES`. A freshly-created
/// uinput node briefly exists as `root:root 0600` (visible in sysfs) before
/// udev applies the `input` group + 0660 — so the node can appear before it is
/// openable by our user. Retry until udev catches up.
fn open_with_retry(path: &std::path::Path) -> Device {
    for _ in 0..200 {
        match Device::open(path) {
            Ok(d) => return d,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("opening {}: {e}", path.display()),
        }
    }
    panic!("{} stayed permission-denied (udev never applied input group)", path.display());
}

/// Inject a key down+up on the source device.
fn tap(source: &mut VirtualDevice, code: KeyCode) {
    source.emit(&[*KeyEvent::new(code, 1)]).unwrap();
    source.emit(&[*KeyEvent::new(code, 0)]).unwrap();
}

/// Read KEY (code, value) events from the grabbed sink until `want` of them are
/// collected or `timeout` elapses. Filters SYN. Returns what was collected.
fn read_keys(sink: &mut Device, want: usize, timeout: Duration) -> Vec<(u16, i32)> {
    let mut out = Vec::new();
    let deadline = Instant::now() + timeout;
    while out.len() < want && Instant::now() < deadline {
        // The sink fd is set non-blocking, so `fetch_events` returns `WouldBlock`
        // (not a hang) when no events are ready — letting us honor the deadline.
        match sink.fetch_events() {
            Ok(events) => {
                for ev in events {
                    if ev.event_type() == EventType::KEY {
                        out.push((ev.code(), ev.value()));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    out
}

/// The remap config under test: KEY_A -> KEY_B (base); KEY_LEFTALT = Hypershift
/// trigger; in the hypershift layer KEY_A -> KEY_F1; KEY_B -> a macro that taps
/// KEY_H then KEY_I.
fn test_config() -> Config {
    let mut p = Profile::new("default");
    p.base.insert(
        "KEY_A".into(),
        BindAction::Key {
            keys: vec!["KEY_B".into()],
        },
    );
    p.base
        .insert("KEY_LEFTALT".into(), BindAction::Hypershift);
    p.base.insert(
        "KEY_B".into(),
        BindAction::Macro {
            name: "hi".into(),
        },
    );
    p.hypershift.insert(
        "KEY_A".into(),
        BindAction::Key {
            keys: vec!["KEY_F1".into()],
        },
    );

    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert("default".into(), p);
    let mut macros = std::collections::BTreeMap::new();
    macros.insert(
        "hi".into(),
        Macro {
            name: "hi".into(),
            steps: vec![
                MacroStep::Press { key: "KEY_H".into() },
                MacroStep::Release { key: "KEY_H".into() },
                MacroStep::Press { key: "KEY_I".into() },
                MacroStep::Release { key: "KEY_I".into() },
            ],
        },
    );
    Config {
        default_profile: "default".into(),
        auto_switch: false,
        tray: true,
        input_mode: InputMode::Digital,
        analog: AnalogConfig::default(),
        profiles,
        macros,
    }
}

/// A no-remap config: one empty profile, no macros. Every key is unbound, so the
/// engine passes it through verbatim (proven by `engine::passthrough_for_unbound_key`).
/// Used by the NKRO test so the assertion is "physical KEY_n -> KEY_n on the sink",
/// uncluttered by any remap.
fn passthrough_config() -> Config {
    let p = Profile::new("default");
    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert("default".into(), p);
    Config {
        default_profile: "default".into(),
        auto_switch: false,
        tray: true,
        input_mode: InputMode::Digital,
        analog: AnalogConfig::default(),
        profiles,
        macros: std::collections::BTreeMap::new(),
    }
}

#[test]
fn synthetic_loopback_remaps_keys_hypershift_and_macros() {
    if !uinput_available() {
        eprintln!("SKIPPED synthetic_loopback: /dev/uinput not writable here");
        return;
    }
    eprintln!("RAN synthetic_loopback: /dev/uinput available");

    // 1. Create the fake SOURCE and resolve its node.
    let mut source = build_source();
    let source_path = node_path(&mut source);

    // 2. Create the real SINK (output thread) and resolve its node.
    let out = output::spawn().expect("output sink");
    let out_tx = out.sender();
    let sink_path = {
        // Retry: output::spawn already retries, but guard against an empty list.
        let mut tries = 0;
        loop {
            if let Some(p) = out.node_paths().first() {
                break p.clone();
            }
            tries += 1;
            assert!(tries < 100, "sink node never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    // 3. GRAB the sink on the test's read fd BEFORE anything is injected, so the
    //    remapped events never reach the focused window and we read them cleanly.
    //    (Retry-open: a fresh uinput node is briefly root:root 0600 before udev.)
    let mut sink_reader = open_with_retry(&sink_path);
    sink_reader.grab().expect("grab sink");
    // Non-blocking so `read_keys` honors its deadline instead of blocking forever
    // on `fetch_events` when fewer events than expected arrive.
    sink_reader
        .set_nonblocking(true)
        .expect("set sink non-blocking");

    // 4. Point the engine at the SOURCE by path (the test seam) and grab it.
    //    Wait until the source node is openable (same udev race) before handing
    //    it to the engine via `open_node`.
    drop(open_with_retry(&source_path));
    let mut node = input::open_node(&source_path).expect("open source node");
    // The synthetic source is a uinput device with no real USB ids, so the
    // production `grab`'s Tartarus-only guard would (correctly) refuse it. The
    // test exercises the grab→read→remap→emit path, not device selection, so it
    // uses the explicitly-unchecked grab seam.
    input::grab_unchecked(&mut node).expect("grab source");

    // 5. Wire the REAL pipeline: reader -> engine -> output sink.
    let (engine_tx, engine_rx) = mpsc::channel::<EngineMsg>();
    let device = tartarus_daemon::device::spawn(); // no real device; lighting no-ops
    let _reader = pipeline::spawn_reader(node, engine_tx);
    let engine = Engine::new(test_config());
    let _engine = pipeline::spawn_engine(
        engine,
        engine_rx,
        out_tx,
        device,
        std::env::temp_dir().join("tartarus-loopback-nonexistent.toml"),
        DAEMON_VERSION,
    );

    // Give the threads a moment to reach their read loops.
    std::thread::sleep(Duration::from_millis(150));

    // ---- Case 1: base remap KEY_A -> KEY_B ----
    tap(&mut source, KeyCode::KEY_A);
    let got = read_keys(&mut sink_reader, 2, Duration::from_secs(2));
    assert_eq!(
        got,
        vec![(KeyCode::KEY_B.0, 1), (KeyCode::KEY_B.0, 0)],
        "KEY_A should remap to KEY_B"
    );

    // ---- Case 2: Hypershift: hold KEY_LEFTALT, KEY_A -> KEY_F1 ----
    source.emit(&[*KeyEvent::new(KeyCode::KEY_LEFTALT, 1)]).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    tap(&mut source, KeyCode::KEY_A);
    let got = read_keys(&mut sink_reader, 2, Duration::from_secs(2));
    assert_eq!(
        got,
        vec![(KeyCode::KEY_F1.0, 1), (KeyCode::KEY_F1.0, 0)],
        "with Hypershift held, KEY_A should remap to KEY_F1"
    );
    source.emit(&[*KeyEvent::new(KeyCode::KEY_LEFTALT, 0)]).unwrap();
    std::thread::sleep(Duration::from_millis(30));

    // ---- Case 3: macro: KEY_B -> taps KEY_H, KEY_I ----
    // (KEY_B isn't bound in hypershift, and hypershift is released, so base.)
    tap(&mut source, KeyCode::KEY_B);
    // Macro emits H down/up, I down/up = 4 key events. Allow time for any delays.
    let got = read_keys(&mut sink_reader, 4, Duration::from_secs(3));
    assert_eq!(
        got,
        vec![
            (KeyCode::KEY_H.0, 1),
            (KeyCode::KEY_H.0, 0),
            (KeyCode::KEY_I.0, 1),
            (KeyCode::KEY_I.0, 0),
        ],
        "KEY_B should play the hi macro (H, I)"
    );

    // Drop the test's grab on the sink so nothing is left grabbed.
    drop(sink_reader);
    // Do NOT join the output/engine threads: the engine thread holds an
    // `out_tx` clone (same reason `main.rs` can't cooperatively join), so a join
    // would block forever. Detach the handles and let process teardown reclaim
    // the uinput devices when the test binary exits.
    std::mem::forget(out);
    std::mem::forget(source);
}

/// Regression (#2): if the device is unplugged while a key is held, the reader's
/// exit must trigger `EngineMsg::ReleaseAll` so the held output key is released
/// on the sink (no stuck key). Here we simulate unplug by DROPPING the source
/// uinput device mid-hold, which makes the grabbed reader's `fetch_events` error.
#[test]
fn unplug_mid_hold_releases_held_keys() {
    if !uinput_available() {
        eprintln!("SKIPPED unplug_mid_hold: /dev/uinput not writable here");
        return;
    }
    eprintln!("RAN unplug_mid_hold: /dev/uinput available");

    let mut source = build_source();
    let source_path = node_path(&mut source);

    let out = output::spawn().expect("output sink");
    let out_tx = out.sender();
    let sink_path = {
        let mut tries = 0;
        loop {
            if let Some(p) = out.node_paths().first() {
                break p.clone();
            }
            tries += 1;
            assert!(tries < 100, "sink node never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    let mut sink_reader = open_with_retry(&sink_path);
    sink_reader.grab().expect("grab sink");
    sink_reader.set_nonblocking(true).expect("nonblocking");

    drop(open_with_retry(&source_path));
    let mut node = input::open_node(&source_path).expect("open source node");
    // Unchecked grab: synthetic uinput source has no Tartarus USB ids (see the
    // companion test for why production `grab` would refuse it).
    input::grab_unchecked(&mut node).expect("grab source");

    let (engine_tx, engine_rx) = mpsc::channel::<EngineMsg>();
    let device = tartarus_daemon::device::spawn();
    let _reader = pipeline::spawn_reader(node, engine_tx);
    let engine = Engine::new(test_config()); // KEY_A -> KEY_B
    let _engine = pipeline::spawn_engine(
        engine,
        engine_rx,
        out_tx,
        device,
        std::env::temp_dir().join("tartarus-loopback-nonexistent.toml"),
        DAEMON_VERSION,
    );

    std::thread::sleep(Duration::from_millis(150));

    // Hold KEY_A (-> KEY_B down on the sink); deliberately NO key-up.
    source.emit(&[*KeyEvent::new(KeyCode::KEY_A, 1)]).unwrap();
    let down = read_keys(&mut sink_reader, 1, Duration::from_secs(2));
    assert_eq!(down, vec![(KeyCode::KEY_B.0, 1)], "held KEY_A -> KEY_B down");

    // Simulate UNPLUG: drop the source device. The grabbed reader's next
    // fetch_events() errors, the reader sends EngineMsg::ReleaseAll, and the
    // engine releases the held output (KEY_B) on the sink.
    drop(source);

    let up = read_keys(&mut sink_reader, 1, Duration::from_secs(3));
    assert_eq!(
        up,
        vec![(KeyCode::KEY_B.0, 0)],
        "unplug mid-hold must release the held KEY_B (no stuck key)"
    );

    drop(sink_reader);
    std::mem::forget(out);
}

/// Pipeline NKRO regression guard: prove the HOST side (reader -> engine ->
/// uinput sink) carries MORE than 6 keys held simultaneously. This injects 8
/// distinct key-DOWNs with NO interleaved ups on the synthetic source and asserts
/// all 8 arrive on the grabbed sink, then 8 ups with no stuck keys.
///
/// SCOPE / what this does NOT prove: the actual 6KRO bug lived in *device-node
/// selection* (the daemon grabbing the iface0 boot keyboard), fixed by
/// `input::plan_grabs` + the skip loop in `main.rs::run`. This test wires a SINGLE
/// synthetic source straight to the engine; it never builds boot-vs-NKRO nodes and
/// never invokes the selection logic — reverting that change leaves this test
/// green. The node-selection fix is covered by `input::tests::plan_grabs_*` (pure
/// unit tests) plus the deferred live-hardware check. This test only guards that
/// the engine `held`/`output_refcount` maps and the 0..0x300 uinput sink remain
/// NKRO-clean (the part §1 of the plan established was never the cap).
///
/// Passthrough config (every key unbound -> emitted verbatim), so the assertion
/// is purely about concurrency, not remapping. The 8 keys are KEY_1..KEY_8 (all
/// declared by `build_source`).
#[test]
fn nkro_eight_simultaneous_keys_pass_through() {
    if !uinput_available() {
        eprintln!("SKIPPED nkro_eight_simultaneous: /dev/uinput not writable here");
        return;
    }
    eprintln!("RAN nkro_eight_simultaneous: /dev/uinput available");

    const KEYS: [KeyCode; 8] = [
        KeyCode::KEY_1,
        KeyCode::KEY_2,
        KeyCode::KEY_3,
        KeyCode::KEY_4,
        KeyCode::KEY_5,
        KeyCode::KEY_6,
        KeyCode::KEY_7,
        KeyCode::KEY_8,
    ];

    // 1. Fake SOURCE + its node.
    let mut source = build_source();
    let source_path = node_path(&mut source);

    // 2. Real SINK (output thread) + its node.
    let out = output::spawn().expect("output sink");
    let out_tx = out.sender();
    let sink_path = {
        let mut tries = 0;
        loop {
            if let Some(p) = out.node_paths().first() {
                break p.clone();
            }
            tries += 1;
            assert!(tries < 100, "sink node never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    // 3. GRAB the sink BEFORE any injection so the 8 keys never reach the desktop.
    let mut sink_reader = open_with_retry(&sink_path);
    sink_reader.grab().expect("grab sink");
    sink_reader.set_nonblocking(true).expect("set sink non-blocking");

    // 4. Grab the SOURCE (unchecked: synthetic uinput device has no Tartarus ids).
    drop(open_with_retry(&source_path));
    let mut node = input::open_node(&source_path).expect("open source node");
    input::grab_unchecked(&mut node).expect("grab source");

    // 5. Wire the REAL pipeline with a passthrough config.
    let (engine_tx, engine_rx) = mpsc::channel::<EngineMsg>();
    let device = tartarus_daemon::device::spawn();
    let _reader = pipeline::spawn_reader(node, engine_tx);
    let engine = Engine::new(passthrough_config());
    let _engine = pipeline::spawn_engine(
        engine,
        engine_rx,
        out_tx,
        device,
        std::env::temp_dir().join("tartarus-loopback-nonexistent.toml"),
        DAEMON_VERSION,
    );

    std::thread::sleep(Duration::from_millis(150));

    // ---- 8 key-DOWNs, NO interleaved ups: all 8 must arrive concurrently. ----
    for k in KEYS {
        source.emit(&[*KeyEvent::new(k, 1)]).unwrap();
    }
    let downs = read_keys(&mut sink_reader, 8, Duration::from_secs(3));
    assert_eq!(
        downs.len(),
        8,
        "expected 8 concurrent key-downs on the sink (NKRO), got {downs:?}"
    );
    for k in KEYS {
        assert!(
            downs.contains(&(k.0, 1)),
            "key {k:?} down missing from sink (rollover cap?); got {downs:?}"
        );
    }

    // ---- 8 ups: every held key releases, no stuck keys. ----
    for k in KEYS {
        source.emit(&[*KeyEvent::new(k, 0)]).unwrap();
    }
    let ups = read_keys(&mut sink_reader, 8, Duration::from_secs(3));
    assert_eq!(
        ups.len(),
        8,
        "expected 8 key-ups on the sink (no stuck keys), got {ups:?}"
    );
    for k in KEYS {
        assert!(
            ups.contains(&(k.0, 0)),
            "key {k:?} up missing from sink (stuck key); got {ups:?}"
        );
    }

    drop(sink_reader);
    std::mem::forget(out);
    std::mem::forget(source);
}
