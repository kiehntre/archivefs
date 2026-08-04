//! Read-only audit using hashes already known to ArchiveFS.
//!
//! Every verdict is derived from a comparison between what the DAT file claims
//! and what ArchiveFS already knows. Nothing here hashes a local file: the
//! caller supplies known hashes and size, and the audit logic compares them
//! against the indexed DAT entries.
//!
//! # Verdict rules
//!
//! - **Exact**: SHA-256, SHA-1, or MD5 exact match against exactly one DAT entry.
//! - **ExactMultipleCandidates**: strong hash matches multiple DAT entries (collision).
//! - **Probable**: CRC32 plus exact size match (no stronger hash available).
//! - **FilenameOnly**: filename matches, but no hash evidence.
//! - **Ambiguous**: credible candidate exists, but conflicting evidence.
//! - **NotInDat**: usable known hash has no candidate in the DAT.
//! - **NoUsableEvidence**: no known hash to compare.

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
    /// Strong hash matches multiple DAT entries.
    ExactMultipleCandidates {
        algorithm: &'static str,
        count: usize,
        game_names: Vec<String>,
    },
    /// CRC32 plus exact size match.
    Probable { game_name: String, rom_name: String },
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
            Self::Probable { .. } => "Probable",
            Self::FilenameOnly { .. } => "Filename only",
            Self::Ambiguous { .. } => "Ambiguous",
            Self::NotInDat => "Not in DAT",
            Self::NoUsableEvidence => "No usable evidence",
        }
    }

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
    pub ambiguous: usize,
    pub not_in_dat: usize,
    pub no_evidence: usize,
}

/// Known hashes and metadata for a single local file.
///
/// The caller populates this from existing ArchiveFS data — no local
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

fn audit_one(known: &KnownFileEvidence, index: &DatIndex) -> AuditVerdict {
    // Try SHA-256 first (strongest).
    if let Some(ref sha256) = known.sha256 {
        let candidates = index.lookup_sha256(sha256);
        return handle_candidates(candidates, "SHA-256");
    }

    // Try SHA-1.
    if let Some(ref sha1) = known.sha1 {
        let candidates = index.lookup_sha1(sha1);
        return handle_candidates(candidates, "SHA-1");
    }

    // Try MD5.
    if let Some(ref md5) = known.md5 {
        let candidates = index.lookup_md5(md5);
        return handle_candidates(candidates, "MD5");
    }

    // Try CRC32 with exact size.
    if let Some(ref crc) = known.crc32 {
        if let Some(size) = known.size_bytes {
            let candidates = index.lookup_crc32(crc);
            let size_matched: Vec<_> = candidates
                .iter()
                .filter(|r| r.size_bytes == Some(size))
                .collect();
            match size_matched.len() {
                1 => {
                    return AuditVerdict::Probable {
                        game_name: size_matched[0].game_name.clone(),
                        rom_name: size_matched[0].rom_name.clone(),
                    };
                }
                0 => {
                    // CRC32 matched but size didn't — ambiguous.
                    if !candidates.is_empty() {
                        return AuditVerdict::Ambiguous {
                            detail: format!(
                                "CRC32 {crc} matches {} DAT entry(s), but size {size} disagrees",
                                candidates.len()
                            ),
                        };
                    }
                }
                _ => {
                    return AuditVerdict::ExactMultipleCandidates {
                        algorithm: "CRC32+size",
                        count: size_matched.len(),
                        game_names: size_matched.iter().map(|r| r.game_name.clone()).collect(),
                    };
                }
            }
        } else {
            // CRC32 without size — less strong, but still evidence.
            let candidates = index.lookup_crc32(crc);
            match candidates.len() {
                0 => return AuditVerdict::NotInDat,
                1 => {
                    return AuditVerdict::Probable {
                        game_name: candidates[0].game_name.clone(),
                        rom_name: candidates[0].rom_name.clone(),
                    };
                }
                _ => {
                    return AuditVerdict::ExactMultipleCandidates {
                        algorithm: "CRC32",
                        count: candidates.len(),
                        game_names: candidates.iter().map(|r| r.game_name.clone()).collect(),
                    };
                }
            }
        }
    }

    // Try filename as last resort.
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
                }],
                clone_of: None,
                sample_of: None,
                board: None,
                rebuild_to: None,
                year: None,
                manufacturer: None,
                source_file: None,
                comment: None,
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
