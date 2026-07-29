//! Behavioural tests for candidate ranking, evidence, and eligibility.

use super::*;
use crate::emulator_environment::EncodedPath;
use crate::patch_manager::cheat_catalogue::{
    CHEAT_CATALOGUE_FORMAT_VERSION, CatalogueIndexState, CheatCatalogueFormat,
};

const CATALOGUE_ROOT: &str = "/catalogue";

#[derive(Default)]
struct RecordBuilder {
    name: String,
    relative_path: String,
    platform: Option<String>,
    region: Option<String>,
    revision: Option<String>,
    serial: Option<String>,
    content_hash: Option<String>,
    emulator: Option<String>,
    cheat_count: usize,
    parsing_complete: bool,
}

impl RecordBuilder {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            relative_path: format!("{name}.cht"),
            emulator: Some("retroarch".to_string()),
            cheat_count: 3,
            parsing_complete: true,
            ..Self::default()
        }
    }

    fn path(mut self, relative: &str) -> Self {
        self.relative_path = relative.to_string();
        self
    }

    fn platform(mut self, platform: &str) -> Self {
        self.platform = Some(platform.to_string());
        self
    }

    fn region(mut self, region: &str) -> Self {
        self.region = Some(region.to_string());
        self
    }

    fn revision(mut self, revision: &str) -> Self {
        self.revision = Some(revision.to_string());
        self
    }

    fn serial(mut self, serial: &str) -> Self {
        self.serial = Some(serial.to_string());
        self
    }

    fn content_hash(mut self, hash: &str) -> Self {
        self.content_hash = Some(hash.to_string());
        self
    }

    fn emulator(mut self, emulator: &str) -> Self {
        self.emulator = Some(emulator.to_string());
        self
    }

    fn malformed(mut self) -> Self {
        self.parsing_complete = false;
        self
    }

    fn build(self) -> CheatGameRecord {
        CheatGameRecord {
            source_game_name: self.name,
            source_platform: self.platform,
            source_region: self.region,
            source_revision: self.revision,
            source_identifier: self.serial,
            source_content_hash: self.content_hash,
            target_emulator: self.emulator,
            cheat_count: self.cheat_count,
            cheats: Vec::new(),
            enabled_by_default_count: 0,
            source_file_path: EncodedPath {
                display: format!("{CATALOGUE_ROOT}/{}", self.relative_path),
                lossy: false,
            },
            source_file_hash: Some("abc123".to_string()),
            format: CheatCatalogueFormat::RetroarchChtDirectory,
            parsing_complete: self.parsing_complete,
            parsing_diagnostics: Vec::new(),
        }
    }
}

fn snapshot(records: Vec<CheatGameRecord>) -> CheatCatalogueSnapshot {
    CheatCatalogueSnapshot {
        format_version: CHEAT_CATALOGUE_FORMAT_VERSION,
        source_name: "test".to_string(),
        source_root: EncodedPath {
            display: CATALOGUE_ROOT.to_string(),
            lossy: false,
        },
        read_only: true,
        complete: true,
        index_state: CatalogueIndexState::Complete,
        total_candidate_files: records.len(),
        games: records,
        excluded_entries: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn archive(name: &str, platform: Option<&str>) -> CheatCandidateArchive {
    CheatCandidateArchive {
        display_name: name.to_string(),
        platform: platform.map(str::to_string),
        content_basename: Some(name.to_string()),
        ..CheatCandidateArchive::default()
    }
}

fn build(records: Vec<CheatGameRecord>, archive: &CheatCandidateArchive) -> CheatCandidateList {
    build_cheat_candidates(
        &snapshot(records),
        archive,
        &CheatCandidateOptions::default(),
    )
}

fn evidence_kinds(candidate: &CheatCandidate) -> Vec<CheatCandidateEvidenceKind> {
    candidate
        .evidence
        .iter()
        .map(|evidence| evidence.kind)
        .collect()
}

// -----------------------------------------------------------------------
// Exact and strong matches
// -----------------------------------------------------------------------

#[test]
fn a_serial_match_is_verified_exact_and_auto_selectable() {
    let mut selected = archive("Some Other Name", Some("PlayStation"));
    selected.serial = Some("slus-12345".to_string());
    let list = build(
        vec![
            RecordBuilder::new("Real Title")
                .platform("PlayStation")
                .serial("SLUS-12345")
                .build(),
        ],
        &selected,
    );
    let candidate = list.automatic_choice().expect("auto-selected");
    assert_eq!(
        candidate.classification,
        CheatCandidateClassification::VerifiedExact
    );
    assert!(evidence_kinds(candidate).contains(&CheatCandidateEvidenceKind::ExactSerial));
    assert_eq!(candidate.confidence_score, 1000);
}

#[test]
fn a_content_hash_match_is_verified_exact() {
    let mut selected = archive("Unknown", None);
    selected.content_hash = Some("DEADBEEF".to_string());
    let list = build(
        vec![
            RecordBuilder::new("Real Title")
                .content_hash("deadbeef")
                .build(),
        ],
        &selected,
    );
    let candidate = list.automatic_choice().expect("auto-selected");
    assert!(evidence_kinds(candidate).contains(&CheatCandidateEvidenceKind::ExactContentHash));
}

#[test]
fn title_platform_and_region_agreement_is_verified_exact() {
    let mut selected = archive("Chrono Quest", Some("Sega - Mega Drive - Genesis"));
    selected.region = Some("USA".to_string());
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Sega - Mega Drive - Genesis")
                .region("usa")
                .build(),
        ],
        &selected,
    );
    let candidate = &list.candidates[0];
    assert_eq!(
        candidate.classification,
        CheatCandidateClassification::VerifiedExact
    );
    let kinds = evidence_kinds(candidate);
    assert!(kinds.contains(&CheatCandidateEvidenceKind::ExactNormalizedTitle));
    assert!(kinds.contains(&CheatCandidateEvidenceKind::PlatformMatch));
    assert!(kinds.contains(&CheatCandidateEvidenceKind::RegionMatch));
}

