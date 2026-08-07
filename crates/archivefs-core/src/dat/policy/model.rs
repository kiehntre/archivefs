//! Typed vocabulary for DAT matching policy.
//!
//! This module defines the *values* the user-authored DAT policy can name:
//! canonical region identifiers, canonical language identifiers, the language
//! preferences a list can contain, and the two behaviour enums (revision
//! policy, clone policy). Everything here is small, pure, and deliberately
//! free of filesystem or GUI concerns so the effective-policy resolver
//! ([`super::evaluate`]) can be tested exhaustively.
//!
//! # Canonical regions are a deliberate, small set
//!
//! The approved design (model §7) gives region preferences as IDs such as
//! `World`, `USA`, `Europe`, `Japan`. This build's canonical set is exactly
//! those four plus `Other`. Everything a catalogue name tags that is a region
//! but is not one of the four - `Germany`, `Brazil`, `Asia`, … - maps to
//! `Other` (see [`super::tags::regions_of_name`]). Keeping the set small keeps
//! validation, the GUI, and the tests bounded; growing the vocabulary later is
//! an additive change, never a rename.
//!
//! # Language preferences are more than a flat list
//!
//! `language_preferences` is an ordered list whose entries are either a
//! specific [`LanguageId`] (`en`, `ja`, …), [`LanguagePreference::MultiLanguage`]
//! ("any entry whose catalogue name carries more than one language tag"), or
//! [`LanguagePreference::OriginalLanguage`] ("the release's own language", as
//! inferred from its region tag). This is what lets a user express the four
//! preferences the design asks for - a specific language, multi-language, the
//! original language, or a bespoke ordering of any of those.

use serde::{Deserialize, Serialize};

/// The longest a preference list (`region_preferences`,
/// `language_preferences`) may be, matching the design document's cap.
pub const MAX_POLICY_PREFERENCE_LEN: usize = 16;

/// A canonical region identifier.
///
/// The order of [`RegionId::ALL`] is the order the GUI presents the regions in
/// when a user builds a preference list from scratch. It deliberately matches
/// the design's example ordering (Europe, USA, Japan, World, Other) so a
/// user-typed list and the on-screen list agree without extra ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionId {
    Europe,
    Usa,
    Japan,
    World,
    Other,
}

impl RegionId {
    /// Every canonical region, in presentation order.
    pub const ALL: [RegionId; 5] = [
        RegionId::Europe,
        RegionId::Usa,
        RegionId::Japan,
        RegionId::World,
        RegionId::Other,
    ];

    /// The stable serialised identifier, as persisted and as used in
    /// `region_preferences`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Europe => "europe",
            Self::Usa => "usa",
            Self::Japan => "japan",
            Self::World => "world",
            Self::Other => "other",
        }
    }

    /// What a person sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::Europe => "Europe",
            Self::Usa => "USA",
            Self::Japan => "Japan",
            Self::World => "World",
            Self::Other => "Other",
        }
    }

    /// Parses a stored identifier. Case-insensitive, so a hand-edited file
    /// that spells `USA` with capitals still resolves.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|region| region.as_str() == value.to_ascii_lowercase())
    }
}

/// A canonical language identifier.
///
/// Language IDs are the ISO 639-1 two-letter codes most catalogue names use
/// for their `(En,Fr,De)` suffix. The set is bounded to the codes that
/// actually appear in real No-Intro / Redump names; anything else is rejected
/// at validation rather than silently accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageId {
    En,
    Ja,
    De,
    Fr,
    Es,
    It,
    Pt,
    Ru,
    Zh,
    Ko,
    Nl,
    Sv,
    No,
    Da,
    Fi,
    Pl,
    El,
    Tr,
    Cs,
    Hu,
    Ar,
    He,
    Hi,
    Th,
    Id,
    Vi,
}

