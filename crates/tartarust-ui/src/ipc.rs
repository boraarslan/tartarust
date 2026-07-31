//! IPC client: talks to the running `tartarust-daemon` over its Unix socket.
//!
//! Wire format mirrors the daemon's `ipc_server` and the `tartarust` CLI: connect
//! to [`tartarust_ipc::socket_path`] (overridable via `TARTARUST_SOCKET`), write
//! one newline-terminated JSON [`Request`], read one newline-terminated JSON
//! [`Response`]. One request per connection.
//!
//! **Design: synchronous and store-agnostic.** Every call here BLOCKS the
//! calling thread on plain `std` socket I/O — GPUI owns the event loop, so
//! callers run these on its background executor and apply the result back on
//! the foreground. This module owns the transport, the load sequence, and the
//! request shapes; everything stateful (the offline flag, optimistic edits,
//! resync-after-rejection, the unsaved bookkeeping, macro-body caching, the
//! switch generation gate) lives in the GPUI store layer. Nothing here reads or
//! writes UI state.
//!
//! The two failure kinds stay distinguishable, because the store treats them
//! differently: [`Error::Transport`] means the daemon is unreachable (offline —
//! keep the last-known state), while a reachable daemon that refuses a request
//! answers [`Response::Error`]. [`send`] hands that rejection back as `Ok`; the
//! typed readers ([`load_snapshot`], [`get_macro`]) map it to [`Error::Rejected`].

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use tartarust_ipc::{
    AnalogState, BindAction, DaemonStatus, DeviceInfo, Layer, LightingState, Macro, Profile,
    ProfileSummary, Request, Response, ThumbAxisMode, socket_path,
};

/// How long one exchange may block. A Unix-socket connect either succeeds or
/// fails immediately, so this bounds the request write and the reply read.
///
/// Previously only the modal export/import flows were bounded (they block a
/// dialog that refuses dismissal while busy, so an unbounded wait on a wedged
/// daemon locked the screen forever). Every call now blocks a real thread rather
/// than an async task, so the bound is uniform: a wedged daemon must never pin
/// a background-executor thread.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Why an IPC call did not produce the payload the caller asked for.
///
/// The split is load-bearing for the store: only [`Error::Transport`] means
/// "daemon unreachable" (flip the offline badge, keep the last-known state).
/// The other two variants came back FROM a live daemon, which proves it is
/// reachable — a stale offline flag should be cleared on those, exactly as the
/// old client did on any decoded reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Connect / write / read / decode failure — the daemon is unreachable.
    Transport(String),
    /// The daemon answered [`Response::Error`]: reachable, but it refused.
    Rejected(String),
    /// The daemon answered a well-formed reply of the wrong variant.
    Unexpected(String),
}

impl Error {
    /// Whether a decoded reply came back, i.e. the daemon is reachable. False
    /// only for [`Error::Transport`] — this is the "clear a stale offline flag"
    /// test the store applies after every exchange.
    pub fn reached_daemon(&self) -> bool {
        !matches!(self, Error::Transport(_))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(m) | Error::Rejected(m) | Error::Unexpected(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Connect, send one request, read one response.
///
/// `Err` is always [`Error::Transport`]: a daemon that answers
/// [`Response::Error`] returns `Ok` carrying that response, because a rejection
/// is not a disconnect. The store resyncs on the former and goes offline on the
/// latter, so the two must not collapse into one `Err`.
pub fn send(request: &Request) -> Result<Response, Error> {
    send_on(&socket_path(), request)
}

/// As [`send`], but against an explicit socket path (the testable core).
pub fn send_on(socket: &Path, request: &Request) -> Result<Response, Error> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| Error::Transport(format!("connect {}: {e}", socket.display())))?;
    // Bound both directions before any I/O: a daemon that accepts and then
    // wedges must not hold this thread forever.
    stream
        .set_write_timeout(Some(REPLY_TIMEOUT))
        .map_err(|e| Error::Transport(format!("set write timeout: {e}")))?;
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(|e| Error::Transport(format!("set read timeout: {e}")))?;

    let mut line =
        serde_json::to_string(request).map_err(|e| Error::Transport(format!("encode: {e}")))?;
    line.push('\n');

    // `&UnixStream` implements both `Read` and `Write`, so one owned stream
    // serves the write and the buffered read without a split.
    let mut writer = &stream;
    writer
        .write_all(line.as_bytes())
        .map_err(|e| Error::Transport(io_message("write", &e)))?;
    writer
        .flush()
        .map_err(|e| Error::Transport(io_message("flush", &e)))?;

    let mut reply = String::new();
    let read = BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|e| Error::Transport(io_message("read", &e)))?;
    if read == 0 {
        return Err(Error::Transport(
            "daemon closed connection without replying".into(),
        ));
    }
    serde_json::from_str(reply.trim_end()).map_err(|e| Error::Transport(format!("decode: {e}")))
}

