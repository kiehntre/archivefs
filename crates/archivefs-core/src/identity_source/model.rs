//! The reusable external-identity model.
//!
//! Deliberately provider-agnostic. RomM is the only provider in Stage 1, but the
//! shape here is what a local DAT catalogue or a Hasheous lookup would fill in
//! too, so adding one of those later means writing an adapter rather than
//! reshaping the model.
//!
//! # Evidence, not truth
//!
//! An external record says what someone else concluded. ArchiveFS keeps that
//! separate from what it verified itself, compares the two, and reports the
//! comparison. [`ExternalVerification`] is the outcome of that comparison, and
//! [`IdentityConflict`] is what is retained when the two disagree - both are
//! kept, and neither silently wins.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which external source a record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityProvider {
    Romm,
}

impl IdentityProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Romm => "RomM",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Romm => "romm",
        }
    }
}

/// A hash an external source published, with the algorithm it used.
///
/// The algorithm is carried rather than implied, because an external source may
/// offer several and ArchiveFS must only ever compare like with like.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalHash {
    pub algorithm: HashAlgorithm,
    /// Lowercase hexadecimal, as normalised on import.
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashAlgorithm {
    Crc32,
    Md5,
    Sha1,
}

impl HashAlgorithm {
    pub fn label(self) -> &'static str {
        match self {
            Self::Crc32 => "CRC32",
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
        }
    }

    /// The exact length of this algorithm's hexadecimal form. Used to reject a
    /// value that cannot be what it claims to be.
    pub fn hex_length(self) -> usize {
        match self {
            Self::Crc32 => 8,
            Self::Md5 => 32,
            Self::Sha1 => 40,
        }
    }
}

impl ExternalHash {
    /// Normalises and validates one published hash.
    ///
    /// Returns `None` for anything that is not the right length of hexadecimal,
    /// so a malformed or placeholder value never becomes evidence.
    pub fn parse(algorithm: HashAlgorithm, value: &str) -> Option<Self> {
        let trimmed = value.trim().to_ascii_lowercase();
        if trimmed.len() != algorithm.hex_length()
            || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit())
        {
            return None;
        }
        Some(Self {
            algorithm,
            value: trimmed,
        })
    }
}

/// A metadata-provider identifier the external source had already resolved.
///
/// Kept as provenance only. ArchiveFS makes no network request to any of these
/// in Stage 1; they are recorded so a later stage, or a person, can follow them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataProviderId {
    pub provider: String,
    pub id: String,
}

/// A reference to artwork the external source owns.
///
/// A reference, never the bytes: the source remains the owner of full-size
/// artwork, and ArchiveFS stores only enough to fetch a thumbnail lazily.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtworkReference {
    /// A path relative to the provider's own base, or an absolute URL the
    /// provider published. Never resolved at import time.
    pub reference: String,
    /// The smaller of the provider's variants, when it publishes more than one.
    pub small_reference: Option<String>,
}

/// How well an external record agrees with what ArchiveFS can see locally.
///
/// The ordering is meaningful: a stronger variant is a stronger claim, and
/// [`ExternalVerification::outranks`] is what stops a weaker external record
/// from displacing stronger local evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalVerification {
    /// No safe local path match at all - the record describes a file ArchiveFS
    /// does not have.
    Unmatched,
    /// The matched file is gone, or has materially changed since import.
    Stale,
    /// The external record contradicts something ArchiveFS established locally.
    /// No side wins; the conflict is retained.
    Ambiguous,
    /// Path, title and platform agree, but nothing was hash-verified.
    ProbableExternal,
    /// Translated path, file size and platform all agree.
    StrongExternal,
    /// An external hash and a locally computed hash of the same algorithm agree.
    ConfirmedExternal,
}

impl ExternalVerification {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unmatched => "Unmatched",
            Self::Stale => "Stale",
            Self::Ambiguous => "Ambiguous",
            Self::ProbableExternal => "Probable (external)",
            Self::StrongExternal => "Strong (external)",
            Self::ConfirmedExternal => "Confirmed (external)",
        }
    }

    /// A one-line explanation of what this level actually rests on.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Unmatched => "no local file matched this record's path, so nothing was compared",
            Self::Stale => {
                "the file this record described is missing or has changed since it was imported"
            }
            Self::Ambiguous => {
                "the external record disagrees with what ArchiveFS determined locally, so neither \
                 is treated as settled"
            }
            Self::ProbableExternal => {
                "the path, title and platform agree, but no hash was compared"
            }
            Self::StrongExternal => "the path, file size and platform all agree",
            Self::ConfirmedExternal => {
                "an external hash and a locally computed hash of the same algorithm agree"
            }
        }
    }

    /// Whether this level is usable as identity at all, as opposed to being a
    /// record of a problem.
    pub fn is_usable(self) -> bool {
        matches!(
            self,
            Self::ProbableExternal | Self::StrongExternal | Self::ConfirmedExternal
        )
    }

    /// Whether this external level may be presented ahead of local evidence of
    /// the given strength.
    ///
    /// External evidence never displaces a local *verified* identity: ArchiveFS
    /// reading a game's own header beats someone else's database saying
    /// otherwise, and where they differ the answer is a conflict, not a swap.
    pub fn outranks(self, local: LocalEvidenceStrength) -> bool {
        match local {
            LocalEvidenceStrength::None => self.is_usable(),
            LocalEvidenceStrength::Weak => {
                matches!(self, Self::StrongExternal | Self::ConfirmedExternal)
            }
            // A locally verified identity is never displaced by an external
            // record, however confident that record is.
            LocalEvidenceStrength::Verified => false,
        }
    }
}

