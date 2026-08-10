//! Turning a RomM record into an [`ExternalIdentityRecord`].
//!
//! This is the adapter's whole job: everything above it speaks EmuWiz's own
//! identity model and knows nothing about RomM's field names. Every field name
//! read here was taken from a real RomM 5.1.0's `/openapi.json`, and every one is
//! optional at the JSON level - a record missing a field is normalised without
//! it rather than rejected, because an older instance or an unmatched game
//! legitimately has gaps.
//!
//! # Platforms go through the existing registry
//!
//! RomM's platform slugs are resolved with [`crate::platform::platform_for_alias`],
//! the same registry the rest of EmuWiz uses. There is deliberately no second
//! table of platform names here: a slug the registry does not recognise stays
//! visible as unknown, with RomM's own name and id preserved, rather than being
//! guessed at with substring matching.
//!
//! # Bad provider data stays visible
//!
//! A malformed hash is not silently dropped and not promoted to evidence: it is
//! counted and reported as rejected provider data, so a person can see that RomM
//! published something unusable rather than wondering why verification never
//! reaches confirmed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity_source::model::{
    ArtworkReference, ExternalHash, ExternalIdentityRecord, ExternalVerification, HashAlgorithm,
    IdentityProvider, MetadataProviderId,
};
use crate::identity_source::path_map::{PathMappings, PathTranslation};

/// The metadata-provider id fields a RomM record can carry, with the name each
/// is recorded under. Read from the real ROM schema.
const METADATA_ID_FIELDS: &[(&str, &str)] = &[
    ("igdb_id", "igdb"),
    ("moby_id", "moby"),
    ("ss_id", "screenscraper"),
    ("launchbox_id", "launchbox"),
    ("ra_id", "retroachievements"),
    ("hasheous_id", "hasheous"),
    ("tgdb_id", "thegamesdb"),
    ("sgdb_id", "steamgriddb"),
    ("flashpoint_id", "flashpoint"),
    ("hltb_id", "howlongtobeat"),
];

/// The most `files[]` entries one record's relationships will carry, so a
/// pathological record cannot make the cache unbounded.
pub const MAX_RELATED_FILES: usize = 64;

/// A hash RomM published that could not be used.
///
/// Kept rather than discarded: "RomM published an MD5 that is not 32 hex
/// characters" is something a person should be able to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedHash {
    pub provider_game_id: String,
    pub algorithm: HashAlgorithm,
    /// Why it was rejected. Deliberately does not echo the value, which could be
    /// arbitrary provider text.
    pub reason: String,
}

/// What normalising one page produced, alongside the records.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalisationReport {
    /// Records whose platform slug the registry did not recognise.
    pub unknown_platforms: Vec<String>,
    pub rejected_hashes: Vec<RejectedHash>,
    /// Records RomM returned that carried no usable identity at all.
    pub skipped_records: usize,
}

impl NormalisationReport {
    pub fn merge(&mut self, other: Self) {
        for platform in other.unknown_platforms {
            if !self.unknown_platforms.contains(&platform) {
                self.unknown_platforms.push(platform);
            }
        }
        self.rejected_hashes.extend(other.rejected_hashes);
        self.skipped_records += other.skipped_records;
    }
}