/// Render an I/O failure for a human. A blocking socket reports an expired
/// [`REPLY_TIMEOUT`] as `WouldBlock`/`TimedOut`, which reads as noise in the
/// offline log line — name it for what it is instead.
fn io_message(stage: &str, e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) {
        format!("daemon did not reply within {}s", REPLY_TIMEOUT.as_secs())
    } else {
        format!("{stage}: {e}")
    }
}

/// Classify a decoded-but-wrong reply: a daemon refusing the request is a
/// [`Error::Rejected`] (it is reachable), anything else is a schema surprise.
fn unexpected_reply(request: &str, reply: Response) -> Error {
    match reply {
        Response::Error { message } => Error::Rejected(message),
        other => Error::Unexpected(format!("{request}: unexpected daemon reply: {other:?}")),
    }
}

/// Log and swallow a best-effort read's failure — the caller keeps what it has.
fn best_effort_miss(request: &str, outcome: Result<Response, Error>) {
    match outcome {
        Ok(other) => tracing::warn!("{request}: unexpected daemon reply: {other:?}"),
        Err(e) => tracing::warn!("{request}: {e}"),
    }
}

// --- Snapshot load -----------------------------------------------------------

/// One read of everything the UI renders at load: daemon status, the profile
/// list, the active profile in full, device info, and the macro names.
///
/// Plain data — no view state, no store. How it is overlaid is the store
/// layer's call, which is why the two best-effort fields are `Option`: `None`
/// means the daemon did not answer that request, and the store should KEEP what
/// it already has rather than blank it. (The old `load_state` got that for free
/// by mutating a base `AppState` in place; a standalone snapshot has no base to
/// fall back to, so it says "missing" explicitly.)
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// From `GetStatus`. Mandatory. `status.unsaved_changes` is the
    /// authoritative Save-affordance flag — the store mirrors it rather than
    /// trusting its own optimistic value.
    pub status: DaemonStatus,
    /// From `ListProfiles`. Mandatory.
    pub profiles: Vec<ProfileSummary>,
    /// Index into [`Snapshot::profiles`] of the active profile, resolved from
    /// `ListProfiles`' own `active` name (0 when it names nothing in the list).
    pub active_profile: usize,
    /// The active profile in full (binds for both layers + lighting). Mandatory.
    pub profile: Profile,
    /// From `GetDeviceInfo`. Best-effort: `None` when the daemon did not answer.
    pub device: Option<DeviceInfo>,
    /// Macro NAMES from `ListMacros`, as bodyless [`Macro`] stubs — `ListMacros`
    /// returns names only and each body is fetched on demand with [`get_macro`].
    /// Best-effort: `None` when the daemon did not answer. On `Some`, the store
    /// must also clear its loaded-bodies set, so the editor can tell a
    /// not-yet-fetched stub from a genuinely-empty macro (review #5).
    pub macros: Option<Vec<Macro>>,
}

/// Pull one full [`Snapshot`] from the daemon. `Err` means overlay nothing: the
/// caller keeps its prior mock/last-known state and its offline flag.
///
/// Sequence: `GetStatus` -> `ListProfiles` -> `GetProfile{active}` ->
/// `GetDeviceInfo` -> `ListMacros`. The first three are MANDATORY: each request
/// opens its own connection, so a daemon dying mid-sequence is a real window —
/// failing the whole load keeps the caller's prior state instead of presenting
/// mock/stale profiles as live. Device info and macros stay best-effort.
pub fn load_snapshot() -> Result<Snapshot, Error> {
    load_snapshot_on(&socket_path())
}

