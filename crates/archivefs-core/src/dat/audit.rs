//! Read-only audit using hashes already known to EmuWiz.
//!
//! Every verdict is derived from a comparison between what the DAT file claims
//! and what EmuWiz already knows. Nothing here hashes a local file: the
//! caller supplies known hashes and size, and the audit logic compares them
//! against the indexed DAT entries.
//!
//! # Verdict rules
//!
//! - **Exact**: SHA-256, SHA-1, or MD5 matches exactly one DAT entry.
//! - **ExactMultipleCandidates**: a cryptographic hash matches several entries.
//! - **Probable**: CRC32 (with size, where both are known) matches one entry.
//! - **ProbableMultipleCandidates**: CRC32 matches several entries.
//! - **FilenameOnly**: filename matches, and no hash evidence was available.
//! - **Ambiguous**: candidates exist, but the evidence conflicts.
//! - **NotInDat**: every hash that could be compared found no candidate.
//! - **NoUsableEvidence**: no hash to compare, and the filename matched nothing.
//!
//! # Every shared algorithm is tried, not just the strongest
//!
//! A DAT carries the algorithms its publisher chose; the caller knows whatever
//! EmuWiz happens to have computed. The two sets overlap but neither contains
//! the other - a No-Intro DAT publishes CRC32/MD5/SHA-1 and no SHA-256, so
//! stopping at the strongest hash the *caller* holds reports a perfectly matching
//! file as absent from the catalogue. Each algorithm is tried in descending order
//! of strength and the first that finds any candidate decides the verdict.

use serde::Serialize;

use super::index::DatIndex;

/// The outcome of comparing a single known file against a DAT index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditVerdict {
    /// SHA-256, SHA-1, or MD5 exact match against exactly one DAT entry.
    Exact {
        game_name: String,
        rom_name: String,
        algorithm: &'static str,
    },
    /// A cryptographic hash matches multiple DAT entries.
    ExactMultipleCandidates {
        algorithm: &'static str,
        count: usize,
        game_names: Vec<String>,
    },
    /// CRC32 plus exact size match.
    Probable { game_name: String, rom_name: String },
    /// CRC32 (with or without size) matches several DAT entries.
    ///
    /// Deliberately *not* `ExactMultipleCandidates`: CRC32 is a 32-bit checksum,
    /// and several entries sharing one is as likely to be a collision as a real
    /// set of identical dumps. Reporting it as an "Exact" verdict would claim a
    /// confidence the evidence does not support.
    ProbableMultipleCandidates {
        algorithm: &'static str,
        count: usize,
        game_names: Vec<String>,
    },
    /// Filename matches, but no hash to confirm.
    FilenameOnly { game_name: String, rom_name: String },
    /// Candidate exists, but evidence conflicts.
    Ambiguous { detail: String },
    /// Known hash has no candidate.
    NotInDat,
    /// No hash available to compare.
    NoUsableEvidence,
}

impl AuditVerdict {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Exact { .. } => "Exact",
            Self::ExactMultipleCandidates { .. } => "Exact (multiple)",
            Self::ProbableMultipleCandidates { .. } => "Probable (multiple)",
            Self::Probable { .. } => "Probable",
            Self::FilenameOnly { .. } => "Filename only",
            Self::Ambiguous { .. } => "Ambiguous",
            Self::NotInDat => "Not in DAT",
            Self::NoUsableEvidence => "No usable evidence",
        }
    }

    /// Whether the verdict rests on a cryptographic hash.
    ///
    /// CRC32 verdicts are excluded however many entries they matched: a 32-bit
    /// checksum agreeing is not the same kind of evidence as SHA-1 agreeing.
    pub fn is_confident(&self) -> bool {
        matches!(
            self,
            Self::Exact { .. } | Self::ExactMultipleCandidates { .. }
        )
    }
}

/// One audited item: a local file compared against the DAT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEntry {
    pub local_path: String,
    pub local_filename: String,
    pub verdict: AuditVerdict,
}

/// The result of an audit pass over a set of local files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditReport {
    pub entries: Vec<AuditEntry>,
    pub summary: AuditSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AuditSummary {
    pub total: usize,
    pub exact: usize,
    pub exact_multiple: usize,
    pub probable: usize,
    pub filename_only: usize,
    pub probable_multiple: usize,
    pub ambiguous: usize,
    pub not_in_dat: usize,
    pub no_evidence: usize,
}

/// Known hashes and metadata for a single local file.
///
/// The caller populates this from existing EmuWiz data — no local
/// hashing is performed inside this module.
#[derive(Debug, Clone, Default)]
pub struct KnownFileEvidence {
    pub filepath: String,
    pub filename: String,
    pub size_bytes: Option<u64>,
    pub crc32: Option<String>,
    pub md5: Option<String>,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
}

impl KnownFileEvidence {
    pub fn new(filepath: impl Into<String>, filename: impl Into<String>) -> Self {
        Self {
            filepath: filepath.into(),
            filename: filename.into(),
            ..Default::default()
        }
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.size_bytes = Some(size);
        self
    }

