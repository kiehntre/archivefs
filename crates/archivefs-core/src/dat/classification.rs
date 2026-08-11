//! Conservative, provider-aware DAT content classification.
//!
//! Classification annotates parsed entries; it never removes or rewrites one.
//! Identity indexing and audit therefore continue to see the complete upstream
//! catalogue. Consumers may apply [`ContentSelectionPolicy`] only after
//! matching has finished.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::model::{DatEcosystem, DatGameEntry, ParsedDat};

/// Bump whenever a rule changes meaning. Plans record this value so a later
/// classifier cannot silently reinterpret an already-reviewed action.
pub const CLASSIFIER_VERSION: &str = "dat-content-p0-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatContentClass {
    Game,
    GameCompilation,
    RequiredMultidiscPart,
    NonGame,
    Unknown,
}

impl DatContentClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Game => "Game",
            Self::GameCompilation => "Game compilation",
            Self::RequiredMultidiscPart => "Required multidisc part",
            Self::NonGame => "Non-game",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierConfidence {
    High,
    Medium,
    None,
}

impl ClassifierConfidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Medium => "Medium",
            Self::None => "No confident classification",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationEvidenceKind {
    StructuredEntryMetadata,
    CanonicalTosecSourceCategory,
    CanonicalTosecMediaToken,
    NoTrustworthyMetadata,
}