#[test]
fn title_and_platform_agreement_without_region_is_strong_not_auto_selected() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
        ],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    let candidate = &list.candidates[0];
    assert_eq!(
        candidate.classification,
        CheatCandidateClassification::Strong
    );
    assert!(
        candidate.manually_selectable,
        "a strong match is installable"
    );
    assert!(!candidate.auto_selectable, "but never chosen for the user");
    assert!(list.automatic_choice().is_none());
}

#[test]
fn a_title_only_match_with_no_platform_corroboration_is_weak() {
    let list = build(
        vec![RecordBuilder::new("Chrono Quest").build()],
        &archive("Chrono Quest", None),
    );
    let candidate = &list.candidates[0];
    assert_eq!(candidate.classification, CheatCandidateClassification::Weak);
    assert!(candidate.manually_selectable);
    assert!(!candidate.auto_selectable);
}

#[test]
fn a_trailing_article_title_matches_through_the_alternate_form() {
    let list = build(
        vec![
            RecordBuilder::new("Legend of Zelda, The")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
        ],
        &archive(
            "The Legend of Zelda",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    let candidate = &list.candidates[0];
    assert_eq!(
        candidate.classification,
        CheatCandidateClassification::Strong
    );
    assert!(evidence_kinds(candidate).contains(&CheatCandidateEvidenceKind::AlternateTitle));
}

#[test]
fn parenthesized_tags_never_defeat_a_title_match() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest (USA) (Rev 1)")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
        ],
        &archive(
            "Chrono Quest (Europe)",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    assert_eq!(list.candidates.len(), 1);
    assert!(
        evidence_kinds(&list.candidates[0])
            .contains(&CheatCandidateEvidenceKind::ExactNormalizedTitle)
    );
}

// -----------------------------------------------------------------------
// Ranking, ambiguity, and manual choice
// -----------------------------------------------------------------------

#[test]
fn a_stronger_candidate_outranks_a_weaker_one() {
    let mut selected = archive(
        "Chrono Quest",
        Some("Nintendo - Nintendo Entertainment System"),
    );
    selected.region = Some("USA".to_string());
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest").path("weak.cht").build(),
            RecordBuilder::new("Chrono Quest")
                .path("exact.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .region("USA")
                .build(),
        ],
        &selected,
    );
    assert_eq!(list.candidates[0].catalogue_relative_path, "exact.cht");
    assert!(list.candidates[0].confidence_score > list.candidates[1].confidence_score);
}