impl LanguageId {
    /// Every supported language, sorted by code so derived lists are stable.
    pub const ALL: [LanguageId; 26] = [
        LanguageId::En,
        LanguageId::Ja,
        LanguageId::De,
        LanguageId::Fr,
        LanguageId::Es,
        LanguageId::It,
        LanguageId::Pt,
        LanguageId::Ru,
        LanguageId::Zh,
        LanguageId::Ko,
        LanguageId::Nl,
        LanguageId::Sv,
        LanguageId::No,
        LanguageId::Da,
        LanguageId::Fi,
        LanguageId::Pl,
        LanguageId::El,
        LanguageId::Tr,
        LanguageId::Cs,
        LanguageId::Hu,
        LanguageId::Ar,
        LanguageId::He,
        LanguageId::Hi,
        LanguageId::Th,
        LanguageId::Id,
        LanguageId::Vi,
    ];

    /// The stable serialised identifier (the ISO code).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
            Self::De => "de",
            Self::Fr => "fr",
            Self::Es => "es",
            Self::It => "it",
            Self::Pt => "pt",
            Self::Ru => "ru",
            Self::Zh => "zh",
            Self::Ko => "ko",
            Self::Nl => "nl",
            Self::Sv => "sv",
            Self::No => "no",
            Self::Da => "da",
            Self::Fi => "fi",
            Self::Pl => "pl",
            Self::El => "el",
            Self::Tr => "tr",
            Self::Cs => "cs",
            Self::Hu => "hu",
            Self::Ar => "ar",
            Self::He => "he",
            Self::Hi => "hi",
            Self::Th => "th",
            Self::Id => "id",
            Self::Vi => "vi",
        }
    }

    /// What a person sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ja => "Japanese",
            Self::De => "German",
            Self::Fr => "French",
            Self::Es => "Spanish",
            Self::It => "Italian",
            Self::Pt => "Portuguese",
            Self::Ru => "Russian",
            Self::Zh => "Chinese",
            Self::Ko => "Korean",
            Self::Nl => "Dutch",
            Self::Sv => "Swedish",
            Self::No => "Norwegian",
            Self::Da => "Danish",
            Self::Fi => "Finnish",
            Self::Pl => "Polish",
            Self::El => "Greek",
            Self::Tr => "Turkish",
            Self::Cs => "Czech",
            Self::Hu => "Hungarian",
            Self::Ar => "Arabic",
            Self::He => "Hebrew",
            Self::Hi => "Hindi",
            Self::Th => "Thai",
            Self::Id => "Indonesian",
            Self::Vi => "Vietnamese",
        }
    }

    /// Parses a stored identifier.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|language| language.as_str() == value.to_ascii_lowercase())
    }
}

/// One entry in an ordered language preference list.
///
/// [`LanguagePreference::Language`] names a specific language; the other two
/// values describe a property of the candidate rather than a code. Multi and
/// Original are deliberately *preference entries*, not languages, because
/// `en`, `ja`, `de` already mean "the candidate carries this language tag" and
/// cannot also mean "the candidate is multilingual".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguagePreference {
    Language(LanguageId),
    /// The candidate's name carries more than one language tag.
    MultiLanguage,
    /// The candidate is in its own release's language, as inferred from its
    /// region tag (see [`super::evaluate::original_language_of`]).
    OriginalLanguage,
}

impl LanguagePreference {
    /// The stable serialised identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Language(language) => language.as_str(),
            Self::MultiLanguage => "multi",
            Self::OriginalLanguage => "original",
        }
    }

    /// What a person sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::Language(language) => language.label(),
            Self::MultiLanguage => "Multi-language",
            Self::OriginalLanguage => "Original language",
        }
    }

    /// Parses a stored identifier.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "multi" => Some(Self::MultiLanguage),
            "original" => Some(Self::OriginalLanguage),
            _ => LanguageId::parse(value).map(Self::Language),
        }
    }
}