/// As [`load_snapshot`], but against an explicit socket path (the testable core).
pub fn load_snapshot_on(socket: &Path) -> Result<Snapshot, Error> {
    // Status first: this is the liveness gate.
    let status = match send_on(socket, &Request::GetStatus)? {
        Response::Status(s) => s,
        other => return Err(unexpected_reply("GetStatus", other)),
    };
    // The full body to fetch is the status' active profile (the list's `active`
    // resolves the INDEX below — the daemon reports the same name in both).
    let active_name = status.active_profile.clone();

    // Profiles list. Mandatory: a failure here (daemon died between requests)
    // must fail the load, not leave the caller's mock/stale list looking live.
    let list = match send_on(socket, &Request::ListProfiles)? {
        Response::Profiles(l) => l,
        other => return Err(unexpected_reply("ListProfiles", other)),
    };
    let active_profile = list
        .profiles
        .iter()
        .position(|p| p.name == list.active)
        .unwrap_or(0);

    // The active profile in full (binds + lighting). Mandatory for the same
    // reason — editors must never edit a mock/stale body presented as live.
    let profile = match send_on(socket, &Request::GetProfile { name: active_name })? {
        Response::Profile(p) => p,
        other => return Err(unexpected_reply("GetProfile", other)),
    };

    // Device info: best-effort.
    let device = match send_on(socket, &Request::GetDeviceInfo) {
        Ok(Response::DeviceInfo(info)) => Some(info),
        other => {
            best_effort_miss("GetDeviceInfo", other);
            None
        }
    };

    // Macros: ListMacros returns names only, so they land as bodyless stubs and
    // each body is fetched on demand via `get_macro`. Best-effort.
    let macros = match send_on(socket, &Request::ListMacros) {
        Ok(Response::Macros { names }) => Some(
            names
                .into_iter()
                .map(|name| Macro {
                    name,
                    steps: Vec::new(),
                })
                .collect(),
        ),
        other => {
            best_effort_miss("ListMacros", other);
            None
        }
    };

    Ok(Snapshot {
        status,
        profiles: list.profiles,
        active_profile,
        profile,
        device,
        macros,
    })
}

// --- Typed reads -------------------------------------------------------------

/// Fetch one macro's full body. Used when a listed macro is a bodyless stub
/// ([`Snapshot::macros`] carries names only).
///
/// The error split is load-bearing here: [`Error::Rejected`] means the macro is
/// GONE daemon-side (deleted between the list and the click), so the store must
/// still mark the name loaded — otherwise the editor sits on its "Loading…"
/// placeholder and the prefetch effect refetches a doomed request forever
/// (review #3). [`Error::Transport`] leaves the stub unloaded so a later retry
/// can fill it.
pub fn get_macro(name: &str) -> Result<Macro, Error> {
    get_macro_on(&socket_path(), name)
}

/// As [`get_macro`], but against an explicit socket path (the testable core).
pub fn get_macro_on(socket: &Path, name: &str) -> Result<Macro, Error> {
    match send_on(
        socket,
        &Request::GetMacro {
            name: name.to_owned(),
        },
    )? {
        Response::Macro(m) => Ok(m),
        other => Err(unexpected_reply("GetMacro", other)),
    }
}

/// Poll the live analog state (mode flag + per-key pressures + actuation config).
/// `None` on ANY failure — the Calibration screen treats that as "offline / not
/// analog" and shows no live bars (the cage harness, with no daemon, always
/// takes this path). Unlike [`load_snapshot`] this is polled on a short interval
/// while the Calibration screen is mounted, so a miss is unremarkable and
/// carries no message.
pub fn get_analog_state() -> Option<AnalogState> {
    get_analog_state_on(&socket_path())
}

/// As [`get_analog_state`], but against an explicit socket path (testable core).
pub fn get_analog_state_on(socket: &Path) -> Option<AnalogState> {
    match send_on(socket, &Request::GetAnalogState) {
        Ok(Response::AnalogState(a)) => Some(a),
        _ => None,
    }
}

