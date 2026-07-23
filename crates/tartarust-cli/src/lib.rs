//! `tartarust` — command-line client for the daemon.
//!
//! Lib-only crate: the whole binary lives here behind [`main`], and the
//! workspace's root package exposes it as the `tartarust` bin via a thin
//! `src/bin/` shim, so one `cargo install --path .` ships every binary.
//!
//! Two families of subcommands:
//!   * **Device-owner passthroughs** (`discover`, `discover-keys`, `run`) shell
//!     out to the `tartarust-daemon` binary — it is the process that grabs the
//!     device. These need a terminal and are not socket calls.
//!   * **Socket commands** (`ping`, `info`, `profile`, `lighting`, `brightness`)
//!     talk to a running daemon over the `tartarust-ipc` Unix socket (newline-
//!     delimited JSON) and print the reply.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use tartarust_ipc::bundle::ImportSource;
use tartarust_ipc::model::{Effect, LightingState, Rgb};
use tartarust_ipc::{Request, Response};

/// Command-line client for the Razer Tartarus Pro daemon.
#[derive(Debug, Parser)]
#[command(name = "tartarust", version, about)]
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
    /// Export a profile (and the macros it references) as portable TOML.
    Export {
        /// Profile name.
        name: String,
        /// Write to this file instead of stdout (`-` also means stdout).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Commit the daemon's pending session edits to its config file.
    Save,
    /// Import profiles from an export file — or a whole config file — and save.
    Import {
        /// A `tartarust profile export` file, or a full config.toml (e.g.
        /// `~/.config/rusty-tartarus/config.toml` to migrate an old install —
        /// every profile in it is imported). `-` reads stdin.
        file: PathBuf,
        /// Import under a different profile name (export files only).
        #[arg(long)]
        rename: Option<String>,
        /// Overwrite existing same-name profiles/macros instead of failing.
        #[arg(long)]
        force: bool,
        /// Apply for this session only; skip the daemon's save-to-disk step.
        #[arg(long)]
        no_save: bool,
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
        /// Color as a hex string `RRGGBB` (for static/reactive/breathing/starlight).
        #[arg(long)]
        color: Option<String>,
    },
}

