//! Ranked, evidence-carrying cheat-file candidates for **one selected
//! archive**.
//!
//! ## Why this exists alongside `cheat_catalogue::match_cheat_game_record`
//!
//! That function answers the *indexing* question - "which library archive,
//! if any, does this one catalogue record belong to?" - and it deliberately
//! refuses to choose when several archives tie. The Cheats & Mods workflow
//! asks the mirror-image question: the archive is already fixed by the
//! user, and what is needed is *which catalogue files could be installed
//! for it*, ordered, with the reasoning shown.
//!
//! Running the indexing matcher once per record and keeping the hits does
//! not answer that: an archive-vs-record tie is not the same relation as a
//! record-vs-archive tie, and the indexing matcher discards the losing
//! tiers' evidence as soon as one tier produces a hit. This module
//! therefore evaluates every signal for every record and keeps all of it,
//! which is exactly what a user choosing between two candidates needs to
//! see.
//!
//! ## What is and is not installable
//!
//! - A **verified exact** candidate (serial, content hash, or normalized
//!   title + canonical platform + region) may be selected automatically,
//!   but only when it is the single best candidate.
//! - A **strong** candidate is shown as the recommended choice and is
//!   installable, but never auto-selected.
//! - **Ambiguous** candidates - two or more sharing the top score - are all
//!   shown and require an explicit choice.
//! - A **cross-platform** candidate (the record declares a platform, and it
//!   is not the archive's) and an **unsupported** candidate (a record aimed
//!   at another emulator, or one that did not parse) are listed for
//!   transparency and are never installable by any path.
//!
//! Evidence is only ever emitted for a comparison this module actually
//! performed against data both sides really declared. A field the archive
//! or the record does not carry produces no evidence at all, rather than a
//! "not checked" placeholder that would read like a finding.

use serde::Serialize;

use super::cheat_catalogue::{CheatCatalogueSnapshot, CheatGameRecord};
use crate::canonical_platform_for_alias;

/// The default bounded candidate-list size. A catalogue with more matching
/// files than this is not an error: the list is capped, `truncated` is set,
/// and [`CheatCandidateOptions::query`] narrows it.
pub const MAX_CHEAT_CANDIDATES: usize = 25;
/// Hard ceiling on records examined per call, so a pathological catalogue
/// cannot make candidate building unbounded work.
pub const MAX_CHEAT_CANDIDATE_RECORDS_SCANNED: usize = 100_000;
/// Evidence entries retained per candidate.
pub const MAX_CHEAT_CANDIDATE_EVIDENCE: usize = 12;

/// The identity of the archive the user selected, as far as it is actually
/// known. Every field is optional except the display name, because a
/// candidate list must still be produced for an archive whose serial, hash,
/// or region were never resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheatCandidateArchive {
    pub display_name: String,
    /// Canonical ArchiveFS platform name, or a recognizable alias.
    pub platform: Option<String>,
    pub region: Option<String>,
    /// Serial / product code (`SLUS-12345`), when identity resolved one.
    pub serial: Option<String>,
    /// CRC32 or SHA-256 of the content, when identity verified one.
    pub content_hash: Option<String>,
    /// The content file's basename without extension - the strongest
    /// filename-level identity, and the name RetroArch itself uses.
    pub content_basename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheatCandidateOptions {
    /// Bounded list size. Zero is treated as [`MAX_CHEAT_CANDIDATES`].
    pub limit: usize,
    /// Case-insensitive substring filter over the candidate display name
    /// and catalogue-relative path. Applied *before* the cap, so a
    /// truncated list stays searchable.
    pub query: Option<String>,
    /// When false, candidates that can never be installed (cross-platform,
    /// unsupported) are omitted entirely rather than listed as blocked.
    pub include_uninstallable: bool,
}

impl Default for CheatCandidateOptions {
    fn default() -> Self {
        Self {
            limit: MAX_CHEAT_CANDIDATES,
            query: None,
            include_uninstallable: true,
        }
    }
}