// --- Request builders --------------------------------------------------------
//
// Pure: each builds the exact `Request` one UI action sends, and nothing else.
// The store layer owns everything around it — the optimistic local edit, the
// `send` on the background executor, the resync when a reachable daemon rejects
// the mutation, and the offline flip on a transport failure.

/// Whether a request changes persisted config and so can leave unsaved session
/// edits. The store flips its unsaved flag true for these so the Save
/// affordance appears immediately, then corrects it from the daemon's
/// authoritative live-vs-disk diff (a `GetStatus`) once the request completes.
/// `SwitchProfile`/reads/`Save` are excluded.
pub fn dirties(request: &Request) -> bool {
    use Request::*;
    matches!(
        request,
        CreateProfile { .. }
            | DuplicateProfile { .. }
            | RenameProfile { .. }
            | DeleteProfile { .. }
            | SetProfileLinkedApp { .. }
            | SetDefaultProfile { .. }
            | SetThumbAxisMode { .. }
            | SetBind { .. }
            | SetLighting { .. }
            | SetBrightness { .. }
            | SaveMacro { .. }
            | DeleteMacro { .. }
            | SetAutoSwitch { .. }
            | SetActuation { .. }
    )
}

/// Update one analog key's actuation settings. Caller must uphold the schema
/// invariants: `release_point < actuation_point`, `second_actuation_point`
/// deeper than actuation, and `second_action` set iff `second_actuation_point`
/// is set — the daemon rejects violations, which the store resyncs on.
pub fn req_set_actuation(
    index: u8,
    actuation_point: u8,
    release_point: u8,
    second_actuation_point: Option<u8>,
    second_action: Option<String>,
) -> Request {
    Request::SetActuation {
        index,
        actuation_point,
        release_point,
        second_actuation_point,
        second_action,
    }
}

/// Persist one key's bind on a layer of a profile.
pub fn req_set_bind(
    profile: String,
    layer: Layer,
    key: String,
    action: Option<BindAction>,
) -> Request {
    Request::SetBind {
        profile,
        layer,
        key,
        action,
    }
}

/// Apply lighting to the ACTIVE profile (`profile: None` — the Lighting screen
/// always edits whatever is active).
pub fn req_set_lighting(lighting: LightingState) -> Request {
    Request::SetLighting {
        profile: None,
        lighting,
    }
}

/// Set device brightness immediately (0–255).
pub fn req_set_brightness(level: u8) -> Request {
    Request::SetBrightness { level }
}

/// Toggle per-application auto-switch (a global/per-device setting).
pub fn req_set_auto_switch(enabled: bool) -> Request {
    Request::SetAutoSwitch { enabled }
}

/// Set (or clear) the application a profile auto-activates for (M4). `None`
/// clears the link.
pub fn req_set_profile_linked_app(name: String, linked_app: Option<String>) -> Request {
    Request::SetProfileLinkedApp { name, linked_app }
}

/// Set how a profile's thumb stick quantizes direction (eight/four-way).
pub fn req_set_thumb_axis_mode(profile: String, mode: ThumbAxisMode) -> Request {
    Request::SetThumbAxisMode { profile, mode }
}

/// Set the profile that is active by default (when no app match applies).
pub fn req_set_default_profile(name: String) -> Request {
    Request::SetDefaultProfile { name }
}

/// Create a new, empty profile.
pub fn req_create_profile(name: String) -> Request {
    Request::CreateProfile { name }
}

/// Deep-copy a profile (binds, lighting, thumb mode — not the app link) under a
/// new name.
pub fn req_duplicate_profile(from: String, to: String) -> Request {
    Request::DuplicateProfile { from, to }
}

/// Rename a profile. `from` must be the ORIGINAL (pre-edit) name — the
/// debounced rename bug (review #4) was sending `from == to`.
pub fn req_rename_profile(from: String, to: String) -> Request {
    Request::RenameProfile { from, to }
}

/// Delete a profile. The daemon refuses the active/last one.
pub fn req_delete_profile(name: String) -> Request {
    Request::DeleteProfile { name }
}