/// Binary entry point: parse args, dispatch. Called by the root package's
/// `src/bin/tartarust.rs` shim.
pub fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // Passthroughs.
        Cmd::Discover => run_daemon(&["discover".into()]),
        Cmd::DiscoverKeys => run_daemon(&["discover-keys".into()]),
        Cmd::Run { config } => {
            let mut args = vec![OsString::from("run")];
            if let Some(path) = config {
                args.push("--config".into());
                // OsString end-to-end: paths are arbitrary bytes on Linux, and
                // a lossy conversion here would hand the daemon a mangled path.
                args.push(path.into_os_string());
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
    let socket = tartarust_ipc::socket_path();
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
            ProfileOp::Export { name, output } => export_profile(&socket, name, output).await?,
            ProfileOp::Save => {
                print_response(send(&socket, &Request::SaveProfile { name: None }).await?)
            }
            ProfileOp::Import {
                file,
                rename,
                force,
                no_save,
            } => import_profiles(&socket, &file, rename, force, no_save).await?,
        },
        Cmd::Lighting { op } => match op {
            LightingOp::Get => {
                print_response(send(&socket, &Request::GetLighting { profile: None }).await?)
            }
            LightingOp::Set { effect, color } => {
                // Validate the args BEFORE any round-trip, so a bad --color
                // errors cleanly even with no daemon running.
                let effect = build_effect(&effect, color.as_deref())?;
                // `set` changes only the effect — fetch the profile's current
                // brightness first so it is preserved. Anything but a Lighting
                // reply is fatal: exiting zero here would report success for a
                // SetLighting that was never sent.
                let brightness = match send(&socket, &Request::GetLighting { profile: None })
                    .await?
                {
                    Response::Lighting(state) => state.brightness,
                    Response::Error { message } => bail!("querying current lighting: {message}"),
                    other => {
                        bail!("unexpected daemon reply: {other:?} — daemon/CLI version mismatch?")
                    }
                };
                print_response(
                    send(
                        &socket,
                        &Request::SetLighting {
                            profile: None,
                            lighting: LightingState { effect, brightness },
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

/// `profile export`: fetch the bundle from the daemon and write it as TOML.
/// With no `--output` the TOML goes to stdout (pipe-friendly, nothing else on
/// stdout); with one, a confirmation line is printed instead.
async fn export_profile(socket: &Path, name: String, output: Option<PathBuf>) -> Result<()> {
    match send(socket, &Request::ExportProfile { name: name.clone() }).await? {
        Response::ProfileExport(bundle) => {
            let toml = tartarust_ipc::bundle::to_toml(&bundle).map_err(anyhow::Error::msg)?;
            match output {
                // `-o -` means stdout, mirroring `import -` reading stdin.
                Some(path) if path != Path::new("-") => {
                    std::fs::write(&path, &toml)
                        .with_context(|| format!("writing {}", path.display()))?;
                    println!(
                        "exported profile `{name}` ({} macro(s)) to {}",
                        bundle.macros.len(),
                        path.display()
                    );
                }
                _ => print!("{toml}"),
            }
            Ok(())
        }
        Response::Error { message } => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        other => bail!("unexpected daemon reply: {other:?} — daemon/CLI version mismatch?"),
    }
}

/// `profile import`: parse the file, push the profile(s) into the daemon, and
/// (unless `--no-save`) commit the daemon's config to disk so the import
/// survives a restart — the point of moving profiles between installs.
async fn import_profiles(
    socket: &Path,
    file: &Path,
    rename: Option<String>,
    force: bool,
    no_save: bool,
) -> Result<()> {
    let text = if file == Path::new("-") {
        std::io::read_to_string(std::io::stdin()).context("reading stdin")?
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?
    };
    let entries = match tartarust_ipc::bundle::parse(&text).map_err(anyhow::Error::msg)? {
        ImportSource::Bundle(bundle) => vec![(bundle.profile, bundle.macros)],
        ImportSource::Config(entries) => {
            if rename.is_some() {
                bail!("--rename only applies to single-profile export files");
            }
            entries
        }
    };
    // SaveProfile persists the daemon's ENTIRE live config — including any
    // unrelated session edits the user has deliberately not saved. Snapshot
    // that state up front so the auto-save below can refuse to piggyback them.
    let had_unsaved = match send(socket, &Request::GetStatus).await? {
        Response::Status(status) => status.unsaved_changes,
        Response::Error { message } => bail!("querying daemon status: {message}"),
        other => bail!("unexpected daemon reply: {other:?} — daemon/CLI version mismatch?"),
    };
    // Don't stop at a rejection: with a multi-profile config file, later
    // profiles may import fine — and whatever DID land is live in the daemon,
    // so the save step below must still run or the successes would silently
    // evaporate on the next daemon restart. That is also why a TRANSPORT
    // failure breaks instead of bubbling out with `?`.
    let total = entries.len();
    let mut imported = 0usize;
    let mut failures = Vec::new();
    let mut remaining = entries.into_iter();
    for (profile, macros) in remaining.by_ref() {
        let shown = rename.clone().unwrap_or_else(|| profile.name.clone());
        let macro_count = macros.len();
        match send(
            socket,
            &Request::ImportProfile {
                profile,
                macros,
                rename: rename.clone(),
                overwrite: force,
            },
        )
        .await
        {
            Ok(Response::Ack) => {
                println!("imported profile `{shown}` ({macro_count} macro(s))");
                imported += 1;
            }
            Ok(Response::Error { message }) => {
                eprintln!("error: importing `{shown}`: {message}");
                failures.push(shown);
            }
            Ok(other) => {
                eprintln!(
                    "error: importing `{shown}`: unexpected daemon reply: {other:?} — \
                     daemon/CLI version mismatch?"
                );
                failures.push(shown);
                break;
            }
            Err(e) => {
                eprintln!("error: importing `{shown}`: {e:#}");
                failures.push(shown);
                break;
            }
        }
    }
    if imported > 0 {
        if no_save {
            println!(
                "session-only (--no-save): `tartarust profile save` or the UI's Save keeps it"
            );
        } else if had_unsaved {
            // Saving would commit those unrelated edits too — leave the
            // decision to the user rather than silently widening the write.
            println!(
                "NOT auto-saving: the daemon already holds unsaved session edits, and a save \
                 commits the whole config. Run `tartarust profile save` to persist everything, \
                 or press Save in the UI."
            );
        } else {
            match send(socket, &Request::SaveProfile { name: None }).await {
                Ok(Response::Error { message }) => {
                    bail!("import applied but saving the config failed: {message}")
                }
                Ok(_) => println!("saved to the daemon's config"),
                Err(e) => bail!(
                    "import applied but saving failed: {e:#} — the imported profile(s) are \
                     live but unsaved"
                ),
            }
        }
    }
    // The `break` arms above abandon the loop mid-file: name every profile
    // that was never even sent, so the report accounts for the whole file
    // rather than just the attempted prefix.
    let unattempted: Vec<String> = remaining.map(|(profile, _)| profile.name).collect();
    if !failures.is_empty() || !unattempted.is_empty() {
        let mut msg = format!(
            "{} of {total} profile(s) failed to import: {}",
            failures.len(),
            failures.join(", ")
        );
        if !unattempted.is_empty() {
            msg.push_str(&format!(
                "; {} profile(s) not attempted after the error: {}",
                unattempted.len(),
                unattempted.join(", ")
            ));
        }
        bail!(msg);
    }
    Ok(())
}

/// Build an [`Effect`] from an effect name and optional hex color.
///
/// `--color` is rejected for effects that cannot carry one — silently
/// accepting it would let the user believe the color took effect.
fn build_effect(effect: &str, color: Option<&str>) -> Result<Effect> {
    let parsed_color = color.map(parse_hex).transpose()?;
    let need_color = || parsed_color.unwrap_or(Rgb::new(0, 255, 0));
    Ok(match effect {
        "none" | "spectrum" | "wave" if parsed_color.is_some() => bail!(
            "--color has no effect for `{effect}` (only static, reactive, breathing, starlight take a color)"
        ),
        "none" => Effect::None,
        "spectrum" => Effect::Spectrum,
        "static" => Effect::Static {
            color: need_color(),
        },
        "wave" => Effect::Wave {
            direction: tartarust_ipc::model::WaveDirection::Left,
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
        other => bail!(
            "unknown effect `{other}` (try: none, static, spectrum, wave, reactive, breathing, starlight)"
        ),
    })
}

/// Parse `RRGGBB` (optionally `#RRGGBB`) into an [`Rgb`].
fn parse_hex(s: &str) -> Result<Rgb> {
    let s = s.strip_prefix('#').unwrap_or(s);
    // Require hex digits only — `from_str_radix` alone would also accept a
    // leading `+` per pair, silently misreading typos like `+1+2+3`.
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
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

/// Cap on one full request/reply round-trip in [`send`]. Generous because a
/// degraded-but-alive device can legitimately hold a reply for several seconds
/// (the daemon retries hidraw ioctls with multi-second kernel timeouts).
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect, send one request, read one response (newline-delimited JSON).
///
/// The whole round-trip is bounded by [`SEND_TIMEOUT`]: a wedged daemon (one
/// whose listener still accepts but whose request loop is stuck, e.g. blocked
/// in a device ioctl) must fail the command — `ping` especially — rather than
/// hang it forever. Connect errors still surface immediately with their own
/// context; only a genuine stall hits the timeout.
async fn send(socket: &Path, request: &Request) -> Result<Response> {
    tokio::time::timeout(SEND_TIMEOUT, async {
        let stream = UnixStream::connect(socket).await.with_context(|| {
            format!(
                "connecting to daemon at {} — is it running? (`tartarust run`)",
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
    })
    .await
    .map_err(|_| {
        anyhow!(
            "daemon did not reply within {}s — is it wedged?",
            SEND_TIMEOUT.as_secs()
        )
    })?
}

/// Run the `tartarust-daemon` binary with `args`, inheriting stdio. Prefers a
/// sibling binary next to this one, falling back to `PATH`.
fn run_daemon(args: &[OsString]) -> Result<()> {
    let program = sibling_daemon().unwrap_or_else(|| PathBuf::from("tartarust-daemon"));
    let status = ProcCommand::new(&program)
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "running `{}` — is tartarust-daemon installed / on PATH?",
                program.display()
            )
        })?;
    std::process::exit(status.code().unwrap_or(1));
}

fn sibling_daemon() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let candidate = dir.join("tartarust-daemon");
    candidate.exists().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_accepts_plain_and_hash_prefixed() {
        assert_eq!(parse_hex("a0b0c0").unwrap(), Rgb::new(0xA0, 0xB0, 0xC0));
        assert_eq!(parse_hex("#FF0000").unwrap(), Rgb::new(0xFF, 0x00, 0x00));
    }

    #[test]
    fn parse_hex_rejects_signs_and_non_hex() {
        // `from_str_radix` alone would accept these as Rgb(1,2,3) / Rgb(10,11,12).
        assert!(parse_hex("+1+2+3").is_err());
        assert!(parse_hex("+A0B0C").is_err());
        assert!(parse_hex("gg0000").is_err());
        assert!(parse_hex("12345").is_err());
        assert!(parse_hex("1234567").is_err());
    }

    #[test]
    fn build_effect_rejects_color_for_colorless_effects() {
        for effect in ["none", "spectrum", "wave"] {
            assert!(build_effect(effect, Some("ff0000")).is_err(), "{effect}");
            assert!(build_effect(effect, None).is_ok(), "{effect}");
        }
    }

    #[test]
    fn build_effect_consumes_color_where_it_applies() {
        for effect in ["static", "reactive", "breathing", "starlight"] {
            assert!(build_effect(effect, Some("ff0000")).is_ok(), "{effect}");
        }
        assert!(build_effect("bogus", None).is_err());
    }
}
