//! Non-disruptive probe for the app-focus watcher.
//!
//! Runs the REAL `appfocus::spawn` watcher path (Hyprland socket2 or X11 poll)
//! and prints each `FocusedApp(class)` it would feed the engine. It does NOT
//! grab the keypad or touch the device — it only observes window focus, so it's
//! safe to run while using the machine.
//!
//!     cargo run -p tartarus-daemon --example focus_probe
//!
//! Focus different windows; you should see the app class print on each change.
//! Ctrl-C to stop.

use std::sync::mpsc;

use tartarus_daemon::appfocus::{self, Backend};
use tartarus_daemon::ipc_server::EngineMsg;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    println!("focus backend: {:?}", appfocus::detect_backend());
    if appfocus::detect_backend() == Backend::None {
        eprintln!("no focus backend available (need Hyprland or X11); nothing to probe");
        return;
    }
    println!("watching focus changes — focus other windows, Ctrl-C to stop\n");

    let (tx, rx) = mpsc::channel::<EngineMsg>();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async move {
        appfocus::spawn(tx);
        // The watcher runs as a tokio task; drain the channel on a blocking
        // thread so the runtime keeps the task alive.
        tokio::task::spawn_blocking(move || {
            for msg in rx {
                if let EngineMsg::FocusedApp(class) = msg {
                    if class.is_empty() {
                        println!("FocusedApp(<none>)");
                    } else {
                        println!("FocusedApp({class})");
                    }
                }
            }
        })
        .await
        .ok();
    });
}
