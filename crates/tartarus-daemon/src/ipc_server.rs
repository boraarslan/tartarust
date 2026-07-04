//! The async IPC server: a Unix-socket front-end for the engine actor.
//!
//! Clients connect, send newline-delimited JSON [`Request`]s, and read
//! newline-delimited JSON [`Response`]s. Each request is forwarded to the
//! single-threaded engine over a `std::sync::mpsc` channel as an [`EngineMsg`],
//! paired with a `tokio::sync::oneshot` reply channel the handler awaits. The
//! oneshot's `Sender::send` is synchronous, so the engine thread (which is not
//! async) can answer without blocking.
//!
//! This server runs inside the daemon's single tokio runtime, concurrently with
//! the synchronous remap engine and the OS reader/output/device threads.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender as StdSender;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;

use tartarus_ipc::{Request, Response};

/// Maximum bytes for a single request line. A well-formed request is small
/// (a few hundred bytes); this caps a malicious/buggy client that never sends a
/// newline from OOMing the daemon (which would take the device grab down too).
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Drop a client connection that sends nothing for this long. Stops a stalled or
/// slow-loris client from pinning a task + fd indefinitely.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A message to the engine thread: either a physical input event, or an IPC
/// request awaiting a reply. The engine is the sole consumer.
pub enum EngineMsg {
    /// A physical key event from a grabbed device: `(code, value)`.
    Input(u16, i32),
    /// An IPC request and the channel to reply on.
    Ipc(Request, oneshot::Sender<Response>),
    /// Periodic/triggered device-info refresh result, pushed into the engine's
    /// cache so `GetStatus`/`GetDeviceInfo` reflect connection state.
    DeviceInfo(tartarus_ipc::DeviceInfo),
    /// The focused application's window class changed (from the app-focus
    /// watcher). The engine may auto-switch profiles. Empty string = no focus.
    FocusedApp(String),
    /// A reader thread is exiting (device unplugged / revoked). The engine must
    /// release every output key it is currently holding, so a key held at unplug
    /// time doesn't stay stuck-down on the virtual device forever.
    ReleaseAll,
}

/// Prepare the listener socket, handling a stale or live existing socket.
///
/// If the path exists: try to connect. A successful connect means another
/// daemon owns the device — we refuse to start (single-instance / single-owner
/// guard). A failed connect means a stale socket from a crashed daemon — unlink
/// and bind. This doubles as the "exactly one process owns the device" guard.
///
/// The probe-unlink-bind sequence is check-then-act, so it is serialized under
/// an exclusive flock on a sidecar lock file: without it, two daemons starting
/// at the same instant can both judge the socket stale, and the loser would
/// unlink the winner's freshly-bound LIVE socket (daemon running but
/// unreachable). The returned [`File`] holds the lock — the caller must keep it
/// alive for the daemon's lifetime; the kernel releases it on process exit.
pub async fn bind(path: &Path) -> Result<(UnixListener, File)> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let lock_path = path.with_extension("lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening startup lock {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => anyhow::bail!(
            "another tartarus-daemon is starting or already running (lock {} is held)",
            lock_path.display()
        ),
        Err(TryLockError::Error(err)) => {
            return Err(err)
                .with_context(|| format!("locking startup lock {}", lock_path.display()));
        }
    }
    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_) => {
                anyhow::bail!(
                    "another tartarus-daemon is already running (socket {} is live)",
                    path.display()
                );
            }
            Err(_) => {
                tracing::warn!(path = %path.display(), "removing stale socket");
                std::fs::remove_file(path)
                    .with_context(|| format!("removing stale socket {}", path.display()))?;
            }
        }
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding IPC socket {}", path.display()))?;
    tracing::info!(path = %path.display(), "IPC server listening");
    Ok((listener, lock))
}

/// Accept loop: spawn a handler task per connection. Runs until the runtime
/// stops. `engine_tx` is cloned into each handler so requests reach the engine.
pub async fn serve(listener: UnixListener, engine_tx: StdSender<EngineMsg>) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let tx = engine_tx.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(stream, tx).await {
                        tracing::debug!(?err, "IPC connection closed with error");
                    }
                });
            }
            Err(err) => {
                tracing::error!(?err, "IPC accept failed");
                // Brief backoff to avoid a busy loop on a persistent error.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Handle one client: read request lines, forward to the engine, write replies.
///
/// Hardened: each request line is capped at [`MAX_REQUEST_BYTES`] (a client that
/// never sends a newline can't grow the buffer unbounded and OOM the daemon),
/// and an idle connection is dropped after [`IDLE_TIMEOUT`] (no slow-loris pin
/// on a task + fd).
async fn handle_connection(stream: UnixStream, engine_tx: StdSender<EngineMsg>) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut buf = Vec::with_capacity(256);

    loop {
        buf.clear();
        // Read one newline-delimited frame with a byte cap and an idle timeout.
        let line = match read_capped_line(&mut reader, &mut buf).await {
            Ok(Ok(Frame::Eof)) => break,
            Ok(Ok(Frame::Line)) => &buf[..], // a frame (without the trailing newline)
            Ok(Err(overflow)) => {
                // Oversized line: reply with an error and close the connection
                // rather than keep buffering.
                let _ = write_response(
                    &mut write_half,
                    &Response::Error { message: overflow },
                )
                .await;
                break;
            }
            Err(_idle) => break, // idle timeout: drop the connection
        };

        let text = String::from_utf8_lossy(line);
        if text.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&text) {
            Ok(request) => dispatch(request, &engine_tx).await,
            Err(e) => Response::Error {
                message: format!("malformed request: {e}"),
            },
        };
        write_response(&mut write_half, &response).await?;
    }
    Ok(())
}

