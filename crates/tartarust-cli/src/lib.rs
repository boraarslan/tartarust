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

use std::path::{Path, PathBuf};
use std::process::Command as ProcCommand;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use tartarust_ipc::model::{Effect, LightingState, Rgb};
use tartarust_ipc::{Macro, Profile, ProfileExport, Request, Response, referenced_macro_names};

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
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
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
        /// Color as a hex string `RRGGBB` (for static/reactive/breathing).
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

/// `profile export`: fetch the bundle from the daemon and write it as TOML.
/// With no `--output` the TOML goes to stdout (pipe-friendly, nothing else on
/// stdout); with one, a confirmation line is printed instead.
async fn export_profile(socket: &Path, name: String, output: Option<PathBuf>) -> Result<()> {
    match send(socket, &Request::ExportProfile { name: name.clone() }).await? {
        Response::ProfileExport(bundle) => {
            let toml = toml::to_string_pretty(&bundle).context("encoding profile bundle")?;
            match output {
                Some(path) => {
                    std::fs::write(&path, &toml)
                        .with_context(|| format!("writing {}", path.display()))?;
                    println!(
                        "exported profile `{name}` ({} macro(s)) to {}",
                        bundle.macros.len(),
                        path.display()
                    );
                }
                None => print!("{toml}"),
            }
            Ok(())
        }
        other => {
            print_response(other); // exits non-zero on Error
            Ok(())
        }
    }
}

/// What an import file turned out to contain.
#[derive(Debug)]
enum ImportFile {
    /// A `tartarust profile export` bundle: one profile + its macros.
    Bundle(ProfileExport),
    /// A full daemon config (e.g. an old install's config.toml): every profile,
    /// each paired with the macros its binds reference.
    Config(Vec<(Profile, Vec<Macro>)>),
}

/// The subset of a daemon config.toml an import needs. Tolerant of every other
/// field (default_profile, auto_switch, analog, ...) — and identical between
/// tartarust and old rusty-tartarus configs, which is what makes
/// `profile import ~/.config/rusty-tartarus/config.toml` a migration path.
#[derive(serde::Deserialize)]
struct ConfigProfiles {
    #[serde(default)]
    profiles: std::collections::BTreeMap<String, Profile>,
    #[serde(default)]
    macros: std::collections::BTreeMap<String, Macro>,
}

/// Classify + parse import-file text. Export bundles are recognized by their
/// `format` envelope; anything else must look like a config with profiles.
fn parse_import(text: &str) -> Result<ImportFile> {
    if let Ok(bundle) = toml::from_str::<ProfileExport>(text) {
        bundle.validate().map_err(anyhow::Error::msg)?;
        return Ok(ImportFile::Bundle(bundle));
    }
    let config: ConfigProfiles = toml::from_str(text)
        .context("file is neither a profile export bundle nor a parseable config")?;
    if config.profiles.is_empty() {
        bail!("file has no profiles to import (not an export bundle, and no [profiles.*] tables)");
    }
    let mut entries = Vec::new();
    for (key, mut profile) in config.profiles {
        // The map key is authoritative in a config file (it is what the daemon
        // keys profiles by); trust it over any stale inline `name`.
        profile.name = key;
        let macros = referenced_macro_names(&profile)
            .iter()
            .filter_map(|n| config.macros.get(n).cloned())
            .collect();
        entries.push((profile, macros));
    }
    Ok(ImportFile::Config(entries))
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
    let entries = match parse_import(&text)? {
        ImportFile::Bundle(bundle) => vec![(bundle.profile, bundle.macros)],
        ImportFile::Config(entries) => {
            if rename.is_some() {
                bail!("--rename only applies to single-profile export files");
            }
            entries
        }
    };
    for (profile, macros) in entries {
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
        .await?
        {
            Response::Error { message } => {
                bail!("importing `{shown}`: {message}");
            }
            _ => println!("imported profile `{shown}` ({macro_count} macro(s))"),
        }
    }
    if no_save {
        println!("session-only (--no-save): save from the UI or re-run without --no-save to keep");
    } else if let Response::Error { message } =
        send(socket, &Request::SaveProfile { name: None }).await?
    {
        bail!("import applied but saving the config failed: {message}");
    } else {
        println!("saved to the daemon's config");
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
    };
    Ok(LightingState { effect, brightness })
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
}

/// Run the `tartarust-daemon` binary with `args`, inheriting stdio. Prefers a
/// sibling binary next to this one, falling back to `PATH`.
fn run_daemon(args: &[String]) -> Result<()> {
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
    use tartarust_ipc::model::{BindAction, MacroStep};

    /// An exported bundle survives the TOML write→parse round trip intact.
    #[test]
    fn bundle_roundtrips_through_toml() {
        let mut profile = Profile::new("grind");
        profile.base.insert(
            "KEY_A".into(),
            BindAction::Macro {
                name: "burst".into(),
            },
        );
        let bundle = ProfileExport::new(
            profile,
            vec![Macro {
                name: "burst".into(),
                steps: vec![
                    MacroStep::Press {
                        key: "KEY_1".into(),
                    },
                    MacroStep::Delay { ms: 30 },
                ],
            }],
        );
        let text = toml::to_string_pretty(&bundle).unwrap();
        let ImportFile::Bundle(back) = parse_import(&text).unwrap() else {
            panic!("an export must parse back as a bundle");
        };
        assert_eq!(back, bundle);
    }

    /// A bundle from a NEWER build is refused rather than half-understood.
    #[test]
    fn future_bundle_version_is_rejected() {
        let mut bundle = ProfileExport::new(Profile::new("x"), vec![]);
        bundle.version += 1;
        let text = toml::to_string_pretty(&bundle).unwrap();
        assert!(
            parse_import(&text)
                .unwrap_err()
                .to_string()
                .contains("newer")
        );
    }

    /// A full config file (tartarust or old rusty-tartarus) imports as one
    /// entry per profile, each paired with only the macros its binds reference;
    /// the `[profiles.X]` table key overrides a mismatched inline `name`.
    #[test]
    fn config_file_yields_profiles_with_their_referenced_macros() {
        let text = r#"
default_profile = "a"
auto_switch = true

[profiles.a]
name = "stale-name"
[profiles.a.base.KEY_A]
type = "macro"
name = "used"

[profiles.b]
name = "b"

[macros.used]
name = "used"
[[macros.used.steps]]
step = "delay"
ms = 5

[macros.unused]
name = "unused"
steps = []
"#;
        let ImportFile::Config(entries) = parse_import(text).unwrap() else {
            panic!("a config file must parse as a multi-profile import");
        };
        let names: Vec<_> = entries.iter().map(|(p, _)| p.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"], "table key wins over inline name");
        assert_eq!(
            entries[0]
                .1
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            vec!["used"]
        );
        assert!(entries[1].1.is_empty());
    }

    /// Neither a bundle nor a config with profiles → a clear error.
    #[test]
    fn unrecognized_files_are_rejected() {
        assert!(parse_import("not even toml [").is_err());
        assert!(
            parse_import("auto_switch = true\n")
                .unwrap_err()
                .to_string()
                .contains("no profiles")
        );
    }
}