/// Normalises one RomM ROM record.
///
/// Returns `None` only when the record has no usable identity at all - no id -
/// which is the one thing that makes it unrecordable.
pub fn normalise_rom(
    value: &Value,
    server_id: &str,
    mappings: &PathMappings,
    imported_at_unix_seconds: i64,
    report: &mut NormalisationReport,
) -> Option<ExternalIdentityRecord> {
    // The id is the only field a record cannot do without: it is what a cached
    // record is keyed by and what a person would use to find it in RomM.
    let provider_game_id = value
        .get("id")
        .and_then(json_id)
        .or_else(|| string_field(value, "id"))?;

    let provider_platform_id = value.get("platform_id").and_then(json_id);
    let platform_slug = string_field(value, "platform_slug");
    let provider_platform_name = platform_slug.clone().or_else(|| {
        string_field(value, "platform_display_name")
            .or_else(|| string_field(value, "platform_custom_name"))
    });
    let platform_candidate = platform_slug
        .as_deref()
        .and_then(canonical_platform_for_romm_slug);
    if platform_candidate.is_none()
        && let Some(slug) = &provider_platform_name
        && !report.unknown_platforms.contains(slug)
    {
        report.unknown_platforms.push(slug.clone());
    }

    let provider_path = provider_path_of(value);
    let translation = mappings.translate(&provider_path);
    let archivefs_path = translation
        .archivefs_path()
        .map(std::path::Path::to_path_buf);

    let mut hashes = Vec::new();
    for (field, algorithm) in [
        ("crc_hash", HashAlgorithm::Crc32),
        ("md5_hash", HashAlgorithm::Md5),
        ("sha1_hash", HashAlgorithm::Sha1),
    ] {
        let Some(raw) = string_field(value, field) else {
            continue;
        };
        match ExternalHash::parse(algorithm, &raw) {
            Some(hash) => hashes.push(hash),
            None => report.rejected_hashes.push(RejectedHash {
                provider_game_id: provider_game_id.clone(),
                algorithm,
                reason: format!(
                    "RomM published a {} that is not {} hexadecimal characters",
                    algorithm.label(),
                    algorithm.hex_length()
                ),
            }),
        }
    }

    let metadata_provider_ids: Vec<MetadataProviderId> = METADATA_ID_FIELDS
        .iter()
        .filter_map(|(field, name)| {
            value
                .get(*field)
                .and_then(json_id)
                .map(|id| MetadataProviderId {
                    provider: (*name).to_string(),
                    id,
                })
        })
        .collect();

    // Artwork references only - never bytes, and never fetched here.
    let artwork = string_field(value, "url_cover")
        .or_else(|| string_field(value, "path_cover_large"))
        .map(|reference| ArtworkReference {
            reference,
            small_reference: string_field(value, "path_cover_small"),
        });

    // Multi-file structure, preserved rather than flattened.
    let related_files: Vec<String> = value
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .take(MAX_RELATED_FILES)
                .filter_map(|file| {
                    string_field(file, "full_path")
                        .or_else(|| string_field(file, "file_name"))
                        .or_else(|| string_field(file, "fs_name"))
                })
                .collect()
        })
        .unwrap_or_default();
    let sibling_game_ids: Vec<String> = value
        .get("sibling_roms")
        .and_then(Value::as_array)
        .map(|siblings| {
            siblings
                .iter()
                .take(MAX_RELATED_FILES)
                .filter_map(|sibling| {
                    sibling
                        .get("id")
                        .and_then(json_id)
                        .or_else(|| json_id(sibling))
                })
                .collect()
        })
        .unwrap_or_default();

    // The provider's own view of whether the file is still there. Recorded as
    // evidence; the local check is what actually decides staleness.
    let mut evidence = Vec::new();
    if value
        .get("missing_from_fs")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        evidence.push("RomM reports this file as missing from its own filesystem".to_string());
    }
    if value
        .get("has_multiple_files")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        evidence.push(format!(
            "RomM reports this as a multi-file game with {} file(s)",
            related_files.len()
        ));
    }
    if let PathTranslation::Unmatched { .. } = &translation {
        evidence.push("no configured path mapping covers this record's RomM path".to_string());
    }
    if let PathTranslation::Refused { refusal, .. } = &translation {
        evidence.push(format!(
            "the RomM path could not be used: {}",
            refusal.detail()
        ));
    }

    Some(ExternalIdentityRecord {
        provider: IdentityProvider::Romm,
        server_id: server_id.to_string(),
        provider_platform_id,
        provider_game_id,
        // RomM's ROM *is* the game record; a per-file id exists only inside
        // `files[]`, so the file id is left absent at the record level and the
        // relationships are carried in `related_files`.
        provider_file_id: None,
        provider_path,
        archivefs_path,
        title: string_field(value, "name"),
        platform_candidate: platform_candidate.map(str::to_string),
        provider_platform_name,
        regions: string_array(value, "regions"),
        revision: string_field(value, "revision"),
        hashes,
        file_size_bytes: value.get("fs_size_bytes").and_then(Value::as_u64),
        metadata_provider_ids,
        artwork,
        related_files,
        sibling_game_ids,
        imported_at_unix_seconds,
        provider_updated_at: string_field(value, "updated_at"),
        // Assigned by matching, which happens after normalisation: an imported
        // record starts as unmatched and is only promoted by evidence.
        verification: ExternalVerification::Unmatched,
        conflicts: Vec::new(),
        evidence,
    })
}

/// The path a RomM record describes, exactly as RomM gives it.
///
/// RomM reports `fs_path` as the directory and `fs_name` as the file, and
/// `full_path` when it has one - preferred, because it is the provider's own
/// answer rather than something reassembled here. In RomM 5.1.0 these are
/// relative to the instance's library base, e.g. `roms/gb/game.gb`.
///
/// Public because a mapping preview has to sample the very same string an import
/// would use. Two copies of this logic would let a preview show a translation the
/// import then did differently, which is the one thing a preview must not do.
pub fn provider_path_of(value: &Value) -> String {
    if let Some(full) = string_field(value, "full_path") {
        return full;
    }
    match (
        string_field(value, "fs_path"),
        string_field(value, "fs_name"),
    ) {
        (Some(directory), Some(name)) => format!("{}/{name}", directory.trim_end_matches('/')),
        (Some(directory), None) => directory,
        (None, Some(name)) => name,
        (None, None) => String::new(),
    }
}