/// One framing outcome from [`read_capped_line`]. EOF is a distinct variant so
/// an empty line (`Line(0)`, e.g. a client's accidental leading `\n`) is skipped
/// by the connection loop instead of being mistaken for a disconnect.
enum Frame {
    /// The peer closed the connection (no pending payload).
    Eof,
    /// A complete frame is in `buf` (possibly empty — an empty line).
    Line,
}

/// Read one `\n`-terminated frame into `buf`, with a HARD byte cap and an idle
/// timeout. Returns:
///   * `Ok(Ok(frame))` — EOF, or a frame in `buf` (without the trailing newline),
///   * `Ok(Err(msg))` — the frame reached the cap before a newline (overflow),
///   * `Err(())` — idle timeout elapsed waiting for data.
///
/// Genuinely memory-bounded: it consumes the buffered reader chunk-by-chunk
/// (`fill_buf`/`consume`) and stops the instant `buf` would exceed the cap, so a
/// newline-less flood never allocates past [`MAX_REQUEST_BYTES`]. The whole read
/// is wrapped in one idle timeout.
#[allow(clippy::type_complexity)]
async fn read_capped_line<R>(reader: &mut R, buf: &mut Vec<u8>) -> Result<Result<Frame, String>, ()>
where
    R: AsyncBufReadExt + Unpin,
{
    tokio::time::timeout(IDLE_TIMEOUT, async {
        loop {
            let available = match reader.fill_buf().await {
                Ok(b) => b,
                Err(_) => return Ok(Frame::Eof), // read error: treat as EOF
            };
            if available.is_empty() {
                // EOF. A non-empty buf is a final unterminated frame; hand it
                // over — the next call returns Eof.
                return Ok(if buf.is_empty() { Frame::Eof } else { Frame::Line });
            }
            // Copy up to a newline, but never past the cap.
            match available.iter().position(|&b| b == b'\n') {
                Some(nl) => {
                    let take = nl.min(MAX_REQUEST_BYTES.saturating_sub(buf.len()));
                    buf.extend_from_slice(&available[..take]);
                    reader.consume(nl + 1); // consume through the newline
                    if buf.len() >= MAX_REQUEST_BYTES && take < nl {
                        return Err(format!(
                            "request exceeds {MAX_REQUEST_BYTES} bytes; closing connection"
                        ));
                    }
                    return Ok(Frame::Line);
                }
                None => {
                    let room = MAX_REQUEST_BYTES.saturating_sub(buf.len());
                    if room == 0 {
                        // Cap reached with no newline in sight: overflow.
                        return Err(format!(
                            "request exceeds {MAX_REQUEST_BYTES} bytes; closing connection"
                        ));
                    }
                    let take = available.len().min(room);
                    buf.extend_from_slice(&available[..take]);
                    let consumed = available.len();
                    reader.consume(consumed);
                }
            }
        }
    })
    .await
    .map_err(|_| ())
}

/// Serialize and write one response line (newline-terminated), flushing.
async fn write_response<W>(write_half: &mut W, response: &Response) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut out = serde_json::to_string(response).expect("response serializes");
    out.push('\n');
    write_half.write_all(out.as_bytes()).await?;
    write_half.flush().await?;
    Ok(())
}

/// Forward a request to the engine and await its reply.
async fn dispatch(request: Request, engine_tx: &StdSender<EngineMsg>) -> Response {
    let (reply_tx, reply_rx) = oneshot::channel();
    if engine_tx.send(EngineMsg::Ipc(request, reply_tx)).is_err() {
        return Response::Error {
            message: "daemon engine is not running".into(),
        };
    }
    match reply_rx.await {
        Ok(response) => response,
        Err(_) => Response::Error {
            message: "daemon dropped the request without replying".into(),
        },
    }
}

/// Best-effort removal of the socket file on shutdown.
pub fn cleanup(path: &Path) {
    if let Err(err) = std::fs::remove_file(path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), ?err, "failed to remove socket on shutdown");
    }
}

/// A small client used by the CLI to talk to the daemon: connect, send one
/// request, read one response. Kept here so the wire framing lives in one place.
pub async fn request_once(socket: &Path, request: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to daemon at {}", socket.display()))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut line = serde_json::to_string(request).context("serializing request")?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    let mut reader = BufReader::new(read_half).lines();
    match reader.next_line().await.context("reading response")? {
        Some(resp_line) => {
            serde_json::from_str::<Response>(&resp_line).context("parsing response")
        }
        None => anyhow::bail!("daemon closed the connection without replying"),
    }
}

/// The socket path, overridable via `RUSTY_TARTARUS_SOCKET` for tests / multi
/// instances.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("RUSTY_TARTARUS_SOCKET") {
        PathBuf::from(p)
    } else {
        tartarus_ipc::default_socket_path()
    }
}
