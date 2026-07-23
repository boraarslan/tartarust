//! Profile-bundle file I/O (the opt-in `bundle` feature).
//!
//! The shared encode/classify/parse logic behind `tartarust profile
//! export`/`import` and the UI's export/import flow — one implementation so the
//! two clients can never drift on what an import file means. Errors are plain
//! `String` messages, matching the crate's message-oriented style
//! ([`crate::Response::Error`]).

use std::collections::BTreeMap;

use crate::model::{Macro, Profile, ProfileExport, referenced_macro_names};

/// What an import file turned out to contain.
#[derive(Debug)]
pub enum ImportSource {
    /// A `tartarust profile export` bundle: one profile + its macros.
    Bundle(ProfileExport),
    /// A full daemon config (e.g. an old install's config.toml): every profile,
    /// each paired with the macros its binds reference.
    Config(Vec<(Profile, Vec<Macro>)>),
}

/// The subset of a daemon config.toml an import needs. Tolerant of every other
/// field (default_profile, auto_switch, analog, ...) — and identical between
/// tartarust and old rusty-tartarus configs, which is what makes importing
/// `~/.config/rusty-tartarus/config.toml` a migration path.
#[derive(serde::Deserialize)]
struct ConfigProfiles {
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    macros: BTreeMap<String, Macro>,
}

/// Encode a bundle as pretty TOML (the export-file format).
pub fn to_toml(bundle: &ProfileExport) -> Result<String, String> {
    toml::to_string_pretty(bundle).map_err(|e| format!("encoding profile bundle: {e}"))
}

/// The bare bundle envelope, parseable from ANY valid TOML table. Probing this
/// first lets a malformed or newer-than-this-build bundle report its own error
/// (and fire the version gate) instead of falling through to a misleading
/// config-parse failure.
#[derive(serde::Deserialize)]
struct Envelope {
    format: Option<String>,
    #[serde(default)]
    version: Option<u32>,
}