/// How strongly one catalogue file matches the selected archive, and
/// therefore what the user is allowed to do with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatCandidateClassification {
    /// Declares a different platform than the archive. Never installable.
    CrossPlatform,
    /// Aimed at another emulator, or did not parse cleanly. Never
    /// installable.
    Unsupported,
    /// Title agreement only, with no platform corroboration.
    Weak,
    /// Tied with at least one other candidate at the same strength.
    /// Installable only after an explicit choice.
    Ambiguous,
    /// Normalized title and canonical platform agree. Installable, never
    /// auto-selected.
    Strong,
    /// Serial, content hash, or title+platform+region agree. May be
    /// auto-selected when it is the single best candidate.
    VerifiedExact,
}

impl CheatCandidateClassification {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::CrossPlatform => "Different platform",
            Self::Unsupported => "Unsupported",
            Self::Weak => "Weak match",
            Self::Ambiguous => "Ambiguous",
            Self::Strong => "Strong match",
            Self::VerifiedExact => "Verified exact",
        }
    }

    /// Whether a candidate with this classification may ever be installed.
    #[must_use]
    pub fn is_installable(self) -> bool {
        matches!(
            self,
            Self::Weak | Self::Ambiguous | Self::Strong | Self::VerifiedExact
        )
    }
}

/// A stable identifier for one comparison that was actually performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatCandidateEvidenceKind {
    ExactSerial,
    ExactContentHash,
    ExactNormalizedTitle,
    AlternateTitle,
    FilenameSimilarity,
    PlatformMatch,
    PlatformMismatch,
    RegionMatch,
    RegionMismatch,
    RevisionMatch,
    RevisionMismatch,
    UnsupportedEmulator,
    ParsingIncomplete,
}

