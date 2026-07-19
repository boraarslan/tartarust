//! `tartarus` — command-line client for the daemon.
//!
//! Lib-only crate: the whole binary lives here behind [`main`], and the
//! workspace's root package exposes it as the `tartarus` bin via a thin
//! `src/bin/` shim, so one `cargo install --path .` ships every binary.
//!
//! Two families of subcommands:
//!   * **Device-owner passthroughs** (`discover`, `discover-keys`, `run`) shell
//!     out to the `tartarus-daemon` binary — it is the process that grabs the
//!     device. These need a terminal and are not socket calls.
//!   * **Socket commands** (`ping`, `info`, `profile`, `lighting`, `brightness`)
//!     talk to a running daemon over the `tartarus-ipc` Unix socket (newline-
//!     delimited JSON) and print the reply.

use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use tartarus_ipc::model::{Effect, LightingState, Rgb};
use tartarus_ipc::{Request, Response};

/// Command-line client for the Razer Tartarus Pro daemon.
#[derive(Debug, Parser)]
#[command(name = "tartarus", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    // ---- device-owner passthroughs (shell out to the daemon) ----
    /// List the Tartarus Pro input nodes (does not grab; safe while in use).
    Discover,
    /// Print each key event from the keypad (for building your config).
    DiscoverKeys,
    /// Run the remap engine + IPC server (grabs the device; needs udev rule).
    Run {
        /// Path to the config file (defaults to the XDG config location).
        #[arg(short, long)]
        config: Option<PathBuf>,
    },

    // ---- socket commands (talk to a running daemon) ----
    /// Liveness check.
    Ping,
    /// Show daemon + device info (firmware, serial, connection).
    Info,
    /// Profile operations.
    Profile {
        #[command(subcommand)]
        op: ProfileOp,
    },
    /// Lighting operations.
    Lighting {
        #[command(subcommand)]
        op: LightingOp,
    },
    /// Set the device brightness (0–255).
    Brightness {
        /// Brightness level.
        level: u8,
    },
    /// Enable or disable per-application auto-switch.
    AutoSwitch {
        /// `on` or `off`.
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Macro operations.
    Macro {
        #[command(subcommand)]
        op: MacroOp,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileOp {
    /// List all profiles and which is active.
    List,
    /// Switch the active profile.
    Switch {
        /// Profile name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum MacroOp {
    /// List all macro names.
    List,
    /// Show one macro's full body (its steps).
    Get {
        /// Macro name.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum LightingOp {
    /// Show the active profile's lighting.
    Get,
    /// Set the active profile's lighting effect.
    Set {
        /// Effect: none, static, spectrum, wave, reactive, breathing, starlight.
        effect: String,
        /// Color as a hex string `RRGGBB` (for static/reactive/breathing).
        #[arg(long)]
        color: Option<String>,
    },
}

/// Binary entry point: parse args, dispatch. Called by the root package's
/// `src/bin/tartarus.rs` shim.
pub fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // Passthroughs.
        Cmd::Discover => run_daemon(&["discover".into()]),
        Cmd::DiscoverKeys => run_daemon(&["discover-keys".into()]),
        Cmd::Run { config } => {
            let mut args = vec!["run".to_string()];
            if let Some(path) = config {
                args.push("--config".into());
                args.push(path.to_string_lossy().into_owned());
            }
            run_daemon(&args)
        }
        // Socket commands run on a small tokio runtime.
        other => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building runtime")?
            .block_on(socket_command(other)),
    }
}

async fn socket_command(cmd: Cmd) -> Result<()> {
    let socket = tartarus_ipc::socket_path();
    match cmd {
        Cmd::Ping => {
            print_response(send(&socket, &Request::Ping).await?);
        }
        Cmd::Info => {
            print_response(send(&socket, &Request::GetDeviceInfo).await?);
        }
        Cmd::Profile { op } => match op {
            ProfileOp::List => print_response(send(&socket, &Request::ListProfiles).await?),
            ProfileOp::Switch { name } => {
                print_response(send(&socket, &Request::SwitchProfile { name }).await?)
            }
        },
        Cmd::Lighting { op } => match op {
            LightingOp::Get => {
                print_response(send(&socket, &Request::GetLighting { profile: None }).await?)
            }
            LightingOp::Set { effect, color } => {
                // Validate the args BEFORE the round-trip below, so a bad
                // --color errors cleanly even with no daemon running. The
                // brightness is a placeholder until the fetch.
                let mut lighting = build_lighting(&effect, color.as_deref(), 255)?;
                // `set` changes only the effect — preserve the profile's current brightness.
                let current = send(&socket, &Request::GetLighting { profile: None }).await?;
                match current {
                    Response::Lighting(state) => lighting.brightness = state.brightness,
                    other => {
                        print_response(other);
                        return Ok(());
                    }
                }
                print_response(
                    send(
                        &socket,
                        &Request::SetLighting {
                            profile: None,
                            lighting,
                        },
                    )
                    .await?,
                );
            }
        },
        Cmd::Brightness { level } => {
            print_response(send(&socket, &Request::SetBrightness { level }).await?);
        }
        Cmd::AutoSwitch { state } => {
            let enabled = state == "on";
            print_response(send(&socket, &Request::SetAutoSwitch { enabled }).await?);
        }
        Cmd::Macro { op } => match op {
            MacroOp::List => print_response(send(&socket, &Request::ListMacros).await?),
            MacroOp::Get { name } => {
                print_response(send(&socket, &Request::GetMacro { name }).await?)
            }
        },
        // Passthroughs handled in main().
        Cmd::Discover | Cmd::DiscoverKeys | Cmd::Run { .. } => unreachable!(),
    }
    Ok(())
}

/// Build a [`LightingState`] from an effect name and optional hex color.
fn build_lighting(effect: &str, color: Option<&str>, brightness: u8) -> Result<LightingState> {
    let parsed_color = color.map(parse_hex).transpose()?;
    let need_color = || parsed_color.unwrap_or(Rgb::new(0, 255, 0));
    let effect = match effect {
        "none" => Effect::None,
        "spectrum" => Effect::Spectrum,
        "static" => Effect::Static {
            color: need_color(),
        },
        "wave" => Effect::Wave {
            direction: tartarus_ipc::model::WaveDirection::Left,
        },
        "reactive" => Effect::Reactive {
            speed: 2,
            color: need_color(),
        },
        "breathing" => Effect::Breathing {
            colors: parsed_color.map(|c| vec![c]).unwrap_or_default(),
        },
        "starlight" => Effect::Starlight {
            speed: 2,
            colors: parsed_color.map(|c| vec![c]).unwrap_or_default(),
        },
        other => bail!("unknown effect `{other}` (try: none, static, spectrum, wave, reactive, breathing, starlight)"),
    };
    Ok(LightingState {
        effect,
        brightness,
    })
}

/// Parse `RRGGBB` (optionally `#RRGGBB`) into an [`Rgb`].
fn parse_hex(s: &str) -> Result<Rgb> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.is_ascii() {
        bail!("color must be 6 hex digits (RRGGBB), got `{s}`");
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16);
    Ok(Rgb::new(
        byte(0).context("bad red")?,
        byte(2).context("bad green")?,
        byte(4).context("bad blue")?,
    ))
}

/// Print a response in a readable form, exiting non-zero on `Error`.
fn print_response(resp: Response) {
    match resp {
        Response::Error { message } => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        other => {
            // Pretty JSON is the most faithful rendering for a scriptable CLI.
            println!(
                "{}",
                serde_json::to_string_pretty(&other).unwrap_or_else(|_| format!("{other:?}"))
            );
        }
    }
}

/// Connect, send one request, read one response (newline-delimited JSON).
async fn send(socket: &Path, request: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket).await.with_context(|| {
        format!(
            "connecting to daemon at {} — is it running? (`tartarus run`)",
            socket.display()
        )
    })?;
    let (read_half, mut write_half) = stream.into_split();
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    let mut reader = BufReader::new(read_half).lines();
    match reader.next_line().await? {
        Some(resp) => Ok(serde_json::from_str(&resp)?),
        None => bail!("daemon closed the connection without replying"),
    }
}

/// Run the `tartarus-daemon` binary with `args`, inheriting stdio. Prefers a
/// sibling binary next to this one, falling back to `PATH`.
fn run_daemon(args: &[String]) -> Result<()> {
    let program = sibling_daemon().unwrap_or_else(|| PathBuf::from("tartarus-daemon"));
    let status = ProcCommand::new(&program)
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "running `{}` — is tartarus-daemon installed / on PATH?",
                program.display()
            )
        })?;
    std::process::exit(status.code().unwrap_or(1));
}

fn sibling_daemon() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join("tartarus-daemon");
    candidate.exists().then_some(candidate)
}
