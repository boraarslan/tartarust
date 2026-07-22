//! Integration test for the IPC server's wire path: socket framing + request
//! dispatch + the oneshot reply roundtrip. This is the one layer the in-crate
//! unit tests (which call `Engine::apply` directly) do not cover.
//!
//! No device and no root are needed: we bind a temp socket, run a real `Engine`
//! on a background thread fed by the same `EngineMsg` channel the daemon uses,
//! and drive it through a real `UnixStream` client.

use std::sync::mpsc;
use std::thread;

use tartarust_daemon::engine::Engine;
use tartarust_daemon::ipc_server::{self, EngineMsg};
use tartarust_daemon::state::Config;
use tartarust_ipc::{Request, Response};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Send a raw line, read one response line back.
async fn roundtrip(socket: &std::path::Path, line: &str) -> String {
    let stream = UnixStream::connect(socket).await.expect("connect");
    let (read_half, mut write_half) = stream.into_split();
    let mut msg = line.to_string();
    msg.push('\n');
    write_half.write_all(msg.as_bytes()).await.expect("write");
    write_half.flush().await.expect("flush");
    let mut reader = BufReader::new(read_half).lines();
    reader.next_line().await.expect("read").expect("a line")
}

#[test]
fn ipc_server_handles_requests_over_a_real_socket() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Unique temp socket for this test.
    let socket =
        std::env::temp_dir().join(format!("tartarust-ipc-test-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);

    // Engine on a background thread, fed by the channel the daemon uses.
    let (engine_tx, engine_rx) = mpsc::channel::<EngineMsg>();
    let mut engine = Engine::new(Config::default_single());
    let engine_thread = thread::spawn(move || {
        for msg in engine_rx {
            if let EngineMsg::Ipc(request, reply) = msg {
                let (response, _cmds) = engine.apply(request, "test");
                let _ = reply.send(response);
            }
        }
    });

    rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let (listener, _lock) = ipc_server::bind(&socket).await.expect("bind");
            let serve_tx = engine_tx.clone();
            let server = tokio::spawn(async move { ipc_server::serve(listener, serve_tx).await });

            // Ping -> Pong
            let pong = roundtrip(&socket, r#"{"kind":"ping"}"#).await;
            assert_eq!(
                serde_json::from_str::<Response>(&pong).unwrap(),
                Response::Pong
            );

            // ListProfiles -> Profiles with the default profile active
            let list = roundtrip(&socket, r#"{"kind":"list_profiles"}"#).await;
            match serde_json::from_str::<Response>(&list).unwrap() {
                Response::Profiles(pl) => {
                    assert_eq!(pl.active, "default");
                    assert_eq!(pl.profiles.len(), 1);
                }
                other => panic!("expected Profiles, got {other:?}"),
            }

            // A typed request also works end to end.
            let req = serde_json::to_string(&Request::GetStatus).unwrap();
            let status = roundtrip(&socket, &req).await;
            assert!(matches!(
                serde_json::from_str::<Response>(&status).unwrap(),
                Response::Status(_)
            ));

            // Malformed JSON -> Error (not a crash, not a hang).
            let err = roundtrip(&socket, "{ this is not json").await;
            assert!(matches!(
                serde_json::from_str::<Response>(&err).unwrap(),
                Response::Error { .. }
            ));

            server.abort();
        })
        .await
        .expect("IPC integration timed out (deadlock?)");
    });

    // Tear down: dropping engine_tx closes the channel so the engine thread ends.
    drop(engine_tx);
    let _ = engine_thread.join();
    let _ = std::fs::remove_file(&socket);
}

/// Regression (#3): an oversized request line (no newline, > 64 KiB) must NOT
/// OOM or hang the daemon — the server caps the frame, replies with an Error,
/// and closes the connection.
#[test]
fn ipc_server_rejects_oversized_request_line() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let socket =
        std::env::temp_dir().join(format!("tartarust-ipc-oversize-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);

    let (engine_tx, engine_rx) = mpsc::channel::<EngineMsg>();
    let mut engine = Engine::new(Config::default_single());
    let engine_thread = thread::spawn(move || {
        for msg in engine_rx {
            if let EngineMsg::Ipc(request, reply) = msg {
                let (response, _cmds) = engine.apply(request, "test");
                let _ = reply.send(response);
            }
        }
    });

    rt.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let (listener, _lock) = ipc_server::bind(&socket).await.expect("bind");
            let serve_tx = engine_tx.clone();
            let server = tokio::spawn(async move { ipc_server::serve(listener, serve_tx).await });

            // Send 256 KiB with NO newline, then try to read the reply.
            let stream = UnixStream::connect(&socket).await.expect("connect");
            let (read_half, mut write_half) = stream.into_split();
            let flood = vec![b'x'; 256 * 1024];
            // write_all may error once the server closes the connection mid-flood;
            // that's acceptable — we just need the daemon to survive + respond.
            let _ = write_half.write_all(&flood).await;
            let _ = write_half.flush().await;

            // The server should have replied with an overflow Error before closing.
            let mut reader = BufReader::new(read_half).lines();
            let line = reader.next_line().await.expect("read");
            if let Some(resp) = line {
                assert!(
                    matches!(
                        serde_json::from_str::<Response>(&resp).unwrap(),
                        Response::Error { .. }
                    ),
                    "expected an Error reply for the oversized line, got: {resp}"
                );
            }
            // Crucially: the daemon is still alive and serving — a fresh ping works.
            let pong = roundtrip(&socket, r#"{"kind":"ping"}"#).await;
            assert_eq!(
                serde_json::from_str::<Response>(&pong).unwrap(),
                Response::Pong,
                "daemon must keep serving after rejecting an oversized line"
            );

            server.abort();
        })
        .await
        .expect("oversized-line test timed out");
    });

    drop(engine_tx);
    let _ = engine_thread.join();
    let _ = std::fs::remove_file(&socket);
}