impl ClassificationEvidenceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::StructuredEntryMetadata => "Structured entry metadata",
            Self::CanonicalTosecSourceCategory => "Canonical TOSEC source category",
            Self::CanonicalTosecMediaToken => "Canonical TOSEC media token",
            Self::NoTrustworthyMetadata => "No trustworthy category metadata",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassificationEvidence {
    pub kind: ClassificationEvidenceKind,
    pub field: Option<String>,
    /// Exact upstream text used by the rule.
    pub original_value: Option<String>,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatContentClassification {
    pub class: DatContentClass,
    pub confidence: ClassifierConfidence,
    pub evidence: Vec<ClassificationEvidence>,
    pub classifier_version: String,
}

impl DatContentClassification {
    pub fn unknown() -> Self {
        Self {
            class: DatContentClass::Unknown,
            confidence: ClassifierConfidence::None,
            evidence: vec![ClassificationEvidence {
                kind: ClassificationEvidenceKind::NoTrustworthyMetadata,
                field: None,
                original_value: None,
                rule: "fallback.unknown".to_string(),
            }],
            classifier_version: CLASSIFIER_VERSION.to_string(),
        }
    }
}

impl Default for DatContentClassification {
    fn default() -> Self {
        Self::unknown()
    }
}

/// Original structured values retained exactly as parsed. These are separate
/// from the derived classification and remain available for technical review.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatOriginalMetadata {
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSelectionPolicy {
    #[default]
    AllEntries,
    GamesOnly,
}

impl ContentSelectionPolicy {
    pub const ALL: [Self; 2] = [Self::AllEntries, Self::GamesOnly];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllEntries => "all_entries",
            Self::GamesOnly => "games_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|policy| policy.as_str() == value.to_ascii_lowercase())
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AllEntries => "All entries",
            Self::GamesOnly => "Games only",
        }
    }

    pub fn eligibility(self, classification: &DatContentClassification) -> ContentEligibility {
        match self {
            Self::AllEntries => ContentEligibility::Selected,
            Self::GamesOnly => match classification.class {
                DatContentClass::Game
                | DatContentClass::GameCompilation
                | DatContentClass::RequiredMultidiscPart
                    if classification.confidence != ClassifierConfidence::None =>
                {
                    ContentEligibility::Selected
                }
                DatContentClass::NonGame
                    if classification.confidence != ClassifierConfidence::None =>
                {
                    ContentEligibility::ExcludedNonGame
                }
                _ => ContentEligibility::NeedsReview,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentEligibility {
    Selected,
    ExcludedNonGame,
    NeedsReview,
}

/// Orthogonal catalogue counts. `total` always equals the full parsed entry
/// count; the selection policy never changes it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatContentSummary {
    pub total: usize,
    pub games: usize,
    pub game_compilations: usize,
    pub required_multidisc_parts: usize,
    pub non_game: usize,
    pub unknown: usize,
    pub selected: usize,
    pub excluded_non_game: usize,
    pub needs_review: usize,
}

pub fn summarize(games: &[DatGameEntry], policy: ContentSelectionPolicy) -> DatContentSummary {
    let mut summary = DatContentSummary {
        total: games.len(),
        ..Default::default()
    };
    for game in games {
        match game.content_classification.class {
            DatContentClass::Game => summary.games += 1,
            DatContentClass::GameCompilation => summary.game_compilations += 1,
            DatContentClass::RequiredMultidiscPart => summary.required_multidisc_parts += 1,
            DatContentClass::NonGame => summary.non_game += 1,
            DatContentClass::Unknown => summary.unknown += 1,
        }
        match policy.eligibility(&game.content_classification) {
            ContentEligibility::Selected => summary.selected += 1,
            ContentEligibility::ExcludedNonGame => summary.excluded_non_game += 1,
            ContentEligibility::NeedsReview => summary.needs_review += 1,
        }
    }
    summary
}

/// Annotates every parsed entry in place. The entry vector and every upstream
/// identity field remain untouched.
pub fn classify_catalogue(dat: &mut ParsedDat) {
    let ecosystem = dat.source.ecosystem;
    let source_name = dat.source.name.as_deref();
    for game in &mut dat.games {
        game.content_classification = classify_entry(ecosystem, source_name, game);
    }
}

fn classify_entry(
    ecosystem: DatEcosystem,
    source_name: Option<&str>,
    game: &DatGameEntry,
) -> DatContentClassification {
    // An explicit recognized field wins regardless of container ecosystem.
    // Generic formats gain no meaning merely from being Logiqx/ClrMamePro,
    // but a field that the actual import carries is evidence in its own right.
    if let Some(classification) = classify_structured(&game.original_metadata) {
        return classification;
    }

    if ecosystem == DatEcosystem::Tosec
        && let Some(source_name) = source_name
        && let Some((base_class, category)) = classify_tosec_source(source_name)
    {
        if matches!(
            base_class,
            DatContentClass::Game | DatContentClass::GameCompilation
        ) && let Some(token) = strict_multidisc_token(&game.name)
        {
            return classification(
                DatContentClass::RequiredMultidiscPart,
                ClassifierConfidence::High,
                vec![
                    evidence(
                        ClassificationEvidenceKind::CanonicalTosecSourceCategory,
                        Some("source.name"),
                        Some(category),
                        "tosec.source_category",
                    ),
                    evidence(
                        ClassificationEvidenceKind::CanonicalTosecMediaToken,
                        Some("entry.name"),
                        Some(token),
                        "tosec.required_multidisc_token",
                    ),
                ],
            );
        }
        return classification(
            base_class,
            ClassifierConfidence::High,
            vec![evidence(
                ClassificationEvidenceKind::CanonicalTosecSourceCategory,
                Some("source.name"),
                Some(category),
                "tosec.source_category",
            )],
        );
    }

    DatContentClassification::unknown()
}

fn classify_structured(metadata: &DatOriginalMetadata) -> Option<DatContentClassification> {
    for key in ["category", "type", "content_type"] {
        let Some(raw) = metadata.fields.get(key) else {
            continue;
        };
        let normalized = normalize_category(raw);
        let class = if matches!(normalized.as_str(), "game" | "games") {
            DatContentClass::Game
        } else if matches!(
            normalized.as_str(),
            "game compilation" | "games compilation" | "compilation games"
        ) {
            DatContentClass::GameCompilation
        } else if is_non_game_category(&normalized) {
            DatContentClass::NonGame
        } else {
            continue;
        };
        return Some(classification(
            class,
            ClassifierConfidence::High,
            vec![evidence(
                ClassificationEvidenceKind::StructuredEntryMetadata,
                Some(key),
                Some(raw.clone()),
                "structured.entry_category",
            )],
        ));
    }
    None
}

fn classify_tosec_source(source_name: &str) -> Option<(DatContentClass, String)> {
    let parts: Vec<_> = source_name.split(" - ").map(normalize_category).collect();
    let has_games = parts.iter().any(|part| part == "game" || part == "games");
    let has_compilation = parts
        .iter()
        .any(|part| part == "compilation" || part == "compilations");
    if has_games && has_compilation {
        return Some((DatContentClass::GameCompilation, source_name.to_string()));
    }
    if has_games {
        return Some((DatContentClass::Game, source_name.to_string()));
    }
    if parts.iter().any(|part| is_non_game_category(part)) {
        return Some((DatContentClass::NonGame, source_name.to_string()));
    }
    None
}

fn is_non_game_category(category: &str) -> bool {
    matches!(
        category,
        "application"
            | "applications"
            | "bios"
            | "coverdisc"
            | "coverdiscs"
            | "coverdisk"
            | "coverdisks"
            | "demo"
            | "demos"
            | "device driver"
            | "device drivers"
            | "documentation"
            | "educational"
            | "firmware"
            | "magazine"
            | "magazines"
            | "manual"
            | "manuals"
            | "multimedia"
            | "music"
            | "operating system"
            | "operating systems"
            | "sampler"
            | "samplers"
            | "theme"
            | "themes"
            | "utilities"
            | "utility"
            | "video"
    )
}

fn normalize_category(raw: &str) -> String {
    raw.trim()
        .to_ascii_lowercase()
        .replace(['_', '/'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Accept only a complete, delimited TOSEC-style media token. A title merely
/// containing words such as "Disc" or "Demo" is never evidence.
fn strict_multidisc_token(name: &str) -> Option<String> {
    let mut rest = name;
    while let Some(close) = rest.rfind(')') {
        let before_close = &rest[..close];
        let Some(open) = before_close.rfind('(') else {
            break;
        };
        let token = &before_close[open + 1..];
        let words: Vec<_> = token.split_whitespace().collect();
        if words.len() == 4
            && matches!(
                words[0].to_ascii_lowercase().as_str(),
                "disc" | "disk" | "part" | "side"
            )
            && words[2].eq_ignore_ascii_case("of")
            && let (Ok(part), Ok(total)) = (words[1].parse::<u16>(), words[3].parse::<u16>())
            && total > 1
            && part > 0
            && part <= total
        {
            return Some(token.to_string());
        }
        rest = &before_close[..open];
    }
    None
}

fn classification(
    class: DatContentClass,
    confidence: ClassifierConfidence,
    evidence: Vec<ClassificationEvidence>,
) -> DatContentClassification {
    DatContentClassification {
        class,
        confidence,
        evidence,
        classifier_version: CLASSIFIER_VERSION.to_string(),
    }
}

fn evidence(
    kind: ClassificationEvidenceKind,
    field: Option<&str>,
    original_value: Option<String>,
    rule: &str,
) -> ClassificationEvidence {
    ClassificationEvidence {
        kind,
        field: field.map(str::to_string),
        original_value,
        rule: rule.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::model::{DatFormat, DatRomEntry, DatSource};

    fn dat(ecosystem: DatEcosystem, source_name: &str, names: &[&str]) -> ParsedDat {
        ParsedDat {
            source: DatSource {
                format: DatFormat::ClrMamePro,
                ecosystem,
                file_path: "fixture.dat".into(),
                name: Some(source_name.into()),
                description: None,
                version: None,
                author: None,
                homepage: None,
                clrmamepro_header: None,
                entry_count: names.len(),
                rom_count: names.len(),
                parse_warnings: Vec::new(),
            },
            games: names
                .iter()
                .map(|name| DatGameEntry {
                    name: (*name).into(),
                    description: None,
                    roms: vec![DatRomEntry {
                        name: format!("{name}.bin"),
                        size_bytes: None,
                        crc32: None,
                        md5: None,
                        sha1: None,
                        sha256: None,
                        status: None,
                        merge: None,
                        date: None,
                    }],
                    clone_of: None,
                    sample_of: None,
                    board: None,
                    rebuild_to: None,
                    year: None,
                    manufacturer: None,
                    source_file: None,
                    comment: None,
                    original_metadata: DatOriginalMetadata::default(),
                    content_classification: DatContentClassification::unknown(),
                })
                .collect(),
        }
    }

    #[test]
    fn tosec_games_and_non_games_use_exact_source_categories() {
        let mut games = dat(
            DatEcosystem::Tosec,
            "Commodore Amiga - Games - ADF",
            &["Lotus"],
        );
        classify_catalogue(&mut games);
        assert_eq!(
            games.games[0].content_classification.class,
            DatContentClass::Game
        );

        let mut apps = dat(
            DatEcosystem::Tosec,
            "Commodore Amiga - Applications - ADF",
            &["Workbench Tool"],
        );
        classify_catalogue(&mut apps);
        assert_eq!(
            apps.games[0].content_classification.class,
            DatContentClass::NonGame
        );
    }

    #[test]
    fn tosec_compilations_and_every_multidisc_part_are_retained() {
        let mut compilation = dat(
            DatEcosystem::Tosec,
            "Commodore Amiga - Games - Compilations - ADF",
            &["Arcade Collection"],
        );
        classify_catalogue(&mut compilation);
        assert_eq!(
            compilation.games[0].content_classification.class,
            DatContentClass::GameCompilation
        );

        let mut multidisc = dat(
            DatEcosystem::Tosec,
            "Commodore Amiga - Games - ADF",
            &["Quest (Disk 1 of 2)", "Quest (Disk 2 of 2)"],
        );
        classify_catalogue(&mut multidisc);
        assert!(multidisc.games.iter().all(|entry| {
            entry.content_classification.class == DatContentClass::RequiredMultidiscPart
        }));
        assert_eq!(
            summarize(&multidisc.games, ContentSelectionPolicy::GamesOnly).selected,
            2
        );
    }

    #[test]
    fn structured_no_intro_metadata_is_preserved_and_classified() {
        let mut parsed = dat(DatEcosystem::NoIntro, "No-Intro", &["Example"]);
        parsed.games[0]
            .original_metadata
            .fields
            .insert("category".into(), "Games".into());
        classify_catalogue(&mut parsed);
        let result = &parsed.games[0].content_classification;
        assert_eq!(result.class, DatContentClass::Game);
        assert_eq!(result.evidence[0].original_value.as_deref(), Some("Games"));
        assert_eq!(
            parsed.games[0].original_metadata.fields["category"],
            "Games"
        );
    }

    #[test]
    fn generic_and_plain_redump_entries_remain_unknown() {
        for ecosystem in [DatEcosystem::GenericLogiqx, DatEcosystem::Redump] {
            let mut parsed = dat(ecosystem, "Catalogue", &["Game Demo Magazine BIOS"]);
            classify_catalogue(&mut parsed);
            assert_eq!(
                parsed.games[0].content_classification.class,
                DatContentClass::Unknown
            );
            assert_eq!(
                ContentSelectionPolicy::GamesOnly
                    .eligibility(&parsed.games[0].content_classification),
                ContentEligibility::NeedsReview
            );
        }
    }

    #[test]
    fn generic_logiqx_uses_an_explicit_recognized_category_but_never_the_title() {
        let mut parsed = dat(
            DatEcosystem::GenericLogiqx,
            "Unbranded catalogue",
            &["Magazine Demo BIOS"],
        );
        parsed.games[0]
            .original_metadata
            .fields
            .insert("category".into(), "Games".into());
        classify_catalogue(&mut parsed);
        assert_eq!(
            parsed.games[0].content_classification.class,
            DatContentClass::Game
        );
        assert_eq!(
            parsed.games[0].content_classification.evidence[0]
                .original_value
                .as_deref(),
            Some("Games")
        );
    }

    #[test]
    fn games_only_excludes_only_confident_non_game_and_all_restores_everything() {
        let game = classification(DatContentClass::Game, ClassifierConfidence::High, vec![]);
        let compilation = classification(
            DatContentClass::GameCompilation,
            ClassifierConfidence::High,
            vec![],
        );
        let part = classification(
            DatContentClass::RequiredMultidiscPart,
            ClassifierConfidence::High,
            vec![],
        );
        let non_game = classification(DatContentClass::NonGame, ClassifierConfidence::High, vec![]);
        let unknown = DatContentClassification::unknown();
        for included in [&game, &compilation, &part] {
            assert_eq!(
                ContentSelectionPolicy::GamesOnly.eligibility(included),
                ContentEligibility::Selected
            );
        }
        assert_eq!(
            ContentSelectionPolicy::GamesOnly.eligibility(&non_game),
            ContentEligibility::ExcludedNonGame
        );
        assert_eq!(
            ContentSelectionPolicy::GamesOnly.eligibility(&unknown),
            ContentEligibility::NeedsReview
        );
        for value in [&game, &compilation, &part, &non_game, &unknown] {
            assert_eq!(
                ContentSelectionPolicy::AllEntries.eligibility(value),
                ContentEligibility::Selected
            );
        }
    }
}