/// Make a profile active. The caller pairs this with [`req_get_profile`] to
/// reload the body: the two must land together, or Customize/Lighting keep
/// editing the OLD profile while binds target the new name (review #1).
pub fn req_switch_profile(name: String) -> Request {
    Request::SwitchProfile { name }
}

/// Fetch one profile in full (binds + lighting).
pub fn req_get_profile(name: String) -> Request {
    Request::GetProfile { name }
}

/// Commit all pending session changes to disk (the Save affordance). `name` is
/// advisory — the daemon persists the whole config.
pub fn req_save_profile(name: Option<String>) -> Request {
    Request::SaveProfile { name }
}

/// Fetch a profile's portable export bundle (the profile + the macros its binds
/// reference). A read: the reply is `Response::ProfileExport`.
pub fn req_export_profile(name: String) -> Request {
    Request::ExportProfile { name }
}

/// Import ONE profile entry (from an export bundle or a whole config file).
/// `rename` applies only when the file holds exactly one entry, like the CLI's
/// `--rename`; the caller enforces that before building the requests, and sends
/// one request per entry.
pub fn req_import_profile(
    profile: Profile,
    macros: Vec<Macro>,
    rename: Option<String>,
    overwrite: bool,
) -> Request {
    Request::ImportProfile {
        profile,
        macros,
        rename,
        overwrite,
    }
}