    pub fn with_crc32(mut self, crc: impl Into<String>) -> Self {
        self.crc32 = Some(crc.into());
        self
    }

    pub fn with_md5(mut self, md5: impl Into<String>) -> Self {
        self.md5 = Some(md5.into());
        self
    }

    pub fn with_sha1(mut self, sha1: impl Into<String>) -> Self {
        self.sha1 = Some(sha1.into());
        self
    }

    pub fn with_sha256(mut self, sha256: impl Into<String>) -> Self {
        self.sha256 = Some(sha256.into());
        self
    }
}

/// Audits a set of known file evidence against a DAT index.
///
/// For each file, tries the strongest hash first (SHA-256 -> SHA-1 -> MD5),
/// then falls back to CRC32+size, then filename.
pub fn audit_files(known: &[KnownFileEvidence], index: &DatIndex) -> AuditReport {
    let mut entries = Vec::with_capacity(known.len());

    for file in known {
        let verdict = audit_one(file, index);
        let filename = std::path::Path::new(&file.filepath)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.filename.clone());
        entries.push(AuditEntry {
            local_path: file.filepath.clone(),
            local_filename: filename,
            verdict,
        });
    }

    let summary = build_summary(&entries);

    AuditReport { entries, summary }
}

pub(crate) fn audit_one(known: &KnownFileEvidence, index: &DatIndex) -> AuditVerdict {
    // Strongest first, but *every* algorithm the caller holds is tried until one
    // finds candidates. Returning on the first algorithm the caller happens to
    // know reports a matching file as absent whenever the DAT does not publish
    // that particular algorithm.
    let mut compared_any = false;

    for (value, algorithm) in [
        (known.sha256.as_deref(), "SHA-256"),
        (known.sha1.as_deref(), "SHA-1"),
        (known.md5.as_deref(), "MD5"),
    ] {
        let Some(value) = value else { continue };
        let Some(normalised) = normalise_for_lookup(value, algorithm) else {
            continue;
        };
        compared_any = true;
        let candidates = match algorithm {
            "SHA-256" => index.lookup_sha256(&normalised),
            "SHA-1" => index.lookup_sha1(&normalised),
            _ => index.lookup_md5(&normalised),
        };
        if !candidates.is_empty() {
            return handle_candidates(candidates, algorithm);
        }
    }

    // CRC32, qualified by size when both are known.
    if let Some(crc) = known.crc32.as_deref()
        && let Some(normalised) = normalise_for_lookup(crc, "CRC32")
    {
        compared_any = true;
        let candidates = index.lookup_crc32(&normalised);
        if !candidates.is_empty() {
            return match known.size_bytes {
                Some(size) => {
                    let matched: Vec<_> = candidates
                        .iter()
                        .filter(|r| r.size_bytes == Some(size))
                        .collect();
                    match matched.len() {
                        1 => AuditVerdict::Probable {
                            game_name: matched[0].game_name.clone(),
                            rom_name: matched[0].rom_name.clone(),
                        },
                        0 => AuditVerdict::Ambiguous {
                            detail: format!(
                                "CRC32 {normalised} matches {} DAT entry(s), but size {size} \
                                 disagrees with all of them",
                                candidates.len()
                            ),
                        },
                        _ => AuditVerdict::ProbableMultipleCandidates {
                            algorithm: "CRC32+size",
                            count: matched.len(),
                            game_names: matched.iter().map(|r| r.game_name.clone()).collect(),
                        },
                    }
                }
                None => match candidates.len() {
                    1 => AuditVerdict::Probable {
                        game_name: candidates[0].game_name.clone(),
                        rom_name: candidates[0].rom_name.clone(),
                    },
                    _ => AuditVerdict::ProbableMultipleCandidates {
                        algorithm: "CRC32",
                        count: candidates.len(),
                        game_names: candidates.iter().map(|r| r.game_name.clone()).collect(),
                    },
                },
            };
        }
    }

    // Something was comparable and none of it found anything: the file is not in
    // this DAT. Falling through to the filename here would dress a name collision
    // up as a match after the hashes had already said otherwise.
    if compared_any {
        return AuditVerdict::NotInDat;
    }

    // No hash at all. A filename match is worth reporting, but only as itself.
    let filename = std::path::Path::new(&known.filepath)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| known.filename.clone());
    if !filename.is_empty() {
        let candidates = index.lookup_filename(&filename);
        if !candidates.is_empty() {
            return AuditVerdict::FilenameOnly {
                game_name: candidates[0].game_name.clone(),
                rom_name: candidates[0].rom_name.clone(),
            };
        }
    }

    AuditVerdict::NoUsableEvidence
}

/// Normalises a caller-supplied hash the same way the index normalised the DAT's.
///
/// The index is keyed on lowercase hex of a fixed length, because that is what
/// the parsers store. A caller holding the same hash in uppercase - which is how
/// plenty of tools print them - would otherwise miss every entry and be told the
/// file is not in the DAT. A value that is not a well-formed hash for its
/// algorithm is not looked up at all.
fn normalise_for_lookup(value: &str, algorithm: &str) -> Option<String> {
    use super::hash::{normalise_crc32, normalise_md5, normalise_sha1, normalise_sha256};
    match algorithm {
        "SHA-256" => normalise_sha256(value),
        "SHA-1" => normalise_sha1(value),
        "MD5" => normalise_md5(value),
        _ => normalise_crc32(value),
    }
}