#[test]
fn equally_strong_candidates_are_all_ambiguous_and_none_is_auto_selected() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .path("b.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
            RecordBuilder::new("Chrono Quest")
                .path("a.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
        ],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    assert_eq!(list.candidates.len(), 2);
    assert!(
        list.candidates
            .iter()
            .all(|candidate| candidate.classification == CheatCandidateClassification::Ambiguous),
        "a tie is shown as a tie, never resolved silently"
    );
    assert!(list.automatic_choice().is_none());
    assert!(
        list.candidates
            .iter()
            .all(|candidate| candidate.manually_selectable),
        "the user can still choose either one explicitly"
    );
    assert_eq!(
        list.candidates
            .iter()
            .map(|candidate| candidate.catalogue_relative_path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.cht", "b.cht"],
        "ties order by catalogue path, so the list is deterministic"
    );
}

#[test]
fn an_ambiguous_tie_does_not_demote_a_clearly_better_candidate() {
    let mut selected = archive(
        "Chrono Quest",
        Some("Nintendo - Nintendo Entertainment System"),
    );
    selected.serial = Some("NES-CQ".to_string());
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .path("a.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
            RecordBuilder::new("Chrono Quest")
                .path("b.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
            RecordBuilder::new("Chrono Quest")
                .path("serial.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .serial("NES-CQ")
                .build(),
        ],
        &selected,
    );
    let best = list.automatic_choice().expect("the serial match wins");
    assert_eq!(best.catalogue_relative_path, "serial.cht");
    assert!(
        list.candidates[1..]
            .iter()
            .all(|candidate| !candidate.auto_selectable)
    );
}

#[test]
fn no_match_produces_an_empty_list_rather_than_an_error() {
    let list = build(
        vec![RecordBuilder::new("Completely Different Game").build()],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    assert!(list.is_empty());
    assert_eq!(list.total_matched, 0);
    assert!(list.automatic_choice().is_none());
}

// -----------------------------------------------------------------------
// Never-installable candidates
// -----------------------------------------------------------------------

#[test]
fn a_cross_platform_candidate_is_listed_but_never_installable() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Sega - Mega Drive - Genesis")
                .build(),
        ],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    let candidate = &list.candidates[0];
    assert_eq!(
        candidate.classification,
        CheatCandidateClassification::CrossPlatform
    );
    assert!(!candidate.manually_selectable);
    assert!(!candidate.auto_selectable);
    assert_eq!(candidate.confidence_score, 0);
    assert!(evidence_kinds(candidate).contains(&CheatCandidateEvidenceKind::PlatformMismatch));
}

#[test]
fn a_candidate_for_another_emulator_is_unsupported() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Nintendo - Nintendo Entertainment System")
                .emulator("pcsx2")
                .build(),
        ],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    let candidate = &list.candidates[0];
    assert_eq!(
        candidate.classification,
        CheatCandidateClassification::Unsupported
    );
    assert!(!candidate.manually_selectable);
    assert!(evidence_kinds(candidate).contains(&CheatCandidateEvidenceKind::UnsupportedEmulator));
}

#[test]
fn a_candidate_that_did_not_parse_cleanly_is_unsupported() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Nintendo - Nintendo Entertainment System")
                .malformed()
                .build(),
        ],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    assert_eq!(
        list.candidates[0].classification,
        CheatCandidateClassification::Unsupported
    );
    assert!(!list.candidates[0].manually_selectable);
}

#[test]
fn uninstallable_candidates_can_be_filtered_out_entirely() {
    let options = CheatCandidateOptions {
        include_uninstallable: false,
        ..CheatCandidateOptions::default()
    };
    let list = build_cheat_candidates(
        &snapshot(vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Sega - Mega Drive - Genesis")
                .build(),
        ]),
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
        &options,
    );
    assert!(list.is_empty());
}

// -----------------------------------------------------------------------
// Region and revision evidence
// -----------------------------------------------------------------------

#[test]
fn a_region_mismatch_is_shown_as_evidence_without_blocking_the_match() {
    let mut selected = archive(
        "Chrono Quest",
        Some("Nintendo - Nintendo Entertainment System"),
    );
    selected.region = Some("Europe".to_string());
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Nintendo - Nintendo Entertainment System")
                .region("USA")
                .build(),
        ],
        &selected,
    );
    let candidate = &list.candidates[0];
    assert_eq!(
        candidate.classification,
        CheatCandidateClassification::Strong,
        "a region difference downgrades from exact but stays installable"
    );
    assert!(evidence_kinds(candidate).contains(&CheatCandidateEvidenceKind::RegionMismatch));
}

