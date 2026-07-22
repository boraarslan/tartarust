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

/// Classify + parse import-file text. Export bundles are recognized by their
/// `format` envelope; anything else must look like a config with profiles.
pub fn parse(text: &str) -> Result<ImportSource, String> {
    if let Ok(bundle) = toml::from_str::<ProfileExport>(text) {
        bundle.validate()?;
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
            .filter_map(|n| config.macros.get(n).cloned())
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

    /// A bundle from a NEWER build is refused rather than half-understood.
    #[test]
    fn future_bundle_version_is_rejected() {
        let mut bundle = ProfileExport::new(Profile::new("x"), vec![]);
        bundle.version += 1;
        let text = to_toml(&bundle).unwrap();
        assert!(parse(&text).unwrap_err().contains("newer"));
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
        let ImportSource::Config(entries) = parse(text).unwrap() else {
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
        assert!(parse("not even toml [").is_err());
        assert!(
            parse("auto_switch = true\n")
                .unwrap_err()
                .contains("no profiles")
        );
    }
}