fn handle_candidates(
    candidates: &[super::index::DatRomRef],
    algorithm: &'static str,
) -> AuditVerdict {
    match candidates.len() {
        0 => AuditVerdict::NotInDat,
        1 => AuditVerdict::Exact {
            game_name: candidates[0].game_name.clone(),
            rom_name: candidates[0].rom_name.clone(),
            algorithm,
        },
        _ => AuditVerdict::ExactMultipleCandidates {
            algorithm,
            count: candidates.len(),
            game_names: candidates.iter().map(|r| r.game_name.clone()).collect(),
        },
    }
}

fn build_summary(entries: &[AuditEntry]) -> AuditSummary {
    let mut summary = AuditSummary {
        total: entries.len(),
        ..Default::default()
    };
    for entry in entries {
        match &entry.verdict {
            AuditVerdict::Exact { .. } => summary.exact += 1,
            AuditVerdict::ExactMultipleCandidates { .. } => summary.exact_multiple += 1,
            AuditVerdict::Probable { .. } => summary.probable += 1,
            AuditVerdict::ProbableMultipleCandidates { .. } => summary.probable_multiple += 1,
            AuditVerdict::FilenameOnly { .. } => summary.filename_only += 1,
            AuditVerdict::Ambiguous { .. } => summary.ambiguous += 1,
            AuditVerdict::NotInDat => summary.not_in_dat += 1,
            AuditVerdict::NoUsableEvidence => summary.no_evidence += 1,
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::model::{
        DatEcosystem, DatFormat, DatGameEntry, DatRomEntry, DatSource, ParsedDat,
    };

    fn make_index() -> DatIndex {
        let dat = ParsedDat {
            source: DatSource {
                format: DatFormat::Logiqx,
                ecosystem: DatEcosystem::GenericLogiqx,
                file_path: "test.dat".into(),
                name: Some("Test".into()),
                description: None,
                version: None,
                author: None,
                homepage: None,
                clrmamepro_header: None,
                entry_count: 1,
                rom_count: 1,
                parse_warnings: Vec::new(),
            },
            games: vec![DatGameEntry {
                name: "Super Game".into(),
                description: None,
                roms: vec![DatRomEntry {
                    name: "super.bin".into(),
                    size_bytes: Some(4096),
                    crc32: Some("abcdef01".into()),
                    md5: Some("d41d8cd98f00b204e9800998ecf8427e".into()),
                    sha1: Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".into()),
                    sha256: None,
                    status: None,
                    merge: None,
                    date: None,
                    loadflag: None,
                }],
                clone_of: None,
                sample_of: None,
                board: None,
                rebuild_to: None,
                year: None,
                manufacturer: None,
                source_file: None,
                comment: None,
                original_metadata: Default::default(),
                content_classification: Default::default(),
                unsupported_structure: false,
            }],
        };
        DatIndex::build(&dat)
    }

    #[test]
    fn exact_match_by_md5() {
        let index = make_index();
        let known = KnownFileEvidence::new("a/b/super.bin", "super.bin")
            .with_md5("d41d8cd98f00b204e9800998ecf8427e");
        let report = audit_files(&[known], &index);
        assert_eq!(report.entries.len(), 1);
        assert!(matches!(
            report.entries[0].verdict,
            AuditVerdict::Exact { .. }
        ));
        assert_eq!(report.summary.exact, 1);
    }

    #[test]
    fn probable_by_crc32_and_size() {
        let index = make_index();
        let known = KnownFileEvidence::new("a/b/super.bin", "super.bin")
            .with_crc32("abcdef01")
            .with_size(4096);
        let report = audit_files(&[known], &index);
        assert_eq!(report.entries.len(), 1);
        assert!(matches!(
            report.entries[0].verdict,
            AuditVerdict::Probable { .. }
        ));
    }

    #[test]
    fn not_in_dat() {
        let index = make_index();
        let known = KnownFileEvidence::new("a/b/unknown.bin", "unknown.bin")
            .with_md5("00000000000000000000000000000000");
        let report = audit_files(&[known], &index);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].verdict, AuditVerdict::NotInDat);
    }

    #[test]
    fn filename_only() {
        let index = make_index();
        let known = KnownFileEvidence::new("a/b/super.bin", "super.bin");
        let report = audit_files(&[known], &index);
        assert_eq!(report.entries.len(), 1);
        assert!(matches!(
            report.entries[0].verdict,
            AuditVerdict::FilenameOnly { .. }
        ));
    }

    #[test]
    fn no_usable_evidence() {
        let index = make_index();
        let known = KnownFileEvidence::new("a/b/nonexistent.bin", "nonexistent.bin");
        let report = audit_files(&[known], &index);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].verdict, AuditVerdict::NoUsableEvidence);
    }
}