#[test]
fn a_revision_mismatch_is_shown_as_evidence() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Nintendo - Nintendo Entertainment System")
                .revision("2")
                .build(),
        ],
        &archive(
            "Chrono Quest (Rev 1)",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    assert!(
        evidence_kinds(&list.candidates[0]).contains(&CheatCandidateEvidenceKind::RevisionMismatch)
    );
}

#[test]
fn no_evidence_is_produced_for_a_field_only_one_side_declares() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Nintendo - Nintendo Entertainment System")
                .region("USA")
                .build(),
        ],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    let kinds = evidence_kinds(&list.candidates[0]);
    assert!(
        !kinds.contains(&CheatCandidateEvidenceKind::RegionMatch)
            && !kinds.contains(&CheatCandidateEvidenceKind::RegionMismatch),
        "the archive declares no region, so no region claim is made: {kinds:?}"
    );
}

#[test]
fn filename_similarity_only_corroborates_an_existing_title_relation() {
    let mut selected = archive(
        "Chrono Quest",
        Some("Nintendo - Nintendo Entertainment System"),
    );
    selected.content_basename = Some("Chrono Quest (USA)".to_string());
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
            RecordBuilder::new("Quest for Glory")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
        ],
        &selected,
    );
    assert_eq!(
        list.candidates.len(),
        1,
        "sharing one word is not by itself a candidate"
    );
    assert!(
        evidence_kinds(&list.candidates[0])
            .contains(&CheatCandidateEvidenceKind::FilenameSimilarity)
    );
}

// -----------------------------------------------------------------------
// Bounding and search
// -----------------------------------------------------------------------

#[test]
fn the_candidate_list_is_capped_and_reports_the_full_total() {
    let records: Vec<CheatGameRecord> = (0..40)
        .map(|index| {
            RecordBuilder::new("Chrono Quest")
                .path(&format!("file{index:02}.cht"))
                .platform("Nintendo - Nintendo Entertainment System")
                .build()
        })
        .collect();
    let options = CheatCandidateOptions {
        limit: 5,
        ..CheatCandidateOptions::default()
    };
    let list = build_cheat_candidates(
        &snapshot(records),
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
        &options,
    );
    assert_eq!(list.candidates.len(), 5);
    assert_eq!(list.total_matched, 40);
    assert!(list.truncated);
}

#[test]
fn a_query_filters_the_candidate_list_before_the_cap() {
    let records = vec![
        RecordBuilder::new("Chrono Quest")
            .path("usa/Chrono Quest.cht")
            .platform("Nintendo - Nintendo Entertainment System")
            .build(),
        RecordBuilder::new("Chrono Quest")
            .path("europe/Chrono Quest.cht")
            .platform("Nintendo - Nintendo Entertainment System")
            .build(),
    ];
    let options = CheatCandidateOptions {
        query: Some("europe".to_string()),
        ..CheatCandidateOptions::default()
    };
    let list = build_cheat_candidates(
        &snapshot(records),
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
        &options,
    );
    assert_eq!(list.candidates.len(), 1);
    assert_eq!(
        list.candidates[0].catalogue_relative_path,
        "europe/Chrono Quest.cht"
    );
    assert_eq!(list.query.as_deref(), Some("europe"));
}

#[test]
fn the_catalogue_relative_path_is_relative_to_the_snapshot_root() {
    let list = build(
        vec![
            RecordBuilder::new("Chrono Quest")
                .path("Nintendo - Nintendo Entertainment System/Chrono Quest.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
        ],
        &archive(
            "Chrono Quest",
            Some("Nintendo - Nintendo Entertainment System"),
        ),
    );
    assert_eq!(
        list.candidates[0].catalogue_relative_path,
        "Nintendo - Nintendo Entertainment System/Chrono Quest.cht"
    );
}

#[test]
fn building_candidates_is_deterministic() {
    let records = || {
        vec![
            RecordBuilder::new("Chrono Quest")
                .path("b.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
            RecordBuilder::new("Chrono Quest")
                .path("a.cht")
                .platform("Nintendo - Nintendo Entertainment System")
                .build(),
        ]
    };
    let selected = archive(
        "Chrono Quest",
        Some("Nintendo - Nintendo Entertainment System"),
    );
    assert_eq!(build(records(), &selected), build(records(), &selected));
}