/// Create or replace a named macro (the macro's `name` field is the key).
pub fn req_save_macro(r#macro: Macro) -> Request {
    Request::SaveMacro { r#macro }
}

/// Delete a macro by name (the ✕ on a Macros-screen rail row).
pub fn req_delete_macro(name: String) -> Request {
    Request::DeleteMacro { name }
}

#[cfg(test)]
mod tests {
    //! The cage harness runs with NO daemon, so the live IPC path never executes
    //! there. These tests stand up a throwaway `UnixListener` on a background
    //! thread that replies with canned `Response`s, exercising the wire logic
    //! (`send_on` + `load_snapshot_on`) against the real `tartarust-ipc` schema.

    use super::*;
    use std::os::unix::net::UnixListener;
    use tartarust_ipc::model::Effect;
    use tartarust_ipc::{AnalogKeyConfig, MacroStep, PROTOCOL_VERSION, ProfileList};

    /// Canned daemon: answers each connection by matching on the request `kind`.
    /// Serves `n` connections then returns, leaving the listener bound (the
    /// caller decides whether anything else answers).
    fn serve_canned(listener: &UnixListener, n: usize) {
        for _ in 0..n {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let req: Request = serde_json::from_str(line.trim_end()).expect("decode request");
            let resp = match req {
                Request::GetStatus => Response::Status(DaemonStatus {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_version: "test".into(),
                    device_connected: true,
                    active_profile: "Live".into(),
                    default_profile: "Live".into(),
                    auto_switch: false,
                    unsaved_changes: false,
                }),
                Request::ListProfiles => Response::Profiles(ProfileList {
                    profiles: vec![
                        ProfileSummary {
                            name: "Live".into(),
                            linked_app: Some("game.exe".into()),
                        },
                        ProfileSummary {
                            name: "Other".into(),
                            linked_app: None,
                        },
                    ],
                    active: "Live".into(),
                }),
                Request::GetProfile { name } => {
                    let mut p = Profile::new(name);
                    p.lighting = LightingState {
                        effect: Effect::Spectrum,
                        brightness: 200,
                    };
                    Response::Profile(p)
                }
                Request::GetDeviceInfo => Response::DeviceInfo(DeviceInfo {
                    firmware: Some("9.9".into()),
                    serial: Some("SERIAL123".into()),
                    connected: true,
                }),
                // ListMacros serializes now that Response::Macros is a struct
                // variant (#16). Return real names so the client's macro stubs
                // are exercised end-to-end.
                Request::ListMacros => Response::Macros {
                    names: vec!["M1".into(), "M2".into()],
                },
                // GetMacro returns the full body for #18 (live macro steps).
                Request::GetMacro { name } => Response::Macro(Macro {
                    name,
                    steps: vec![
                        MacroStep::Press {
                            key: "KEY_R".into(),
                        },
                        MacroStep::Delay { ms: 25 },
                        MacroStep::Release {
                            key: "KEY_R".into(),
                        },
                    ],
                }),
                // GetAnalogState: an active keypad with one configured key, so the
                // calibration client's analog read is exercised end-to-end.
                Request::GetAnalogState => Response::AnalogState(AnalogState {
                    analog_active: true,
                    pressures: vec![0, 0, 0x90],
                    keys: vec![AnalogKeyConfig {
                        index: 2,
                        key: "KEY_Q".into(),
                        actuation_point: 0x60,
                        release_point: 0x50,
                        second_actuation_point: None,
                        second_action: None,
                    }],
                }),
                _ => Response::Ack,
            };
            let mut out = serde_json::to_string(&resp).unwrap();
            out.push('\n');
            let mut w = &stream;
            w.write_all(out.as_bytes()).unwrap();
            w.flush().unwrap();
        }
    }

    /// Accept one connection, return the RAW request line the client wrote, and
    /// reply `Ack` so the client's read completes cleanly.
    fn capture_one(listener: &UnixListener) -> String {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let mut w = &stream;
        w.write_all(b"{\"kind\":\"ack\"}\n").unwrap();
        w.flush().unwrap();
        line
    }

    /// A unique socket path per test, so tests run in parallel without sharing
    /// the env var (the explicit-socket cores `send_on`/`load_snapshot_on` avoid
    /// any global mutation).
    fn temp_socket(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("tartarust-test-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn send_roundtrips_one_request() {
        let sock = temp_socket("send");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || serve_canned(&listener, 1));

        let resp = send_on(&sock, &Request::GetStatus).expect("send ok");
        match resp {
            Response::Status(s) => assert_eq!(s.active_profile, "Live"),
            other => panic!("unexpected response: {other:?}"),
        }
        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    /// The load sequence's payload, end to end. (What the STORE does with a
    /// snapshot — overlaying it onto live state while preserving transient view
    /// state — is the GPUI store layer's business and is tested there.)
    #[test]
    fn load_snapshot_reads_live_state() {
        let sock = temp_socket("load");
        let listener = UnixListener::bind(&sock).unwrap();
        // load_snapshot makes 5 sequential requests, one connection each.
        let server = std::thread::spawn(move || serve_canned(&listener, 5));

        let snap = load_snapshot_on(&sock).expect("daemon reachable");

        assert_eq!(snap.status.active_profile, "Live");
        assert!(!snap.status.unsaved_changes);
        assert_eq!(snap.profile.name, "Live");
        assert_eq!(snap.profile.lighting.brightness, 200);
        assert_eq!(snap.profiles.len(), 2);
        assert_eq!(
            snap.active_profile, 0,
            "index resolved from the active name"
        );
        assert_eq!(
            snap.device.as_ref().and_then(|d| d.serial.as_deref()),
            Some("SERIAL123")
        );
        // ListMacros succeeds (#16), so the names come back — as bodyless stubs,
        // since there is no body in a ListMacros reply.
        let macros = snap.macros.as_ref().expect("ListMacros answered");
        assert_eq!(macros.len(), 2, "macro names from ListMacros");
        assert_eq!(macros[0].name, "M1");
        assert!(macros[0].steps.is_empty(), "live macros load name-only");

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn load_snapshot_fails_when_no_daemon() {
        let sock = temp_socket("absent"); // never bound — connect must fail
        let err = load_snapshot_on(&sock).expect_err("no daemon => Err");
        assert!(matches!(err, Error::Transport(_)), "got {err:?}");
        assert!(!err.reached_daemon());
    }

    /// A daemon dying MID-sequence (after `GetStatus`, before `ListProfiles`)
    /// must fail the whole load: an `Ok` here would hand the caller a snapshot
    /// whose profile list came from nowhere, or (worse, pre-split) let the store
    /// present its own mock profiles as live daemon state.
    #[test]
    fn load_snapshot_fails_when_daemon_dies_mid_sequence() {
        let sock = temp_socket("die-mid");
        let listener = UnixListener::bind(&sock).unwrap();
        // Answer ONLY the GetStatus connection, then accept the follow-up
        // ListProfiles connection and close it without replying — a daemon that
        // died after its first answer. (Dropping the listener instead would race
        // the client's connect, which the kernel backlog can still accept, and
        // the client would then sit out the full REPLY_TIMEOUT.)
        let server = std::thread::spawn(move || {
            serve_canned(&listener, 1);
            let _ = listener.accept();
        });

        let err =
            load_snapshot_on(&sock).expect_err("mid-sequence failure must fail the load wholesale");
        assert!(matches!(err, Error::Transport(_)), "got {err:?}");

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    /// Executable check of the mutation WIRE FORMAT: drive a real `SetBind` (the
    /// shape the Customize screen builds) through the builder + transport and
    /// assert the daemon receives exactly the expected tagged JSON.
    #[test]
    fn set_bind_emits_expected_request_json() {
        let sock = temp_socket("setbind");
        let listener = UnixListener::bind(&sock).unwrap();
        // Capture the raw request line the client sends.
        let captured = std::thread::spawn(move || capture_one(&listener));

        let req = req_set_bind(
            "FPS — Default".into(),
            Layer::Hypershift,
            "KEY_W".into(),
            Some(BindAction::Key {
                keys: vec!["KEY_F5".into()],
            }),
        );
        let resp = send_on(&sock, &req).expect("send ok");
        assert!(matches!(resp, Response::Ack));

        let line = captured.join().unwrap();
        // Re-parse to compare structurally (field order is serde-defined but we
        // assert on the decoded value to stay robust).
        let got: Request = serde_json::from_str(line.trim_end()).unwrap();
        match got {
            Request::SetBind {
                profile,
                layer,
                key,
                action,
            } => {
                assert_eq!(profile, "FPS — Default");
                assert_eq!(layer, Layer::Hypershift);
                assert_eq!(key, "KEY_W");
                assert_eq!(
                    action,
                    Some(BindAction::Key {
                        keys: vec!["KEY_F5".into()]
                    })
                );
            }
            other => panic!("expected SetBind, got {other:?}"),
        }
        // And spot-check the tag is present in the raw wire form.
        assert!(line.contains("\"kind\":\"set_bind\""), "wire: {line}");

        let _ = std::fs::remove_file(&sock);
    }

    /// GetMacro round-trip: the request shape the Macros screen sends and the
    /// `Response::Macro(Macro)` it decodes into real steps (#18).
    #[test]
    fn get_macro_round_trips_full_body() {
        let sock = temp_socket("getmacro");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || serve_canned(&listener, 1));

        let m = get_macro_on(&sock, "M1").expect("send ok");
        assert_eq!(m.name, "M1");
        assert_eq!(m.steps.len(), 3, "full body, not a name-only stub");
        assert_eq!(
            m.steps[0],
            MacroStep::Press {
                key: "KEY_R".into()
            }
        );
        assert_eq!(m.steps[1], MacroStep::Delay { ms: 25 });

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
    }

    /// GetAnalogState round-trip: the request the Calibration screen polls and
    /// the `Response::AnalogState(AnalogState)` it decodes into the live mode
    /// flag, per-key pressures, and actuation config it renders.
    #[test]
    fn get_analog_state_round_trips() {
        let sock = temp_socket("analog");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || serve_canned(&listener, 1));

        let analog = get_analog_state_on(&sock).expect("daemon reachable");
        assert!(analog.analog_active, "canned daemon is in analog mode");
        assert_eq!(analog.pressures, vec![0, 0, 0x90]);
        assert_eq!(analog.keys.len(), 1);
        assert_eq!(analog.keys[0].index, 2);
        assert_eq!(analog.keys[0].key, "KEY_Q");
        assert_eq!(analog.keys[0].actuation_point, 0x60);
        assert_eq!(analog.keys[0].release_point, 0x50);

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);

        // No daemon => None (the cage-harness / offline path: no live bars).
        let absent = temp_socket("analog-absent");
        assert!(get_analog_state_on(&absent).is_none());
    }

    /// `SetActuation` wire shape: the request the Calibration sliders build,
    /// including a dual-step (second actuation + action), must reach the daemon
    /// as the expected tagged JSON.
    #[test]
    fn set_actuation_emits_expected_request_json() {
        let sock = temp_socket("setact");
        let listener = UnixListener::bind(&sock).unwrap();
        let captured = std::thread::spawn(move || capture_one(&listener));

        let req = req_set_actuation(2, 0x60, 0x50, Some(0xC0), Some("KEY_LEFTSHIFT".into()));
        let resp = send_on(&sock, &req).expect("send ok");
        assert!(matches!(resp, Response::Ack));

        let line = captured.join().unwrap();
        match serde_json::from_str::<Request>(line.trim_end()).unwrap() {
            Request::SetActuation {
                index,
                actuation_point,
                release_point,
                second_actuation_point,
                second_action,
            } => {
                assert_eq!(index, 2);
                assert_eq!(actuation_point, 0x60);
                assert_eq!(release_point, 0x50);
                assert_eq!(second_actuation_point, Some(0xC0));
                assert_eq!(second_action.as_deref(), Some("KEY_LEFTSHIFT"));
            }
            other => panic!("expected SetActuation, got {other:?}"),
        }
        assert!(line.contains("\"kind\":\"set_actuation\""), "wire: {line}");

        let _ = std::fs::remove_file(&sock);
    }

    /// A daemon that *rejects* a mutation surfaces as `Response::Error` (not an
    /// `Err`) — this is the branch the store keys on to re-pull authoritative
    /// state instead of leaving the optimistic edit diverged (review #2). A
    /// connection failure (offline) is a transport `Err` and is NOT a rejection.
    #[test]
    fn rejected_mutation_is_response_error_not_transport_err() {
        let sock = temp_socket("reject");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let mut w = &stream;
            w.write_all(b"{\"kind\":\"error\",\"message\":\"no such profile\"}\n")
                .unwrap();
            w.flush().unwrap();
        });

        let resp = send_on(&sock, &req_set_brightness(128)).expect("transport ok");
        // The rejection is an in-band Error the store resyncs on...
        assert!(matches!(resp, Response::Error { .. }));
        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);

        // ...whereas no daemon at all is a transport Err (offline: edit stays
        // local, no resync). Confirm the two are distinguishable.
        let absent = temp_socket("reject-absent");
        assert!(matches!(
            send_on(&absent, &req_set_brightness(1)),
            Err(Error::Transport(_))
        ));
    }

    /// The debounced rename (review #4) must send `RenameProfile{from, to}` with
    /// the ORIGINAL (pre-edit) name as `from` — the bug was `from == to`. This
    /// pins the wire shape the Profiles rename handler builds.
    #[test]
    fn rename_emits_from_original_to_new() {
        let sock = temp_socket("rename");
        let listener = UnixListener::bind(&sock).unwrap();
        let captured = std::thread::spawn(move || capture_one(&listener));

        // Shape the handler produces: from = committed (original), to = edited.
        let req = req_rename_profile("FPS".into(), "FPSX".into());
        let resp = send_on(&sock, &req).expect("send ok");
        assert!(matches!(resp, Response::Ack));

        let line = captured.join().unwrap();
        match serde_json::from_str::<Request>(line.trim_end()).unwrap() {
            Request::RenameProfile { from, to } => {
                assert_eq!(
                    from, "FPS",
                    "from must be the original, not the edited name"
                );
                assert_eq!(to, "FPSX");
            }
            other => panic!("expected RenameProfile, got {other:?}"),
        }
        let _ = std::fs::remove_file(&sock);
    }

    /// `dirties` gates the optimistic unsaved flag: config-changing mutations
    /// yes, reads and `Save`/`SwitchProfile` no.
    #[test]
    fn dirties_flags_only_config_mutations() {
        assert!(dirties(&req_set_brightness(10)));
        assert!(dirties(&req_create_profile("x".into())));
        assert!(dirties(&req_delete_macro("m".into())));
        assert!(!dirties(&req_save_profile(None)));
        assert!(!dirties(&req_switch_profile("x".into())));
        assert!(!dirties(&Request::GetStatus));
    }
}