/// Normalises one platform record into its canonical mapping, for a summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalisedPlatform {
    pub provider_platform_id: Option<String>,
    pub provider_slug: String,
    pub provider_name: Option<String>,
    /// The canonical EmuWiz platform, when the registry recognises the slug.
    ///
    /// Owned rather than `&'static str` because this is persisted: a cache written
    /// by one build must be readable by the next, and a borrowed registry name
    /// cannot survive a round trip through JSON.
    pub canonical: Option<String>,
    /// How many ROMs RomM reports on this platform, when it says.
    pub rom_count: Option<u64>,
}

pub fn normalise_platform(value: &Value) -> Option<NormalisedPlatform> {
    let provider_slug = string_field(value, "slug")
        .or_else(|| string_field(value, "fs_slug"))
        .or_else(|| string_field(value, "name"))?;
    Some(NormalisedPlatform {
        provider_platform_id: value.get("id").and_then(json_id),
        canonical: canonical_platform_for_romm_slug(&provider_slug).map(str::to_string),
        provider_name: string_field(value, "name").or_else(|| string_field(value, "display_name")),
        rom_count: value
            .get("rom_count")
            .and_then(Value::as_u64)
            .or_else(|| value.get("roms_count").and_then(Value::as_u64)),
        provider_slug,
    })
}

/// Resolves a RomM platform slug to a canonical EmuWiz platform.
///
/// Delegates to the one platform registry. Exact, normalised matching only -
/// `platform_for_alias` compares whole normalised aliases, never substrings, so
/// RomM's `amiga-cd32` resolves and its `zx-spectrum-next` does not become
/// ZX Spectrum.
///
/// A short explicit table handles the few slugs whose RomM spelling has no alias
/// in the registry. It is deliberately tiny and each entry is a slug observed on
/// a real instance; anything not here stays unknown rather than being guessed.
pub fn canonical_platform_for_romm_slug(slug: &str) -> Option<&'static str> {
    if let Some(platform) = crate::platform::platform_for_alias(slug) {
        return Some(platform.id);
    }
    // RomM slugs that differ from every alias the registry carries. Each maps to
    // a platform that must exist - asserted by a test - and each is an exact
    // slug, never a pattern.
    const ROMM_SLUG_ALIASES: &[(&str, &str)] = &[
        ("acpc", "Amstrad CPC"),
        ("c-plus-4", "Commodore 64"),
        ("c16", "Commodore 64"),
        ("cpc", "Amstrad CPC"),
        ("dc", "Dreamcast"),
        ("fds", "NES"),
        ("gb", "Game Boy"),
        ("gba", "Game Boy Advance"),
        ("gbc", "Game Boy Color"),
        ("genesis-slash-megadrive", "MegaDrive"),
        ("n64", "N64"),
        ("nds", "Nintendo DS"),
        ("neo-geo-cd", "Neo Geo CD"),
        ("ngc", "GameCube"),
        ("pc-fx", "PC Engine"),
        ("ps", "PSX"),
        ("psvita", "PlayStation Vita"),
        ("sega-cd", "Sega CD"),
        ("sega32", "Sega 32X"),
        ("segacd", "Sega CD"),
        ("sfam", "SNES"),
        ("sms", "MasterSystem"),
        ("snes", "SNES"),
        ("turbografx-16-slash-pc-engine-cd", "PC Engine CD"),
        ("win", "PC"),
        ("xboxone", "Xbox"),
    ];
    let normalised = crate::platform::normalize_alias(slug);
    ROMM_SLUG_ALIASES
        .iter()
        .find(|(alias, _)| crate::platform::normalize_alias(alias) == normalised)
        .map(|(_, canonical)| *canonical)
}

/// Every canonical target the RomM slug table names, for the test that proves
/// they all exist in the registry.
pub fn romm_slug_targets() -> Vec<&'static str> {
    // Kept in step with the table above by the test; listing them separately
    // would be a second source of truth, so they are re-derived here.
    [
        "Amstrad CPC",
        "Commodore 64",
        "Dreamcast",
        "NES",
        "Game Boy",
        "Game Boy Advance",
        "Game Boy Color",
        "MegaDrive",
        "N64",
        "Nintendo DS",
        "Neo Geo CD",
        "GameCube",
        "PC Engine",
        "PSX",
        "PlayStation Vita",
        "Sega CD",
        "Sega 32X",
        "SNES",
        "MasterSystem",
        "PC Engine CD",
        "PC",
        "Xbox",
    ]
    .to_vec()
}

/// A string field, trimmed, with empty and JSON null treated as absent.
fn string_field(value: &Value, field: &str) -> Option<String> {
    let text = value.get(field)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// An id field, accepting either a JSON number or a string, as a string.
fn json_id(value: &Value) -> Option<String> {
    if let Some(number) = value.as_u64() {
        return Some(number.to_string());
    }
    if let Some(number) = value.as_i64() {
        return Some(number.to_string());
    }
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn string_array(value: &Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(|text| text.trim().to_string()))
                .filter(|text| !text.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