/// How the policy chooses between candidates that differ only by revision.
///
/// A catalogue entry's revision is read from its name's `(Rev N)` / `(Rev A)`
/// marker; an entry with no marker is the *original* dump (revision 0). The
/// safe default is [`RevisionPolicy::AskWhenAmbiguous`]: the policy never
/// uses revision to pick a winner, which is exactly what today's audit does
/// when it reports every candidate without choosing one.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum RevisionPolicy {
    /// Prefer the newest verified revision (highest marker); a marked entry
    /// outranks an unmarked one.
    LatestVerified,
    /// Prefer the earliest verified revision (lowest marker); the unmarked
    /// original is earliest.
    EarliestVerified,
    /// Prefer the original dump - the entry with no revision marker.
    PreferOriginal,
    /// Never let revision decide; a tie that revision alone could break stays
    /// ambiguous.
    #[default]
    AskWhenAmbiguous,
}

impl RevisionPolicy {
    pub const ALL: [RevisionPolicy; 4] = [
        RevisionPolicy::LatestVerified,
        RevisionPolicy::EarliestVerified,
        RevisionPolicy::PreferOriginal,
        RevisionPolicy::AskWhenAmbiguous,
    ];

    /// The stable serialised identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LatestVerified => "latest_verified",
            Self::EarliestVerified => "earliest_verified",
            Self::PreferOriginal => "prefer_original",
            Self::AskWhenAmbiguous => "ask_when_ambiguous",
        }
    }

    /// What a person sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::LatestVerified => "Latest verified revision",
            Self::EarliestVerified => "Earliest verified revision",
            Self::PreferOriginal => "Prefer original",
            Self::AskWhenAmbiguous => "Ask when ambiguous",
        }
    }

    /// Parses a stored identifier.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|policy| policy.as_str() == value.to_ascii_lowercase())
    }
}

/// How the policy treats entries that declare a parent (`clone_of`).
///
/// Safe default is [`ClonePolicy::KeepAllVariants`]: parent relationships are
/// ignored and every candidate is retained, which is exactly what today's DAT
/// audit does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ClonePolicy {
    /// A parent always outranks a clone of it, whatever region or language
    /// says.
    PreferParent,
    /// A clone may outrank its parent only when the region/language
    /// preferences already make the clone preferable; a tie between them
    /// stays with the parent.
    PreferClone,
    /// Parent relationships are ignored; every candidate is kept and ranked
    /// by the other criteria alone.
    #[default]
    KeepAllVariants,
    /// Do not choose between a clone and its parent automatically: if the
    /// top candidates are a parent/clone pair that the other criteria did not
    /// separate, the resolution is ambiguous.
    RequireExplicitChoice,
}

impl ClonePolicy {
    pub const ALL: [ClonePolicy; 4] = [
        ClonePolicy::PreferParent,
        ClonePolicy::PreferClone,
        ClonePolicy::KeepAllVariants,
        ClonePolicy::RequireExplicitChoice,
    ];

    /// The stable serialised identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreferParent => "prefer_parent",
            Self::PreferClone => "prefer_clone",
            Self::KeepAllVariants => "keep_all_variants",
            Self::RequireExplicitChoice => "require_explicit_choice",
        }
    }

    /// What a person sees.
    pub fn label(self) -> &'static str {
        match self {
            Self::PreferParent => "Prefer parent",
            Self::PreferClone => "Prefer clone when its region/language fits",
            Self::KeepAllVariants => "Keep all variants",
            Self::RequireExplicitChoice => "Require explicit choice when ambiguous",
        }
    }

    /// Parses a stored identifier.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|policy| policy.as_str() == value.to_ascii_lowercase())
    }
}

/// Which policy field a value belongs to. Used for validation reporting and
/// for the Effective Policy Summary's "source of this value" tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PolicyField {
    Region,
    Language,
    Revision,
    Clone,
}

impl PolicyField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Region => "region preference",
            Self::Language => "language preference",
            Self::Revision => "revision policy",
            Self::Clone => "clone policy",
        }
    }
}

/// Which scope supplied a resolved policy value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PolicyScope {
    /// The document's global preferences.
    Global,
    /// A per-platform override in the document's `platforms` map.
    PlatformOverride,
}

impl PolicyScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::PlatformOverride => "Platform override",
        }
    }
}