/// How strong ArchiveFS's own evidence is for the same file.
///
/// Kept deliberately coarse: this is only ever used to decide whether an
/// external record may lead, and a finer scale would invite pretending to a
/// precision the comparison does not have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalEvidenceStrength {
    /// ArchiveFS has no identity of its own for this file. The default, so a
    /// caller that has not looked cannot accidentally claim it has.
    #[default]
    None,
    /// A folder alias, an extension, or another non-conclusive signal.
    Weak,
    /// A signature, header or hash ArchiveFS computed itself.
    Verified,
}

/// One specific disagreement between an external record and local evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityConflict {
    pub field: ConflictField,
    /// What the external source says.
    pub external: String,
    /// What ArchiveFS determined locally.
    pub local: String,
    /// Why it matters, in a person's words.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictField {
    Platform,
    Hash,
    FileSize,
    Signature,
    FileState,
}

impl ConflictField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Platform => "Platform",
            Self::Hash => "Hash",
            Self::FileSize => "File size",
            Self::Signature => "Format signature",
            Self::FileState => "File state",
        }
    }
}

/// One imported identity record.
///
/// Every field the provider published that ArchiveFS can use, plus the
/// provenance needed to explain where it came from and when. Nothing here is
/// derived at display time: the comparison happens once, at import, and its
/// outcome is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalIdentityRecord {
    pub provider: IdentityProvider,
    /// Which instance this came from, so two servers' records never merge.
    /// A stable, non-secret identifier - the approved origin, never a token.
    pub server_id: String,
    /// The provider's own platform identifier.
    pub provider_platform_id: Option<String>,
    /// The provider's own game identifier.
    pub provider_game_id: String,
    /// The provider's own file identifier, where a game has several files.
    pub provider_file_id: Option<String>,
    /// The path as the provider knows it, kept verbatim for provenance.
    pub provider_path: String,
    /// The path in ArchiveFS's own terms, after mapping. `None` when no mapping
    /// applied, which is what [`ExternalVerification::Unmatched`] describes.
    pub archivefs_path: Option<PathBuf>,
    pub title: Option<String>,
    /// The canonical ArchiveFS platform this record suggests, when the
    /// provider's platform could be mapped to one.
    pub platform_candidate: Option<String>,
    /// The provider's own platform name, kept even when it could not be mapped.
    pub provider_platform_name: Option<String>,
    pub regions: Vec<String>,
    pub revision: Option<String>,
    pub hashes: Vec<ExternalHash>,
    pub file_size_bytes: Option<u64>,
    pub metadata_provider_ids: Vec<MetadataProviderId>,
    pub artwork: Option<ArtworkReference>,
    /// Related files - the other discs of a multi-disc release, or the members
    /// of a multi-file game - as the provider expressed them.
    pub related_files: Vec<String>,
    /// Sibling records the provider links to this one.
    pub sibling_game_ids: Vec<String>,
    /// When ArchiveFS imported it, as a Unix timestamp.
    pub imported_at_unix_seconds: i64,
    /// When the provider last changed it, where the provider says.
    pub provider_updated_at: Option<String>,
    pub verification: ExternalVerification,
    pub conflicts: Vec<IdentityConflict>,
    /// The observed facts behind the verification level.
    pub evidence: Vec<String>,
}

impl ExternalIdentityRecord {
    /// The best hash of a given algorithm, if the provider published one.
    pub fn hash(&self, algorithm: HashAlgorithm) -> Option<&ExternalHash> {
        self.hashes.iter().find(|hash| hash.algorithm == algorithm)
    }

    /// The strongest hash available, preferring SHA-1 over MD5 over CRC32.
    pub fn strongest_hash(&self) -> Option<&ExternalHash> {
        self.hashes.iter().max_by_key(|hash| hash.algorithm)
    }

    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    /// A one-line summary for a list view.
    pub fn summary(&self) -> String {
        format!(
            "{} - {} ({})",
            self.title.as_deref().unwrap_or("(untitled)"),
            self.platform_candidate
                .as_deref()
                .or(self.provider_platform_name.as_deref())
                .unwrap_or("unknown platform"),
            self.verification.label()
        )
    }
}

/// Counts across an imported set, for a status view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityImportCounts {
    pub total: usize,
    pub confirmed: usize,
    pub strong: usize,
    pub probable: usize,
    pub ambiguous: usize,
    pub stale: usize,
    pub unmatched: usize,
    pub with_hashes: usize,
    pub with_artwork: usize,
    pub multi_file: usize,
}

impl IdentityImportCounts {
    pub fn of(records: &[ExternalIdentityRecord]) -> Self {
        let mut counts = Self {
            total: records.len(),
            ..Self::default()
        };
        for record in records {
            match record.verification {
                ExternalVerification::ConfirmedExternal => counts.confirmed += 1,
                ExternalVerification::StrongExternal => counts.strong += 1,
                ExternalVerification::ProbableExternal => counts.probable += 1,
                ExternalVerification::Ambiguous => counts.ambiguous += 1,
                ExternalVerification::Stale => counts.stale += 1,
                ExternalVerification::Unmatched => counts.unmatched += 1,
            }
            if !record.hashes.is_empty() {
                counts.with_hashes += 1;
            }
            if record.artwork.is_some() {
                counts.with_artwork += 1;
            }
            if !record.related_files.is_empty() {
                counts.multi_file += 1;
            }
        }
        counts
    }

    /// Records that are usable as identity.
    pub fn usable(&self) -> usize {
        self.confirmed + self.strong + self.probable
    }
}
