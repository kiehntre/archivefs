//! Read-only coverage accounting for the already-approved Dolphin and
//! RetroArch cheat providers.
//!
//! The builders in this module are pure: callers supply bounded, previously
//! loaded catalogue snapshots and verified identity evidence. No function in
//! this module opens a path, accesses the network, or writes provider data.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use super::dolphin_gecko_provider::GeckoApplicabilityDecision;
use super::{
    CheatCandidateArchive, CheatCandidateClassification, CheatCandidateEvidenceKind,
    CheatCandidateOptions, CheatCatalogueSnapshot, DolphinCatalogue, build_cheat_candidates,
    revision_applicability,
};

pub const CHEAT_PROVIDER_COVERAGE_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageProvider {
    DolphinUpstreamGameSettings,
    RetroarchLibretroDatabase,
}

impl CoverageProvider {
    pub const fn emulator(self) -> &'static str {
        match self {
            Self::DolphinUpstreamGameSettings => "Dolphin",
            Self::RetroarchLibretroDatabase => "RetroArch",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::DolphinUpstreamGameSettings => "Dolphin upstream GameSettings",
            Self::RetroarchLibretroDatabase => "libretro database cheats",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageRejectionCategory {
    IdentityUnavailable,
    CatalogueUnavailable,
    NoSourceRecord,
    RegionMismatch,
    RevisionMismatch,
    PlatformMismatch,
    CoreOrEmulatorMismatch,
    FilenameOnlyMatch,
    AmbiguousMatch,
    MalformedEntry,
    DuplicateEntry,
    ConflictingEntry,
    UnsupportedFormat,
    SafetyFiltered,
}

impl CoverageRejectionCategory {
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::IdentityUnavailable => "the required verified game identity is unavailable",
            Self::CatalogueUnavailable => "the local provider catalogue is unavailable",
            Self::NoSourceRecord => "the provider contains no matching source record",
            Self::RegionMismatch => "the provider record targets a different region",
            Self::RevisionMismatch => "the provider record targets a different revision",
            Self::PlatformMismatch => "the provider record targets a different platform",
            Self::CoreOrEmulatorMismatch => "the record targets another emulator or core context",
            Self::FilenameOnlyMatch => "only weak filename or title evidence matched",
            Self::AmbiguousMatch => "multiple candidates tied and none was selected silently",
            Self::MalformedEntry => "the provider entry did not parse completely",
            Self::DuplicateEntry => "the provider contains a duplicate entry",
            Self::ConflictingEntry => "same-name provider entries have different content",
            Self::UnsupportedFormat => "the provider entry uses an unsupported format",
            Self::SafetyFiltered => "the existing provider safety policy rejected the entry",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageRejection {
    pub category: CoverageRejectionCategory,
    pub count: usize,
    pub explanation: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageGameIdentity {
    pub archive_id: i64,
    pub title: String,
    pub platform: String,
    pub identity_kind: Option<String>,
    pub verified_identity: Option<String>,
    pub region: Option<String>,
    pub revision: Option<String>,
    pub serial: Option<String>,
    pub content_hash: Option<String>,
    pub content_basename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheatCoverageEntry {
    pub emulator: &'static str,
    pub provider: CoverageProvider,
    pub provider_name: &'static str,
    pub platform: String,
    pub game_title: String,
    pub identity_kind: Option<String>,
    pub verified_identity: Option<String>,
    pub region: Option<String>,
    pub revision: Option<String>,
    pub compatible_cheat_count: usize,
    pub rejected_candidate_count: usize,
    pub rejection_reasons: Vec<CoverageRejection>,
    pub duplicate_count: usize,
    pub conflicting_entry_count: usize,
    pub unsupported_format_count: usize,
    pub no_match_reason: Option<CoverageRejectionCategory>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CheatCoverageSummary {
    pub games_inspected: usize,
    pub games_with_compatible_cheats: usize,
    pub games_without_compatible_cheats: usize,
    pub compatible_cheats: usize,
    pub rejected_candidates: usize,
    pub duplicates: usize,
    pub conflicts: usize,
    pub unsupported_formats: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCoverageProvenance {
    pub provider: CoverageProvider,
    pub source: &'static str,
    pub licence: &'static str,
    pub local_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheatProviderCoverageReport {
    pub format_version: u32,
    pub read_only: bool,
    pub bounded_selection: bool,
    pub provenance: Vec<ProviderCoverageProvenance>,
    pub games: Vec<CheatCoverageEntry>,
    pub summary: CheatCoverageSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DolphinCodeFormat {
    Gecko,
    ActionReplay,
    Unsupported,
}

#[must_use]
pub fn classify_dolphin_code_section(section: &str) -> DolphinCodeFormat {
    if section.eq_ignore_ascii_case("Gecko") || section.eq_ignore_ascii_case("Gecko_Enabled") {
        DolphinCodeFormat::Gecko
    } else if section.eq_ignore_ascii_case("ActionReplay")
        || section.eq_ignore_ascii_case("ActionReplay_Enabled")
    {
        DolphinCodeFormat::ActionReplay
    } else {
        DolphinCodeFormat::Unsupported
    }
}

#[must_use]
pub fn build_cheat_provider_coverage_report(
    dolphin_games: &[CoverageGameIdentity],
    dolphin_catalogue: Option<&DolphinCatalogue>,
    retroarch_games: &[CoverageGameIdentity],
    retroarch_catalogue: Option<&CheatCatalogueSnapshot>,
) -> CheatProviderCoverageReport {
    let mut games = dolphin_games
        .iter()
        .map(|game| dolphin_entry(game, dolphin_catalogue))
        .chain(
            retroarch_games
                .iter()
                .map(|game| retroarch_entry(game, retroarch_catalogue)),
        )
        .collect::<Vec<_>>();
    games.sort_by(|left, right| {
        left.emulator
            .cmp(right.emulator)
            .then_with(|| left.platform.cmp(&right.platform))
            .then_with(|| left.game_title.cmp(&right.game_title))
    });

    let summary = CheatCoverageSummary {
        games_inspected: games.len(),
        games_with_compatible_cheats: games
            .iter()
            .filter(|game| game.compatible_cheat_count > 0)
            .count(),
        games_without_compatible_cheats: games
            .iter()
            .filter(|game| game.compatible_cheat_count == 0)
            .count(),
        compatible_cheats: games.iter().map(|game| game.compatible_cheat_count).sum(),
        rejected_candidates: games.iter().map(|game| game.rejected_candidate_count).sum(),
        duplicates: games.iter().map(|game| game.duplicate_count).sum(),
        conflicts: games.iter().map(|game| game.conflicting_entry_count).sum(),
        unsupported_formats: games.iter().map(|game| game.unsupported_format_count).sum(),
    };

    let dolphin_revision =
        dolphin_catalogue.map(|catalogue| catalogue.metadata.resolved_commit.clone());
    CheatProviderCoverageReport {
        format_version: CHEAT_PROVIDER_COVERAGE_FORMAT_VERSION,
        read_only: true,
        bounded_selection: true,
        provenance: vec![
            ProviderCoverageProvenance {
                provider: CoverageProvider::DolphinUpstreamGameSettings,
                source: "dolphin-emu/dolphin Data/Sys/GameSettings",
                licence: "GPL-2.0-or-later",
                local_revision: dolphin_revision,
            },
            ProviderCoverageProvenance {
                provider: CoverageProvider::RetroarchLibretroDatabase,
                source: "libretro/libretro-database cht",
                licence: "provider snapshot manifest",
                local_revision: None,
            },
        ],
        games,
        summary,
    }
}

fn base_entry(game: &CoverageGameIdentity, provider: CoverageProvider) -> CheatCoverageEntry {
    CheatCoverageEntry {
        emulator: provider.emulator(),
        provider,
        provider_name: provider.display_name(),
        platform: game.platform.clone(),
        game_title: game.title.clone(),
        identity_kind: game.identity_kind.clone(),
        verified_identity: game.verified_identity.clone(),
        region: game.region.clone(),
        revision: game.revision.clone(),
        compatible_cheat_count: 0,
        rejected_candidate_count: 0,
        rejection_reasons: Vec::new(),
        duplicate_count: 0,
        conflicting_entry_count: 0,
        unsupported_format_count: 0,
        no_match_reason: None,
    }
}

fn dolphin_entry(
    game: &CoverageGameIdentity,
    catalogue: Option<&DolphinCatalogue>,
) -> CheatCoverageEntry {
    let mut entry = base_entry(game, CoverageProvider::DolphinUpstreamGameSettings);
    let Some(game_id) = game.verified_identity.as_deref() else {
        reject(
            &mut entry,
            CoverageRejectionCategory::IdentityUnavailable,
            1,
        );
        entry.no_match_reason = Some(CoverageRejectionCategory::IdentityUnavailable);
        return entry;
    };
    let Some(catalogue) = catalogue else {
        reject(
            &mut entry,
            CoverageRejectionCategory::CatalogueUnavailable,
            1,
        );
        entry.no_match_reason = Some(CoverageRejectionCategory::CatalogueUnavailable);
        return entry;
    };
    let Some(provider_game) = catalogue.find(game_id) else {
        reject(&mut entry, CoverageRejectionCategory::NoSourceRecord, 1);
        entry.no_match_reason = Some(CoverageRejectionCategory::NoSourceRecord);
        return entry;
    };
    if game.region.as_deref().is_some_and(|region| {
        !provider_game
            .region
            .display_name()
            .eq_ignore_ascii_case(region)
    }) {
        reject(
            &mut entry,
            CoverageRejectionCategory::RegionMismatch,
            provider_game.codes.len().max(1),
        );
        entry.rejected_candidate_count = provider_game.codes.len();
        entry.no_match_reason = Some(CoverageRejectionCategory::RegionMismatch);
        return entry;
    }

    let revision = game
        .revision
        .as_deref()
        .and_then(|value| value.parse::<u16>().ok());
    let mut names: BTreeMap<&str, Vec<&[String]>> = BTreeMap::new();
    for code in &provider_game.codes {
        names
            .entry(code.name.as_str())
            .or_default()
            .push(&code.code_lines);
    }
    let duplicate_names = names
        .iter()
        .filter_map(|(name, bodies)| (bodies.len() > 1).then_some(*name))
        .collect::<BTreeSet<_>>();
    for code in &provider_game.codes {
        if duplicate_names.contains(code.name.as_str()) {
            entry.rejected_candidate_count += 1;
            reject(&mut entry, CoverageRejectionCategory::DuplicateEntry, 1);
            continue;
        }
        let revision_decision =
            revision.map(|value| revision_applicability(code.revision_applicability, value));
        if revision_decision == Some(GeckoApplicabilityDecision::Reject) {
            entry.rejected_candidate_count += 1;
            reject(&mut entry, CoverageRejectionCategory::RevisionMismatch, 1);
        } else if !code.safe_to_offer {
            entry.rejected_candidate_count += 1;
            entry.unsupported_format_count += 1;
            reject(&mut entry, CoverageRejectionCategory::MalformedEntry, 1);
            reject(&mut entry, CoverageRejectionCategory::SafetyFiltered, 1);
        } else {
            entry.compatible_cheat_count += 1;
        }
    }
    for bodies in names.values().filter(|bodies| bodies.len() > 1) {
        let extras = bodies.len() - 1;
        entry.duplicate_count += extras;
        let distinct = bodies.iter().copied().collect::<BTreeSet<_>>().len();
        if distinct > 1 {
            entry.conflicting_entry_count += bodies.len();
            reject(
                &mut entry,
                CoverageRejectionCategory::ConflictingEntry,
                bodies.len(),
            );
        }
    }
    if entry.compatible_cheat_count == 0 {
        if entry.rejection_reasons.is_empty() {
            reject(&mut entry, CoverageRejectionCategory::SafetyFiltered, 1);
        }
        entry.no_match_reason = entry
            .rejection_reasons
            .first()
            .map(|reason| reason.category);
    }
    entry
}

fn retroarch_entry(
    game: &CoverageGameIdentity,
    snapshot: Option<&CheatCatalogueSnapshot>,
) -> CheatCoverageEntry {
    let mut entry = base_entry(game, CoverageProvider::RetroarchLibretroDatabase);
    let Some(snapshot) = snapshot else {
        reject(
            &mut entry,
            CoverageRejectionCategory::CatalogueUnavailable,
            1,
        );
        entry.no_match_reason = Some(CoverageRejectionCategory::CatalogueUnavailable);
        return entry;
    };
    let archive = CheatCandidateArchive {
        display_name: game.title.clone(),
        platform: Some(game.platform.clone()),
        region: game.region.clone(),
        serial: game.serial.clone(),
        content_hash: game.content_hash.clone(),
        content_basename: game.content_basename.clone(),
    };
    let candidates = build_cheat_candidates(
        snapshot,
        &archive,
        &CheatCandidateOptions {
            limit: snapshot.games.len().max(1),
            query: None,
            include_uninstallable: true,
        },
    );
    let mut hashes = BTreeMap::<String, usize>::new();
    let mut installable_records = 0usize;
    for candidate in &candidates.candidates {
        if let Some(hash) = &candidate.source_file_hash {
            *hashes.entry(hash.clone()).or_default() += 1;
        }
        let region_mismatch = candidate
            .evidence
            .iter()
            .any(|evidence| evidence.kind == CheatCandidateEvidenceKind::RegionMismatch);
        let revision_mismatch = candidate
            .evidence
            .iter()
            .any(|evidence| evidence.kind == CheatCandidateEvidenceKind::RevisionMismatch);
        if region_mismatch || revision_mismatch {
            entry.rejected_candidate_count += candidate.cheat_count.max(1);
            if region_mismatch {
                reject(&mut entry, CoverageRejectionCategory::RegionMismatch, 1);
            }
            if revision_mismatch {
                reject(&mut entry, CoverageRejectionCategory::RevisionMismatch, 1);
            }
            continue;
        }
        match candidate.classification {
            CheatCandidateClassification::VerifiedExact | CheatCandidateClassification::Strong => {
                installable_records += 1;
                entry.compatible_cheat_count += candidate.cheat_count;
            }
            CheatCandidateClassification::Ambiguous => {
                entry.rejected_candidate_count += candidate.cheat_count.max(1);
                reject(&mut entry, CoverageRejectionCategory::AmbiguousMatch, 1);
            }
            CheatCandidateClassification::Weak => {
                entry.rejected_candidate_count += candidate.cheat_count.max(1);
                reject(&mut entry, CoverageRejectionCategory::FilenameOnlyMatch, 1);
            }
            CheatCandidateClassification::CrossPlatform => {
                entry.rejected_candidate_count += candidate.cheat_count.max(1);
                reject(&mut entry, CoverageRejectionCategory::PlatformMismatch, 1);
            }
            CheatCandidateClassification::Unsupported => {
                entry.rejected_candidate_count += candidate.cheat_count.max(1);
                entry.unsupported_format_count += 1;
                let emulator_mismatch = candidate.evidence.iter().any(|evidence| {
                    evidence.kind == CheatCandidateEvidenceKind::UnsupportedEmulator
                });
                reject(
                    &mut entry,
                    if emulator_mismatch {
                        CoverageRejectionCategory::CoreOrEmulatorMismatch
                    } else {
                        CoverageRejectionCategory::MalformedEntry
                    },
                    1,
                );
            }
        }
    }
    entry.duplicate_count = hashes.values().map(|count| count.saturating_sub(1)).sum();
    if entry.duplicate_count > 0 {
        let duplicate_count = entry.duplicate_count;
        reject(
            &mut entry,
            CoverageRejectionCategory::DuplicateEntry,
            duplicate_count,
        );
    }
    if installable_records > 1 {
        entry.conflicting_entry_count = installable_records;
        reject(
            &mut entry,
            CoverageRejectionCategory::ConflictingEntry,
            installable_records,
        );
    }
    if candidates.scan_limit_reached || candidates.truncated {
        reject(&mut entry, CoverageRejectionCategory::SafetyFiltered, 1);
    }
    if entry.compatible_cheat_count == 0 {
        let reason = entry
            .rejection_reasons
            .first()
            .map_or(CoverageRejectionCategory::NoSourceRecord, |reason| {
                reason.category
            });
        if candidates.total_matched == 0 {
            reject(&mut entry, CoverageRejectionCategory::NoSourceRecord, 1);
        }
        entry.no_match_reason = Some(reason);
    }
    entry
}

fn reject(entry: &mut CheatCoverageEntry, category: CoverageRejectionCategory, count: usize) {
    if let Some(reason) = entry
        .rejection_reasons
        .iter_mut()
        .find(|reason| reason.category == category)
    {
        reason.count += count;
    } else {
        entry.rejection_reasons.push(CoverageRejection {
            category,
            count,
            explanation: category.explanation(),
        });
        entry
            .rejection_reasons
            .sort_by_key(|reason| reason.category);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator_environment::EncodedPath;
    use crate::patch_manager::{
        CatalogueIndexState, CheatCatalogueFormat, CheatDefinition, CheatGameRecord,
        DolphinCatalogueCode, DolphinCatalogueGame, DolphinCatalogueMetadata, GeckoRegion,
        GeckoRevisionApplicability,
    };

    fn game(title: &str, platform: &str) -> CoverageGameIdentity {
        CoverageGameIdentity {
            archive_id: 1,
            title: title.to_string(),
            platform: platform.to_string(),
            identity_kind: None,
            verified_identity: None,
            region: None,
            revision: None,
            serial: None,
            content_hash: None,
            content_basename: Some(title.to_string()),
        }
    }

    fn dolphin_catalogue(codes: Vec<DolphinCatalogueCode>) -> DolphinCatalogue {
        DolphinCatalogue {
            metadata: DolphinCatalogueMetadata {
                schema_version: 1,
                repository: "dolphin-emu/dolphin".into(),
                canonical_repository_url: "https://example.invalid/dolphin".into(),
                resolved_commit: "abc123".into(),
                source_archive_url: "https://example.invalid/archive".into(),
                license: "GPL-2.0-or-later".into(),
                license_url: "https://example.invalid/license".into(),
                attribution: "fixture".into(),
                fetched_at_unix_seconds: 1,
                archive_sha256: "00".repeat(32),
                downloaded_bytes: 1,
                archive_entry_count: 1,
                game_settings_files_inspected: 1,
                games_with_usable_gecko: 1,
                total_usable_gecko_entries: codes.len(),
                malformed_or_skipped_files: 0,
                non_matching_files_skipped: 0,
                warnings: vec![],
            },
            games: vec![DolphinCatalogueGame {
                game_id: "GALE01".into(),
                title: Some("Test Game".into()),
                region: GeckoRegion::Usa,
                source_relative_path: "Data/Sys/GameSettings/GALE01.ini".into(),
                codes,
                file_warnings: vec![],
            }],
        }
    }

    fn gecko(name: &str, body: &str) -> DolphinCatalogueCode {
        DolphinCatalogueCode {
            name: name.into(),
            code_lines: vec![body.into()],
            notes: vec![],
            enabled_by_default: false,
            revision_applicability: GeckoRevisionApplicability::Uncertain,
            parse_warnings: vec![],
            safe_to_offer: true,
        }
    }

    fn retro_snapshot(records: Vec<CheatGameRecord>) -> CheatCatalogueSnapshot {
        CheatCatalogueSnapshot {
            format_version: 1,
            source_name: "fixture".into(),
            source_root: EncodedPath::from_path(std::path::Path::new("/redacted")),
            read_only: true,
            complete: true,
            index_state: CatalogueIndexState::Complete,
            total_candidate_files: records.len(),
            games: records,
            excluded_entries: vec![],
            diagnostics: vec![],
        }
    }

    fn retro_record(title: &str, platform: &str, hash: &str) -> CheatGameRecord {
        CheatGameRecord {
            source_game_name: title.into(),
            source_platform: Some(platform.into()),
            source_region: None,
            source_revision: None,
            source_identifier: None,
            source_content_hash: None,
            target_emulator: Some("RetroArch".into()),
            cheat_count: 2,
            cheats: vec![CheatDefinition {
                description: Some("Lives".into()),
                enabled_by_default: false,
                declared_index: Some(0),
            }],
            enabled_by_default_count: 0,
            source_file_path: EncodedPath::from_path(std::path::Path::new("/redacted/game.cht")),
            source_file_hash: Some(hash.into()),
            format: CheatCatalogueFormat::RetroarchChtDirectory,
            parsing_complete: true,
            parsing_diagnostics: vec![],
        }
    }

    #[test]
    fn dolphin_requires_exact_game_id() {
        let mut selected = game("Test Game", "GameCube");
        selected.verified_identity = Some("GALE01".into());
        selected.identity_kind = Some("dolphin_game_id".into());
        selected.region = Some("USA".into());
        let report = build_cheat_provider_coverage_report(
            &[selected],
            Some(&dolphin_catalogue(vec![gecko(
                "Lives",
                "01234567 89ABCDEF",
            )])),
            &[],
            None,
        );
        assert_eq!(report.games[0].compatible_cheat_count, 1);
        assert_eq!(report.games[0].no_match_reason, None);
    }

    #[test]
    fn dolphin_region_mismatch_is_rejected() {
        let mut selected = game("Test Game", "GameCube");
        selected.verified_identity = Some("GALE01".into());
        selected.region = Some("Europe".into());
        let report = build_cheat_provider_coverage_report(
            &[selected],
            Some(&dolphin_catalogue(vec![gecko(
                "Lives",
                "01234567 89ABCDEF",
            )])),
            &[],
            None,
        );
        assert_eq!(
            report.games[0].no_match_reason,
            Some(CoverageRejectionCategory::RegionMismatch)
        );
    }

    #[test]
    fn dolphin_revision_mismatch_is_rejected() {
        let mut code = gecko("Lives", "01234567 89ABCDEF");
        code.revision_applicability = GeckoRevisionApplicability::Exact(2);
        let mut selected = game("Test Game", "GameCube");
        selected.verified_identity = Some("GALE01".into());
        selected.region = Some("USA".into());
        selected.revision = Some("1".into());
        let report = build_cheat_provider_coverage_report(
            &[selected],
            Some(&dolphin_catalogue(vec![code])),
            &[],
            None,
        );
        assert_eq!(report.games[0].compatible_cheat_count, 0);
        assert_eq!(
            report.games[0].no_match_reason,
            Some(CoverageRejectionCategory::RevisionMismatch)
        );
    }

    #[test]
    fn gecko_and_action_replay_are_distinguished() {
        assert_eq!(
            classify_dolphin_code_section("Gecko"),
            DolphinCodeFormat::Gecko
        );
        assert_eq!(
            classify_dolphin_code_section("ActionReplay"),
            DolphinCodeFormat::ActionReplay
        );
        assert_eq!(
            classify_dolphin_code_section("OnFrame"),
            DolphinCodeFormat::Unsupported
        );
    }

    #[test]
    fn duplicate_and_conflicting_dolphin_codes_are_counted() {
        let mut selected = game("Test Game", "GameCube");
        selected.verified_identity = Some("GALE01".into());
        selected.region = Some("USA".into());
        let report = build_cheat_provider_coverage_report(
            &[selected],
            Some(&dolphin_catalogue(vec![
                gecko("Lives", "01234567 89ABCDEF"),
                gecko("Lives", "11111111 22222222"),
            ])),
            &[],
            None,
        );
        assert_eq!(report.games[0].duplicate_count, 1);
        assert_eq!(report.games[0].conflicting_entry_count, 2);
        assert_eq!(report.games[0].compatible_cheat_count, 0);
        assert_eq!(report.games[0].rejected_candidate_count, 2);
    }

    #[test]
    fn retroarch_exact_title_and_platform_match_counts_cheats() {
        let selected = game("Sonic", "MegaDrive");
        let snapshot = retro_snapshot(vec![retro_record("Sonic", "MegaDrive", "aa")]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(report.games[0].compatible_cheat_count, 2);
    }

    #[test]
    fn retroarch_exact_content_identity_beats_an_unrelated_title() {
        let mut selected = game("Local Name", "MegaDrive");
        selected.content_hash = Some("AABB".into());
        let mut record = retro_record("Provider Name", "MegaDrive", "aa");
        record.source_content_hash = Some("aabb".into());
        let snapshot = retro_snapshot(vec![record]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(report.games[0].compatible_cheat_count, 2);
    }

    #[test]
    fn retroarch_platform_mismatch_is_rejected() {
        let selected = game("Sonic", "SNES");
        let snapshot = retro_snapshot(vec![retro_record("Sonic", "MegaDrive", "aa")]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert!(
            report.games[0]
                .rejection_reasons
                .iter()
                .any(|reason| reason.category == CoverageRejectionCategory::PlatformMismatch)
        );
    }

    #[test]
    fn retroarch_region_mismatch_never_counts_as_coverage() {
        let mut selected = game("Sonic", "MegaDrive");
        selected.region = Some("USA".into());
        let mut record = retro_record("Sonic", "MegaDrive", "aa");
        record.source_region = Some("Europe".into());
        let snapshot = retro_snapshot(vec![record]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(report.games[0].compatible_cheat_count, 0);
        assert!(
            report.games[0]
                .rejection_reasons
                .iter()
                .any(|reason| { reason.category == CoverageRejectionCategory::RegionMismatch })
        );
    }

    #[test]
    fn retroarch_revision_mismatch_never_counts_as_coverage() {
        let selected = game("Sonic (Rev 1)", "MegaDrive");
        let mut record = retro_record("Sonic", "MegaDrive", "aa");
        record.source_revision = Some("2".into());
        let snapshot = retro_snapshot(vec![record]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(report.games[0].compatible_cheat_count, 0);
        assert!(
            report.games[0]
                .rejection_reasons
                .iter()
                .any(|reason| { reason.category == CoverageRejectionCategory::RevisionMismatch })
        );
    }

    #[test]
    fn malformed_retroarch_record_is_unsupported() {
        let selected = game("Sonic", "MegaDrive");
        let mut record = retro_record("Sonic", "MegaDrive", "aa");
        record.parsing_complete = false;
        let snapshot = retro_snapshot(vec![record]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(report.games[0].unsupported_format_count, 1);
        assert_eq!(report.games[0].compatible_cheat_count, 0);
    }

    #[test]
    fn other_emulator_format_is_rejected_without_weakening_the_match() {
        let selected = game("Sonic", "MegaDrive");
        let mut record = retro_record("Sonic", "MegaDrive", "aa");
        record.target_emulator = Some("AnotherEmulator".into());
        let snapshot = retro_snapshot(vec![record]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(report.games[0].compatible_cheat_count, 0);
        assert!(report.games[0].rejection_reasons.iter().any(|reason| {
            reason.category == CoverageRejectionCategory::CoreOrEmulatorMismatch
        }));
    }

    #[test]
    fn unavailable_catalogue_is_an_honest_zero_match() {
        let selected = game("Sonic", "MegaDrive");
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], None);
        assert_eq!(report.games[0].compatible_cheat_count, 0);
        assert_eq!(
            report.games[0].no_match_reason,
            Some(CoverageRejectionCategory::CatalogueUnavailable)
        );
    }

    #[test]
    fn duplicate_retroarch_files_are_visible_and_ambiguous() {
        let selected = game("Sonic", "MegaDrive");
        let snapshot = retro_snapshot(vec![
            retro_record("Sonic", "MegaDrive", "aa"),
            retro_record("Sonic", "MegaDrive", "aa"),
        ]);
        let report = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(report.games[0].duplicate_count, 1);
        assert_eq!(report.games[0].compatible_cheat_count, 0);
        assert!(
            report.games[0]
                .rejection_reasons
                .iter()
                .any(|reason| reason.category == CoverageRejectionCategory::AmbiguousMatch)
        );
    }

    #[test]
    fn report_counts_and_json_shape_are_stable() {
        let selected = game("Missing", "SNES");
        let report = build_cheat_provider_coverage_report(
            &[],
            None,
            &[selected],
            Some(&retro_snapshot(vec![])),
        );
        assert_eq!(report.summary.games_inspected, 1);
        assert_eq!(report.summary.games_without_compatible_cheats, 1);
        let json = serde_json::to_value(&report).unwrap();
        let keys = json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from([
                "bounded_selection".into(),
                "format_version".into(),
                "games".into(),
                "provenance".into(),
                "read_only".into(),
                "summary".into()
            ])
        );
        assert_eq!(json["read_only"], true);
    }

    #[test]
    fn audit_builders_do_not_modify_inputs() {
        let selected = game("Sonic", "MegaDrive");
        let snapshot = retro_snapshot(vec![retro_record("Sonic", "MegaDrive", "aa")]);
        let before = serde_json::to_string(&snapshot).unwrap();
        let _ = build_cheat_provider_coverage_report(&[], None, &[selected], Some(&snapshot));
        assert_eq!(serde_json::to_string(&snapshot).unwrap(), before);
    }
}