impl CheatCandidateEvidenceKind {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::ExactSerial => "exact_serial",
            Self::ExactContentHash => "exact_content_hash",
            Self::ExactNormalizedTitle => "exact_normalized_title",
            Self::AlternateTitle => "alternate_title",
            Self::FilenameSimilarity => "filename_similarity",
            Self::PlatformMatch => "platform_match",
            Self::PlatformMismatch => "platform_mismatch",
            Self::RegionMatch => "region_match",
            Self::RegionMismatch => "region_mismatch",
            Self::RevisionMatch => "revision_match",
            Self::RevisionMismatch => "revision_mismatch",
            Self::UnsupportedEmulator => "unsupported_emulator",
            Self::ParsingIncomplete => "parsing_incomplete",
        }
    }

    /// Whether this evidence argues *for* the match. A mismatch is still
    /// evidence, and is still shown - it just never raises the score.
    #[must_use]
    pub fn is_supporting(self) -> bool {
        matches!(
            self,
            Self::ExactSerial
                | Self::ExactContentHash
                | Self::ExactNormalizedTitle
                | Self::AlternateTitle
                | Self::FilenameSimilarity
                | Self::PlatformMatch
                | Self::RegionMatch
                | Self::RevisionMatch
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheatCandidateEvidence {
    pub kind: CheatCandidateEvidenceKind,
    /// The exact compared values, never prose about what might have been
    /// compared.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheatCandidate {
    /// Path relative to the catalogue root - the stable identity the
    /// install path re-resolves, never an absolute path the GUI retains.
    pub catalogue_relative_path: String,
    pub display_name: String,
    pub platform: Option<String>,
    pub region: Option<String>,
    pub revision: Option<String>,
    pub classification: CheatCandidateClassification,
    /// Ordered strength, 0-1000. Comparable only within one list.
    pub confidence_score: u32,
    pub evidence: Vec<CheatCandidateEvidence>,
    pub cheat_count: usize,
    pub source_file_hash: Option<String>,
    /// May be chosen without the user picking it.
    pub auto_selectable: bool,
    /// May be chosen by the user. False for anything not installable.
    pub manually_selectable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheatCandidateList {
    /// Best first. Ties are broken by catalogue-relative path so the order
    /// is deterministic across runs.
    pub candidates: Vec<CheatCandidate>,
    /// How many candidates matched before the cap was applied.
    pub total_matched: usize,
    pub truncated: bool,
    /// The query that produced this list, echoed back so a filtered list is
    /// never mistaken for the whole catalogue's answer.
    pub query: Option<String>,
    /// How many catalogue records were examined.
    pub records_scanned: usize,
    /// True when the record limit stopped the scan early.
    pub scan_limit_reached: bool,
}

impl CheatCandidateList {
    pub fn installable(&self) -> impl Iterator<Item = &CheatCandidate> {
        self.candidates
            .iter()
            .filter(|candidate| candidate.manually_selectable)
    }

    /// The candidate to preselect, or `None` when the user must choose.
    /// Only ever a verified-exact candidate that is the unique best.
    #[must_use]
    pub fn automatic_choice(&self) -> Option<&CheatCandidate> {
        let first = self.candidates.first()?;
        first.auto_selectable.then_some(first)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

// ---------------------------------------------------------------------
// Normalization helpers
// ---------------------------------------------------------------------

/// Lowercases, keeps alphanumerics, collapses everything else to single
/// spaces. Matches `cheat_catalogue`'s own normalization so the two modules
/// agree about what "the same title" means.
fn normalize(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut last_was_space = true;
    for character in text.chars() {
        if character.is_alphanumeric() {
            normalized.extend(character.to_lowercase());
            last_was_space = false;
        } else if !last_was_space {
            normalized.push(' ');
            last_was_space = true;
        }
    }
    normalized.truncate(normalized.trim_end().len());
    normalized
}

/// Strips every parenthesized or bracketed tag - the `(USA)`, `(Rev 1)`,
/// `[!]` segments No-Intro and libretro filenames carry - before
/// normalizing, so a tag difference alone never defeats a title match.
fn base_title(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut depth = 0usize;
    for character in text.chars() {
        match character {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => result.push(character),
            _ => {}
        }
    }
    normalize(&result)
}

/// Rewrites the trailing-article convention (`Legend of Zelda, The` ->
/// `the legend of zelda`) that catalogue and archive filenames disagree
/// about constantly. Returns `None` when the title has no trailing article,
/// so the caller can tell "an alternate form exists" from "it does not".
fn alternate_article_title(normalized: &str) -> Option<String> {
    for article in [
        "the", "a", "an", "los", "las", "les", "le", "la", "der", "die", "das",
    ] {
        if let Some(stem) = normalized.strip_suffix(&format!(" {article}")) {
            return Some(format!("{article} {stem}"));
        }
    }
    None
}

/// Token overlap as a percentage, 0-100. Symmetric, and 0 when either side
/// has no tokens - never a divide by zero.
fn filename_similarity(left: &str, right: &str) -> u32 {
    let left_tokens: Vec<&str> = left.split(' ').filter(|token| !token.is_empty()).collect();
    let right_tokens: Vec<&str> = right.split(' ').filter(|token| !token.is_empty()).collect();
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0;
    }
    let shared = left_tokens
        .iter()
        .filter(|token| right_tokens.contains(token))
        .count();
    let total = left_tokens.len().max(right_tokens.len());
    u32::try_from(shared * 100 / total).unwrap_or(0)
}

fn extract_revision(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let position = lower.find("rev")?;
    let token: String = text[position + 3..]
        .trim_start_matches(|character: char| character == '.' || character.is_whitespace())
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    (!token.is_empty()).then(|| token.to_ascii_uppercase())
}

fn normalize_identifier(text: &str) -> String {
    text.trim().to_ascii_uppercase()
}

// ---------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------

const SCORE_EXACT_SERIAL: u32 = 1000;
const SCORE_EXACT_CONTENT_HASH: u32 = 950;
const SCORE_TITLE_PLATFORM_REGION: u32 = 800;
const SCORE_TITLE_PLATFORM: u32 = 700;
const SCORE_ALTERNATE_TITLE_PLATFORM: u32 = 650;
const SCORE_TITLE_ONLY: u32 = 400;
const SCORE_ALTERNATE_TITLE_ONLY: u32 = 350;
/// Filename similarity contributes at most this much, and only on top of a
/// title relation that already exists - never as the sole reason to list a
/// candidate, which is how an unrelated game with one shared word would
/// otherwise appear.
const SCORE_FILENAME_SIMILARITY_MAX: u32 = 120;
/// The minimum token overlap that counts as filename evidence at all.
const FILENAME_SIMILARITY_FLOOR: u32 = 60;

/// Builds the ranked candidate list for one archive.
///
/// Pure: reads only the already-loaded snapshot and the supplied identity.
/// Touches no filesystem, opens no database, and never mutates the
/// catalogue.
#[must_use]
pub fn build_cheat_candidates(
    snapshot: &CheatCatalogueSnapshot,
    archive: &CheatCandidateArchive,
    options: &CheatCandidateOptions,
) -> CheatCandidateList {
    let limit = if options.limit == 0 {
        MAX_CHEAT_CANDIDATES
    } else {
        options.limit
    };
    let query = options
        .query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);

    let archive_title = base_title(&archive.display_name);
    let archive_alternate = alternate_article_title(&archive_title);
    let archive_basename = archive
        .content_basename
        .as_deref()
        .map(base_title)
        .filter(|value| !value.is_empty());
    let archive_platform = archive
        .platform
        .as_deref()
        .and_then(canonical_platform_for_alias);
    let archive_region = archive.region.as_deref().map(normalize);
    let archive_revision = extract_revision(&archive.display_name);

    let mut scored: Vec<CheatCandidate> = Vec::new();
    let mut records_scanned = 0usize;
    let mut scan_limit_reached = false;

    for record in &snapshot.games {
        if records_scanned >= MAX_CHEAT_CANDIDATE_RECORDS_SCANNED {
            scan_limit_reached = true;
            break;
        }
        records_scanned += 1;

        let Some(candidate) = evaluate_record(
            record,
            snapshot,
            archive,
            &archive_title,
            archive_alternate.as_deref(),
            archive_basename.as_deref(),
            archive_platform,
            archive_region.as_deref(),
            archive_revision.as_deref(),
        ) else {
            continue;
        };
        if !options.include_uninstallable && !candidate.classification.is_installable() {
            continue;
        }
        if let Some(needle) = query.as_deref()
            && !candidate.display_name.to_ascii_lowercase().contains(needle)
            && !candidate
                .catalogue_relative_path
                .to_ascii_lowercase()
                .contains(needle)
        {
            continue;
        }
        scored.push(candidate);
    }

    // Deterministic ordering: strongest first, then by catalogue path so two
    // equally-strong candidates always appear in the same order.
    scored.sort_by(|left, right| {
        right
            .confidence_score
            .cmp(&left.confidence_score)
            .then_with(|| {
                left.catalogue_relative_path
                    .cmp(&right.catalogue_relative_path)
            })
    });

    // A tie at the top is exactly the ambiguity the user has to resolve, so
    // every tied candidate is demoted to `Ambiguous` and nothing is
    // auto-selected. Classification is only downgraded here, never raised.
    if let Some(best_score) = scored
        .iter()
        .filter(|candidate| candidate.classification.is_installable())
        .map(|candidate| candidate.confidence_score)
        .max()
    {
        let tied = scored
            .iter()
            .filter(|candidate| {
                candidate.classification.is_installable()
                    && candidate.confidence_score == best_score
            })
            .count();
        if tied > 1 {
            for candidate in &mut scored {
                if candidate.classification.is_installable()
                    && candidate.confidence_score == best_score
                {
                    candidate.classification = CheatCandidateClassification::Ambiguous;
                    candidate.auto_selectable = false;
                }
            }
        }
    }

    let total_matched = scored.len();
    let truncated = total_matched > limit;
    scored.truncate(limit);

    CheatCandidateList {
        candidates: scored,
        total_matched,
        truncated,
        query: options.query.clone(),
        records_scanned,
        scan_limit_reached,
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_record(
    record: &CheatGameRecord,
    snapshot: &CheatCatalogueSnapshot,
    archive: &CheatCandidateArchive,
    archive_title: &str,
    archive_alternate: Option<&str>,
    archive_basename: Option<&str>,
    archive_platform: Option<&'static str>,
    archive_region: Option<&str>,
    archive_revision: Option<&str>,
) -> Option<CheatCandidate> {
    let record_title = base_title(&record.source_game_name);
    let record_alternate = alternate_article_title(&record_title);
    let record_platform = record
        .source_platform
        .as_deref()
        .and_then(canonical_platform_for_alias);
    let record_revision = record
        .source_revision
        .clone()
        .or_else(|| extract_revision(&record.source_game_name));

    let mut evidence: Vec<CheatCandidateEvidence> = Vec::new();
    let mut score = 0u32;
    let mut title_related = false;

    // --- Identity tiers. Both sides must actually declare the field.
    let serial_match = match (
        archive.serial.as_deref(),
        record.source_identifier.as_deref(),
    ) {
        (Some(left), Some(right)) if normalize_identifier(left) == normalize_identifier(right) => {
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::ExactSerial,
                detail: format!("serial {} matches exactly", normalize_identifier(left)),
            });
            score = score.max(SCORE_EXACT_SERIAL);
            true
        }
        _ => false,
    };
    let hash_match = match (
        archive.content_hash.as_deref(),
        record.source_content_hash.as_deref(),
    ) {
        (Some(left), Some(right)) if normalize_identifier(left) == normalize_identifier(right) => {
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::ExactContentHash,
                detail: format!(
                    "content hash {} matches exactly",
                    normalize_identifier(left)
                ),
            });
            score = score.max(SCORE_EXACT_CONTENT_HASH);
            true
        }
        _ => false,
    };

    // --- Title relation.
    let exact_title = !record_title.is_empty() && record_title == archive_title;
    let alternate_title = !exact_title
        && (record_alternate.as_deref() == Some(archive_title)
            || (archive_alternate.is_some() && archive_alternate == Some(record_title.as_str())));
    if exact_title {
        title_related = true;
        evidence.push(CheatCandidateEvidence {
            kind: CheatCandidateEvidenceKind::ExactNormalizedTitle,
            detail: format!("normalized title {record_title:?} matches exactly"),
        });
    } else if alternate_title {
        title_related = true;
        evidence.push(CheatCandidateEvidence {
            kind: CheatCandidateEvidenceKind::AlternateTitle,
            detail: format!(
                "titles match after article normalization ({:?} / {:?})",
                archive.display_name, record.source_game_name
            ),
        });
    }

    // --- Platform. A declared disagreement is a hard stop; agreement is
    // what promotes a title match from weak to strong.
    let mut cross_platform = false;
    match (archive_platform, record_platform) {
        (Some(left), Some(right)) if left == right => {
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::PlatformMatch,
                detail: format!("canonical platform {left} matches"),
            });
        }
        (Some(left), Some(right)) => {
            cross_platform = true;
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::PlatformMismatch,
                detail: format!("archive platform {left} but catalogue file declares {right}"),
            });
        }
        _ => {}
    }
    let platform_agrees = matches!(
        (archive_platform, record_platform),
        (Some(left), Some(right)) if left == right
    );

    // --- Region and revision, only when both sides declare them.
    let mut region_agrees = false;
    if let (Some(left), Some(right)) = (archive_region, record.source_region.as_deref()) {
        let right = normalize(right);
        if left == right {
            region_agrees = true;
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::RegionMatch,
                detail: format!("region {right} matches"),
            });
        } else {
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::RegionMismatch,
                detail: format!("archive region {left} but catalogue file declares {right}"),
            });
        }
    }
    if let (Some(left), Some(right)) = (archive_revision, record_revision.as_deref()) {
        if left == right {
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::RevisionMatch,
                detail: format!("revision {right} matches"),
            });
        } else {
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::RevisionMismatch,
                detail: format!("archive revision {left} but catalogue file declares {right}"),
            });
        }
    }

    // --- Filename similarity, only as corroboration of a title relation.
    if title_related && let Some(basename) = archive_basename {
        let similarity = filename_similarity(basename, &record_title);
        if similarity >= FILENAME_SIMILARITY_FLOOR {
            evidence.push(CheatCandidateEvidence {
                kind: CheatCandidateEvidenceKind::FilenameSimilarity,
                detail: format!("content filename shares {similarity}% of its title tokens"),
            });
            score += SCORE_FILENAME_SIMILARITY_MAX * similarity / 100;
        }
    }

    // --- Title-derived base score.
    if exact_title {
        score = score.max(if platform_agrees && region_agrees {
            SCORE_TITLE_PLATFORM_REGION
        } else if platform_agrees {
            SCORE_TITLE_PLATFORM
        } else {
            SCORE_TITLE_ONLY
        });
    } else if alternate_title {
        score = score.max(if platform_agrees {
            SCORE_ALTERNATE_TITLE_PLATFORM
        } else {
            SCORE_ALTERNATE_TITLE_ONLY
        });
    }

    // Nothing related this record to this archive at all - it is not a
    // candidate, and listing it would be noise, not transparency.
    if !title_related && !serial_match && !hash_match {
        return None;
    }

    // --- Classification.
    let emulator_supported = record
        .target_emulator
        .as_deref()
        .is_none_or(|value| value.eq_ignore_ascii_case("retroarch"));
    if !emulator_supported {
        evidence.push(CheatCandidateEvidence {
            kind: CheatCandidateEvidenceKind::UnsupportedEmulator,
            detail: format!(
                "catalogue file targets {}, not RetroArch",
                record
                    .target_emulator
                    .as_deref()
                    .unwrap_or("another emulator")
            ),
        });
    }
    if !record.parsing_complete {
        evidence.push(CheatCandidateEvidence {
            kind: CheatCandidateEvidenceKind::ParsingIncomplete,
            detail: "catalogue file did not parse cleanly during indexing".to_string(),
        });
    }

    let classification = if !emulator_supported || !record.parsing_complete {
        CheatCandidateClassification::Unsupported
    } else if cross_platform {
        CheatCandidateClassification::CrossPlatform
    } else if serial_match || hash_match || (exact_title && platform_agrees && region_agrees) {
        CheatCandidateClassification::VerifiedExact
    } else if platform_agrees && (exact_title || alternate_title) {
        CheatCandidateClassification::Strong
    } else {
        CheatCandidateClassification::Weak
    };

    // A candidate that can never be installed keeps its evidence (so the
    // user can see *why* it is listed and blocked) but scores zero, so it
    // can never displace a real match in the ordering.
    if !classification.is_installable() {
        score = 0;
    }
    evidence.truncate(MAX_CHEAT_CANDIDATE_EVIDENCE);

    let relative_path = catalogue_relative_path(record, snapshot);
    Some(CheatCandidate {
        catalogue_relative_path: relative_path,
        display_name: record.source_game_name.clone(),
        platform: record.source_platform.clone(),
        region: record.source_region.clone(),
        revision: record_revision,
        classification,
        confidence_score: score.min(1000),
        evidence,
        cheat_count: record.cheat_count,
        source_file_hash: record.source_file_hash.clone(),
        auto_selectable: classification == CheatCandidateClassification::VerifiedExact,
        manually_selectable: classification.is_installable(),
    })
}

/// The record's path relative to the catalogue root. Falls back to the full
/// encoded display path when the record sits outside the declared root,
/// rather than fabricating a relative path that would not resolve.
fn catalogue_relative_path(record: &CheatGameRecord, snapshot: &CheatCatalogueSnapshot) -> String {
    let full = &record.source_file_path.display;
    let root = &snapshot.source_root.display;
    full.strip_prefix(root.as_str())
        .map(|rest| rest.trim_start_matches(['/', '\\']).to_string())
        .filter(|rest| !rest.is_empty())
        .unwrap_or_else(|| full.clone())
}

#[cfg(test)]
mod tests;