/// Classify + parse import-file text. Export bundles are recognized by their
/// `format` envelope; anything else must look like a config with profiles.
pub fn parse(text: &str) -> Result<ImportSource, String> {
    let envelope: Envelope = toml::from_str(text).map_err(|e| format!("invalid TOML: {e}"))?;
    if let Some(format) = envelope.format {
        // A `format` key means the file claims to be a bundle: gate on the
        // envelope FIRST (a newer build's bundle may not deserialize at all),
        // then surface the real parse error rather than the config fallback's.
        // A missing version is NOT assumed current — that would let an
        // unversioned (hand-mangled or future) bundle skip the gate.
        let Some(version) = envelope.version else {
            return Err("bundle is missing its `version` field (a hand-edit removed it?)".into());
        };
        ProfileExport::check_envelope(&format, version)?;
        let bundle: ProfileExport =
            toml::from_str(text).map_err(|e| format!("invalid profile bundle: {e}"))?;
        bundle.validate()?;
        // Bundled macros must satisfy the shared structural rules (e.g. the
        // delay cap) so an import can never smuggle in a macro the daemon
        // could not persist. The daemon re-checks at its boundary; failing
        // here just surfaces the error at parse time with the file in hand.
        for m in &bundle.macros {
            m.validate()?;
        }
        return Ok(ImportSource::Bundle(bundle));
    }
    let config: ConfigProfiles = toml::from_str(text).map_err(|e| {
        format!("file is neither a profile export bundle nor a parseable config: {e}")
    })?;
    if config.profiles.is_empty() {
        return Err(
            "file has no profiles to import (not an export bundle, and no [profiles.*] tables)"
                .into(),
        );
    }
    let mut entries = Vec::new();
    for (key, mut profile) in config.profiles {
        // The map key is authoritative in a config file (it is what the daemon
        // keys profiles by); trust it over any stale inline `name`.
        profile.name = key;
        let macros = referenced_macro_names(&profile)
            .iter()
            .filter_map(|n| {
                // Same rule for macros: binds reference the map key, so the
                // bundled macro must carry it — a mismatched inline `name`
                // would import as a dangling reference.
                config.macros.get(n).cloned().map(|mut m| {
                    m.name = n.clone();
                    m
                })
            })
            .collect();
        entries.push((profile, macros));
    }
    Ok(ImportSource::Config(entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BindAction, MacroStep};

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
        let text = to_toml(&bundle).unwrap();
        let ImportSource::Bundle(back) = parse(&text).unwrap() else {
            panic!("an export must parse back as a bundle");
        };
        assert_eq!(back, bundle);
    }

    /// A bundle macro with an over-cap delay is refused at parse time. Values
    /// in (MAX_MACRO_DELAY_MS, i64::MAX] ARE TOML-representable, so such a
    /// file can exist on disk — but importing it would hand the daemon a macro
    /// it refuses (and, unchecked, one that would wedge whole-config saves).
    #[test]
    fn bundle_with_over_cap_delay_is_rejected() {
        let bundle = ProfileExport::new(
            Profile::new("x"),
            vec![Macro {
                name: "stall".into(),
                steps: vec![MacroStep::Delay {
                    ms: crate::model::MAX_MACRO_DELAY_MS + 1,
                }],
            }],
        );
        let text = to_toml(&bundle).unwrap();
        let err = parse(&text).unwrap_err();
        assert!(
            err.contains("delay"),
            "want the delay-cap error, got: {err}"
        );
    }

    /// A bundle from a NEWER build is refused rather than half-understood —
    /// even when its body no longer deserializes as this build's shape, the
    /// envelope probe must fire the version gate first.
    #[test]
    fn future_bundle_version_is_rejected() {
        let mut bundle = ProfileExport::new(Profile::new("x"), vec![]);
        bundle.version += 1;
        let text = to_toml(&bundle).unwrap();
        assert!(parse(&text).unwrap_err().contains("newer"));

        // Same gate with a body this build cannot even parse.
        let alien = format!(
            "format = \"tartarust-profile\"\nversion = {}\n[profile]\nname = \"x\"\n[[profile.widgets]]\nkind = \"from-the-future\"\n",
            crate::model::PROFILE_EXPORT_VERSION + 1
        );
        assert!(parse(&alien).unwrap_err().contains("newer"));
    }

    /// A file claiming to be a bundle gets the BUNDLE parse error on a bad
    /// body — not the config fallback's misleading complaint.
    #[test]
    fn malformed_bundle_reports_a_bundle_error() {
        let text = r#"
format = "tartarust-profile"
version = 1
[profile]
name = "x"
[profile.base.KEY_A]
step = "not-a-bind-action"
"#;
        let err = parse(text).unwrap_err();
        assert!(
            err.contains("invalid profile bundle"),
            "want the bundle parse error, got: {err}"
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
name = "stale-macro-name"
[[macros.used.steps]]
step = "delay"
ms = 5

[macros.unused]
name = "unused"
steps = []
"#;
        let ImportSource::Config(entries) = parse(text).unwrap() else {
            panic!("a config file must parse as a multi-profile import");
        };
        let names: Vec<_> = entries.iter().map(|(p, _)| p.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"], "table key wins over inline name");
        // The macro's stale inline name is normalized to its map key too —
        // that key is what the bind references.
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

    /// A bundle without a `version` field must not skip the gate by defaulting
    /// to the current version.
    #[test]
    fn missing_bundle_version_is_rejected() {
        let text = "format = \"tartarust-profile\"\n[profile]\nname = \"x\"\n";
        assert!(parse(text).unwrap_err().contains("version"));
    }

    /// Hand-written config tables may omit the redundant inline `name` — the
    /// map key is authoritative and fills it in for profiles AND macros.
    #[test]
    fn config_tables_may_omit_inline_names() {
        let text = r#"
[profiles.game]
[profiles.game.base.KEY_A]
type = "macro"
name = "burst"

[macros.burst]
[[macros.burst.steps]]
step = "delay"
ms = 5
"#;
        let ImportSource::Config(entries) = parse(text).unwrap() else {
            panic!("nameless tables must still parse as a config import");
        };
        assert_eq!(entries[0].0.name, "game");
        assert_eq!(
            entries[0]
                .1
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            vec!["burst"]
        );
    }

    /// Neither a bundle nor a config with profiles → a clear error.
    #[test]
    fn unrecognized_files_are_rejected() {
        assert!(parse("not even toml [").is_err());
        assert!(
            parse("auto_switch = true\n")
                .unwrap_err()
                .contains("no profiles")
        );
    }
}
