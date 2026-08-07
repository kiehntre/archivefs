//! The persisted DAT matching policy, as it sits inside
//! `~/.config/archivefs/dat_sources.toml`.
//!
//! # Deliberately tolerant, like the file it lives in
//!
//! The DAT sources file is built to keep what it does not understand:
//! unknown keys are captured with `#[serde(flatten)]` and re-emitted verbatim
//! on save. This policy table follows the same rule, and it goes one step
//! further. Preference *values* that a newer build could add (a new revision
//! policy name, a new language code, a new region) are stored as raw strings,
//! not enums, so a value this build does not know is carried through a
//! load/edit/save cycle untouched instead of failing the parse of the whole
//! file. [`super::evaluate`] parses those strings into the typed vocabulary
//! and reports anything it cannot understand as a *problem* - the value is
//! preserved, the resolution simply ignores it.
//!
//! # No format version
//!
//! There is deliberately no `format_version`, for the same reason the
//! registry has none: a reader that preserves what it does not know has
//! nothing to misinterpret, so a version key would have no consumer. This PR
//! does not cross the migration one-way door.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The whole DAT matching policy document.
///
/// Every field is optional: an absent field means "no preference, use the
/// safe default", exactly the `Option` distinction the registry's own fields
/// use. The document answers *one* question - how verified DAT candidates are
/// preferred - and lives in the one file that owns the DAT sources, so there
/// is no second policy document to drift.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DatPolicyConfig {
    /// Preferred regions in order. Each entry is a canonical region id
    /// (`world`, `usa`, `japan`, `europe`, `other`). Empty = no preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_preferences: Option<Vec<String>>,

    /// Preferred languages in order. Each entry is a language id (`en`,
    /// `ja`, …) or one of the special values `multi` / `original`. Empty =
    /// no preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_preferences: Option<Vec<String>>,

    /// The revision policy, as its stable name (`latest_verified`,
    /// `earliest_verified`, `prefer_original`, `ask_when_ambiguous`). Kept as
    /// a string so an unknown value round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_policy: Option<String>,

    /// The clone policy, as its stable name (`prefer_parent`,
    /// `prefer_clone`, `keep_all_variants`, `require_explicit_choice`). Kept
    /// as a string so an unknown value round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_policy: Option<String>,

    /// Per-platform overrides, keyed by **canonical platform id**. A key this
    /// build cannot canonicalise is a validation problem and is ignored for
    /// resolution, but is preserved verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<BTreeMap<String, DatPlatformPolicyConfig>>,

    /// Keys a newer build wrote that this one does not define, kept verbatim
    /// so saving from this build does not delete them.
    #[serde(flatten)]
    pub unknown_fields: toml::Table,
}

/// One per-platform override of the global policy.
///
/// Field semantics are the same as the global document's, with one extra
/// distinction: `None` means "inherit from the global scope", while
/// `Some([])` for a list means "no preference for this platform, do not
/// inherit". This is the model document's `Option<Vec<_>>` rule (§7.2).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DatPlatformPolicyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_preferences: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_preferences: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_policy: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clone_policy: Option<String>,

    #[serde(flatten)]
    pub unknown_fields: toml::Table,
}

/// A policy document with nothing set: every preference absent. This is the
/// persisted shape of "the safe defaults apply".
pub fn default_dat_policy() -> DatPolicyConfig {
    DatPolicyConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_policy_serialises_to_nothing_meaningful() {
        let text = toml::to_string_pretty(&DatPolicyConfig::default()).unwrap();
        // No preference keys, no version key, no empty tables.
        assert!(!text.contains("region_preferences"), "{text}");
        assert!(!text.contains("revision_policy"), "{text}");
        assert!(!text.contains("format_version"), "{text}");
    }

    #[test]
    fn policy_round_trips_through_toml() {
        let policy = DatPolicyConfig {
            region_preferences: Some(vec!["europe".into(), "usa".into()]),
            language_preferences: Some(vec!["en".into(), "multi".into()]),
            revision_policy: Some("latest_verified".into()),
            clone_policy: Some("prefer_parent".into()),
            platforms: Some(BTreeMap::from([(
                "NES".to_string(),
                DatPlatformPolicyConfig {
                    region_preferences: Some(vec!["japan".into()]),
                    ..Default::default()
                },
            )])),
            unknown_fields: toml::Table::new(),
        };
        let text = toml::to_string_pretty(&policy).unwrap();
        let back: DatPolicyConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, policy);
    }

    #[test]
    fn unknown_policy_keys_survive_a_round_trip() {
        let raw = r#"
region_preferences = ["europe"]
future_policy_key = "kept"

[future_table]
setting = 3
"#;
        let policy: DatPolicyConfig = toml::from_str(raw).unwrap();
        assert_eq!(policy.region_preferences, Some(vec!["europe".to_string()]));
        assert_eq!(
            policy.unknown_fields.get("future_policy_key"),
            Some(&toml::Value::String("kept".into()))
        );
        assert_nested_setting(&policy.unknown_fields);
        let text = toml::to_string_pretty(&policy).unwrap();
        let back: DatPolicyConfig = toml::from_str(&text).unwrap();
        assert_eq!(
            back.unknown_fields.get("future_policy_key"),
            Some(&toml::Value::String("kept".into()))
        );
        assert_nested_setting(&back.unknown_fields);
    }

    /// Asserts the flattened `future_table` sub-table kept its own scalar.
    fn assert_nested_setting(unknown: &toml::Table) {
        let Some(toml::Value::Table(table)) = unknown.get("future_table") else {
            panic!("future_table must survive as a sub-table");
        };
        assert_eq!(
            table.get("setting"),
            Some(&toml::Value::Integer(3)),
            "{table:?}"
        );
    }
}
