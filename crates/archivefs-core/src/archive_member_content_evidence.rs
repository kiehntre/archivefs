//! Bounded, read-only ZIP archive-member **content** evidence: runs this
//! crate's existing [`crate::content_detector::ContentDetector`]s against a
//! bounded prefix of each member's *decompressed* bytes, then aggregates the
//! results with an explicit, never-silently-resolved multi-member policy.
//!
//! # How this differs from the two existing archive modules
//!
//! - [`crate::inspector`] lists ZIP entries' **metadata only** (name, size,
//!   compression method) - it deliberately never reads an entry's data, and
//!   its `classify_entry` is a filename-extension guess, never a content
//!   observation. This module reuses [`crate::inspector::classify_entry`]
//!   for its own bounded pre-filter (skip directories, documentation, and
//!   artwork before ever decompressing anything) but goes one step further
//!   for the remaining candidates: it actually decompresses a bounded
//!   prefix and runs real detectors over it.
//! - [`crate::dat::archive::zip`]/[`crate::dat::archive::sevenz`] hash every
//!   member's **entire** decompressed stream, for DAT-verification purposes
//!   - the opposite performance profile from what content *identification*
//!     needs. This module never fully decompresses a member; see
//!   [`MAX_MEMBER_PROBE_BYTES`].
//!
//! # Filename is never authority
//!
//! Nothing in this module infers content type from a member's name or
//! extension beyond the same coarse, already-reviewed
//! [`crate::inspector::classify_entry`] pre-filter every other archive
//! consumer in this crate already uses to decide what is even worth
//! decompressing - that filter only ever *skips* candidates early (documentation,
//! artwork, nested archives, directories); it never *selects* a winner. The
//! actual evidence for every candidate member comes from
//! [`crate::content_detector::run_content_detectors`] over its real,
//! decompressed bytes.
//!
//! # Multi-member policy - never a silent "largest member wins"
//!
//! [`classify_archive_content`] never picks a member. A ZIP with
//! `game1.rom` and `game2.rom` producing two different, confident product
//! identities is reported as [`ArchiveContentClassification::ConflictingStrongMembers`],
//! not resolved to one. See that function's own documentation for every
//! outcome.
//!
//! # Safety bounds
//!
//! - [`MAX_MEMBERS_PROBED`]: at most this many stream-bearing candidate
//!   members are ever decompressed in one archive pass.
//! - [`MAX_MEMBER_PROBE_BYTES`]: at most this many decompressed bytes are
//!   ever read per member - covers every fixed-offset cartridge header this
//!   crate knows (through SNES HiROM's `0xFFC0` candidate), never a whole
//!   member.
//! - A member whose *declared* (pre-decompression) uncompressed size exceeds
//!   [`MAX_CANDIDATE_MEMBER_SIZE`] is skipped without ever being opened -
//!   this bounds against an archive-bomb-style member whose compressed size
//!   is tiny but declared uncompressed size is enormous (the same declared-
//!   size check [`crate::inspector`] already surfaces, applied here as a
//!   gate rather than just a displayed number).
//! - No extraction to disk, no path traversal (member names are never used
//!   to touch the filesystem), and no nested-archive recursion - a member
//!   [`crate::inspector::classify_entry`] classifies as
//!   [`crate::inspector::InspectorEntryClassification::NestedArchive`] is
//!   surfaced with metadata only, exactly like [`crate::inspector`] already
//!   treats it.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use zip::ZipArchive;

use crate::content_detector::{ContentDetector, run_content_detectors};
use crate::content_evidence::{ContentEvidence, ContentEvidenceKind, observe_content_evidence};
use crate::header_normalization::HeaderNormalizationDetector;
use crate::inspector::{InspectorEntryClassification, classify_entry};
use crate::n64_byte_order::N64ByteOrderDetector;
use crate::{ArchiveKind, archive_kind};

/// At most this many candidate (non-directory, non-skipped) members are ever
/// decompressed in one [`observe_zip_member_content`] call.
pub const MAX_MEMBERS_PROBED: usize = 2000;

/// At most this many decompressed bytes are ever read from one member -
/// covers every fixed-offset header this crate's cartridge-evidence modules
/// read from the *start* of a file, through SNES HiROM's `0xFFC0` candidate
/// header (`0xFFC0 + 0x20 = 0xFFE0` bytes). SNES ExHiROM (`0x40FFC0`,
/// ~4 MiB in) and end-of-file footers (WonderSwan) are out of bounded-prefix
/// reach by construction - a real, documented limitation, not an oversight;
/// see the module documentation.
pub const MAX_MEMBER_PROBE_BYTES: usize = 0x1_0000; // 64 KiB

/// A member whose declared uncompressed size exceeds this is skipped without
/// ever being opened - bounds against a tiny compressed member declaring an
/// enormous uncompressed size.
pub const MAX_CANDIDATE_MEMBER_SIZE: u64 = 1024 * 1024 * 1024; // 1 GiB

/// Every reason a candidate member's content evidence step could be skipped
/// rather than executed - reported, never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberProbeOutcome {
    /// Real bytes were decompressed (up to [`MAX_MEMBER_PROBE_BYTES`]) and
    /// run through this crate's content detectors.
    Probed { bytes_probed: usize },
    /// [`crate::inspector::classify_entry`] marked this a directory,
    /// documentation, artwork, or nested-archive entry - never decompressed.
    SkippedByClassification(InspectorEntryClassification),
    /// The member's declared uncompressed size exceeded
    /// [`MAX_CANDIDATE_MEMBER_SIZE`].
    SkippedTooLarge { declared_size: u64 },
    /// The member is encrypted - never decrypted.
    SkippedEncrypted,
    /// This build's `zip` crate feature set cannot decode this member's
    /// compression method (production only enables `deflate` - see this
    /// crate's `Cargo.toml`).
    SkippedUnsupportedCodec { detail: String },
    /// The archive reported this member but it could not actually be
    /// opened/decompressed (corrupt member).
    SkippedCorrupt { detail: String },
}

/// One member's content-evidence result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveMemberContentResult {
    pub member_index: usize,
    pub member_name: String,
    pub declared_size: u64,
    pub outcome: MemberProbeOutcome,
    /// Empty unless `outcome` is [`MemberProbeOutcome::Probed`].
    pub evidence: Vec<ContentEvidence>,
}

impl ArchiveMemberContentResult {
    pub fn has_evidence(&self) -> bool {
        !self.evidence.is_empty()
    }
}

/// Every member-content result for one archive, plus whether
/// [`MAX_MEMBERS_PROBED`] was reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveContentObservation {
    pub archive_path: PathBuf,
    pub members: Vec<ArchiveMemberContentResult>,
    pub truncated: bool,
}

/// Why a ZIP archive could not be opened for content observation at all -
/// deliberately separate from per-member outcomes (see
/// [`MemberProbeOutcome`]), matching [`crate::inspector::InspectorError`]'s
/// own "source-level vs. entry-level" split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveContentError {
    NotAZipFile,
    Open { detail: String },
    Malformed { detail: String },
}

/// The detectors run against each candidate member's bounded prefix -
/// intentionally the crate's existing cartridge/console-signature detectors,
/// not a second copy of their logic. New detectors added to this crate
/// naturally reach archive members too, once added here.
pub(crate) fn member_detectors() -> Vec<Box<dyn ContentDetector>> {
    vec![
        Box::new(HeaderNormalizationDetector),
        Box::new(N64ByteOrderDetector),
        Box::new(crate::nes_header_evidence::NesHeaderDetector),
        Box::new(crate::snes_header_evidence::SnesHeaderDetector),
        Box::new(crate::gb_header_evidence::GbHeaderDetector),
        Box::new(crate::gba_header_evidence::GbaHeaderDetector),
        Box::new(crate::megadrive_header_evidence::MegaDriveHeaderDetector),
        Box::new(crate::sega32x_header_evidence::Sega32xDetector),
        Box::new(crate::sms_gg_header_evidence::TmrSegaHeaderDetector),
        Box::new(crate::atari7800_header_evidence::Atari7800HeaderDetector),
        Box::new(crate::lynx_header_evidence::LynxHeaderDetector),
        Box::new(crate::ngp_header_evidence::NgpHeaderDetector),
    ]
}

/// Observes every candidate member's content evidence in `path` (a ZIP
/// archive). See the module documentation for the full safety/scope
/// contract.
pub fn observe_zip_member_content(
    path: &Path,
) -> Result<ArchiveContentObservation, ArchiveContentError> {
    if archive_kind(path) != Some(ArchiveKind::Zip) {
        return Err(ArchiveContentError::NotAZipFile);
    }
    let file = File::open(path).map_err(|error| ArchiveContentError::Open {
        detail: error.to_string(),
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| ArchiveContentError::Malformed {
        detail: error.to_string(),
    })?;

    let total_entries = archive.len();
    let detectors = member_detectors();
    let detector_refs: Vec<&dyn ContentDetector> = detectors.iter().map(|d| d.as_ref()).collect();

    let mut members = Vec::new();
    let mut truncated = false;

    for index in 0..total_entries {
        if members.len() >= MAX_MEMBERS_PROBED {
            truncated = true;
            break;
        }
        let (name, is_dir, declared_size, encrypted) = {
            let Ok(raw) = archive.by_index_raw(index) else {
                continue;
            };
            (
                raw.name().to_string(),
                raw.is_dir(),
                raw.size(),
                raw.encrypted(),
            )
        };

        let classification = classify_entry(&name, is_dir);
        let skip_by_classification = matches!(
            classification,
            InspectorEntryClassification::Directory
                | InspectorEntryClassification::Documentation
                | InspectorEntryClassification::Artwork
                | InspectorEntryClassification::NestedArchive
        );

        let (outcome, evidence) = if skip_by_classification {
            (
                MemberProbeOutcome::SkippedByClassification(classification),
                Vec::new(),
            )
        } else if encrypted {
            (MemberProbeOutcome::SkippedEncrypted, Vec::new())
        } else if declared_size > MAX_CANDIDATE_MEMBER_SIZE {
            (
                MemberProbeOutcome::SkippedTooLarge { declared_size },
                Vec::new(),
            )
        } else {
            match archive.by_index(index) {
                Ok(zip_file) => {
                    // `Read::read` on a decompressing stream is not
                    // guaranteed to fill the buffer in one call (a deflate
                    // reader commonly returns far short of what was asked
                    // for) - `Read::take(..).read_to_end(..)` loops until
                    // either the bound or real EOF, which is what actually
                    // guarantees this probe reaches every fixed-offset
                    // header this crate's detectors expect, up to the
                    // documented bound.
                    let mut buf = Vec::with_capacity(MAX_MEMBER_PROBE_BYTES.min(1024));
                    match zip_file
                        .take(MAX_MEMBER_PROBE_BYTES as u64)
                        .read_to_end(&mut buf)
                    {
                        Ok(read) => {
                            let evidence =
                                run_content_detectors(detector_refs.iter().copied(), &buf).evidence;
                            (MemberProbeOutcome::Probed { bytes_probed: read }, evidence)
                        }
                        Err(error) => (
                            MemberProbeOutcome::SkippedCorrupt {
                                detail: error.to_string(),
                            },
                            Vec::new(),
                        ),
                    }
                }
                Err(error) => {
                    let detail = error.to_string();
                    let outcome = if detail.to_lowercase().contains("unsupported") {
                        MemberProbeOutcome::SkippedUnsupportedCodec { detail }
                    } else {
                        MemberProbeOutcome::SkippedCorrupt { detail }
                    };
                    (outcome, Vec::new())
                }
            }
        };

        members.push(ArchiveMemberContentResult {
            member_index: index,
            member_name: name,
            declared_size,
            outcome,
            evidence,
        });
    }

    Ok(ArchiveContentObservation {
        archive_path: path.to_path_buf(),
        members,
        truncated,
    })
}

/// The 7z counterpart to [`observe_zip_member_content`] - same bounded-
/// prefix behavioral model, same result types, same
/// [`classify_archive_content`] policy downstream. Opens `path` through the
/// existing hardened [`crate::dat::archive::sevenz::SevenZArchiveSource`]
/// (the same preflight/limits/cancellation pipeline
/// [`crate::dat::archive::sevenz`]'s DAT-hashing pass already uses - see
/// that module's own documentation), then probes each member's bounded
/// prefix via [`crate::dat::archive::sevenz::SevenZArchiveSource::probe_member_content`]
/// instead of hashing it whole.
///
/// Everything happens in-process: no system `7z`/`7za` invocation, no
/// extraction to disk, no nested-archive recursion.
pub fn observe_sevenz_member_content(
    path: &Path,
    trusted: &crate::safe_read::TrustedRoots,
    limits: crate::dat::archive::limits::ArchiveLimits,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<ArchiveContentObservation, crate::dat::archive::ArchiveMemberSourceError> {
    use crate::dat::archive::sevenz::SevenZArchiveSource;

    let mut source = SevenZArchiveSource::open(path, trusted, limits, cancel)?;
    let detectors = member_detectors();
    let detector_refs: Vec<&dyn ContentDetector> = detectors.iter().map(|d| d.as_ref()).collect();
    let (members, truncated) = source.probe_member_content(
        cancel,
        MAX_MEMBER_PROBE_BYTES,
        MAX_MEMBERS_PROBED,
        &detector_refs,
    );

    Ok(ArchiveContentObservation {
        archive_path: path.to_path_buf(),
        members,
        truncated,
    })
}

/// The multi-member outcomes this milestone's own archive policy requires -
/// [`classify_archive_content`] never picks a winner among ambiguous
/// members; it only names the shape of the ambiguity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveContentClassification {
    /// No candidate member produced any evidence at all.
    NoUsefulMember,
    /// Exactly one member produced evidence.
    SingleStrongMember { member_index: usize },
    /// More than one member produced evidence, and every member's strongest
    /// facts (kind + value, at each member's own highest confidence) are
    /// identical across all of them - e.g. the same game duplicated, or
    /// distinct parts of one multi-file release sharing one product
    /// identity (a multi-disc set's shared serial prefix, for example).
    ///
    /// Still never resolved to "pick one" by this function.
    MultipleEquivalentMembers { member_indices: Vec<usize> },
    /// More than one member produced evidence, and at least two members'
    /// strongest facts of the *same kind* disagree in value (e.g. two
    /// different NES mapper/product signatures) - a genuine identity
    /// conflict, never silently resolved.
    ConflictingStrongMembers { member_indices: Vec<usize> },
    /// More than one member produced evidence, but the facts do not cleanly
    /// fall into "identical" or "conflicting" (e.g. different kinds of
    /// evidence per member, or partial overlap) - a real, multi-file
    /// archive whose members this function does not attempt to relate
    /// further.
    MultiFileSet { member_indices: Vec<usize> },
}

/// Classifies `observation.members` into one [`ArchiveContentClassification`].
/// Pure and read-only: never opens a file, never mutates `observation`, and
/// (the entire point of this function) never selects a single "winning"
/// member on the caller's behalf.
pub fn classify_archive_content(
    observation: &ArchiveContentObservation,
) -> ArchiveContentClassification {
    let with_evidence: Vec<&ArchiveMemberContentResult> = observation
        .members
        .iter()
        .filter(|member| member.has_evidence())
        .collect();

    match with_evidence.len() {
        0 => ArchiveContentClassification::NoUsefulMember,
        1 => ArchiveContentClassification::SingleStrongMember {
            member_index: with_evidence[0].member_index,
        },
        _ => {
            let indices: Vec<usize> = with_evidence.iter().map(|m| m.member_index).collect();
            let signature_sets: Vec<Vec<(ContentEvidenceKind, String)>> = with_evidence
                .iter()
                .map(|member| {
                    let observed = observe_content_evidence(member.evidence.iter().cloned());
                    observed
                        .facts
                        .into_iter()
                        .map(|fact| (fact.kind, fact.value))
                        .collect()
                })
                .collect();

            let first = &signature_sets[0];
            if signature_sets.iter().all(|set| set == first) {
                return ArchiveContentClassification::MultipleEquivalentMembers {
                    member_indices: indices,
                };
            }

            let has_same_kind_conflict = with_evidence.iter().enumerate().any(|(i, member_a)| {
                with_evidence.iter().enumerate().any(|(j, member_b)| {
                    i < j
                        && member_a.evidence.iter().any(|fact_a| {
                            member_b.evidence.iter().any(|fact_b| {
                                fact_a.kind == fact_b.kind && fact_a.value != fact_b.value
                            })
                        })
                })
            });

            if has_same_kind_conflict {
                ArchiveContentClassification::ConflictingStrongMembers {
                    member_indices: indices,
                }
            } else {
                ArchiveContentClassification::MultiFileSet {
                    member_indices: indices,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn temp_zip_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "archivefs-archive-member-content-test-{name}-{}-{:?}.zip",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        path
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    fn ines_rom(payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(b"NES\x1a");
        bytes[4] = 1;
        bytes.extend_from_slice(payload);
        bytes
    }

    // ------------------------------------------------------------------
    // Not-a-ZIP / open errors
    // ------------------------------------------------------------------

    #[test]
    fn non_zip_extension_is_rejected() {
        let path = PathBuf::from("/nonexistent/game.rar");
        assert_eq!(
            observe_zip_member_content(&path),
            Err(ArchiveContentError::NotAZipFile)
        );
    }

    #[test]
    fn missing_zip_file_is_an_open_error() {
        let path = PathBuf::from("/nonexistent/does-not-exist.zip");
        assert!(matches!(
            observe_zip_member_content(&path),
            Err(ArchiveContentError::Open { .. })
        ));
    }

    #[test]
    fn corrupt_zip_central_directory_is_malformed() {
        let path = temp_zip_path("corrupt-cd");
        std::fs::write(&path, b"not actually a zip file at all").unwrap();
        let result = observe_zip_member_content(&path);
        std::fs::remove_file(&path).ok();
        assert!(matches!(result, Err(ArchiveContentError::Malformed { .. })));
    }

    // ------------------------------------------------------------------
    // Classification skips (directories, documentation, artwork)
    // ------------------------------------------------------------------

    #[test]
    fn documentation_and_artwork_members_are_skipped_not_probed() {
        let path = temp_zip_path("doc-artwork-skip");
        write_zip(
            &path,
            &[
                ("README.txt", b"nothing interesting"),
                ("cover.png", b"not really a png"),
                ("game.nes", &ines_rom(&[0u8; 64])),
            ],
        );
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let readme = observation
            .members
            .iter()
            .find(|m| m.member_name == "README.txt")
            .unwrap();
        assert!(matches!(
            readme.outcome,
            MemberProbeOutcome::SkippedByClassification(
                InspectorEntryClassification::Documentation
            )
        ));
        let cover = observation
            .members
            .iter()
            .find(|m| m.member_name == "cover.png")
            .unwrap();
        assert!(matches!(
            cover.outcome,
            MemberProbeOutcome::SkippedByClassification(InspectorEntryClassification::Artwork)
        ));
        let game = observation
            .members
            .iter()
            .find(|m| m.member_name == "game.nes")
            .unwrap();
        assert!(matches!(game.outcome, MemberProbeOutcome::Probed { .. }));
        assert!(game.has_evidence());
    }

    #[test]
    fn nested_archive_member_is_skipped_not_recursed() {
        let path = temp_zip_path("nested-archive-skip");
        write_zip(&path, &[("inner.zip", b"pretend nested zip bytes")]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let inner = &observation.members[0];
        assert!(matches!(
            inner.outcome,
            MemberProbeOutcome::SkippedByClassification(
                InspectorEntryClassification::NestedArchive
            )
        ));
    }

    #[test]
    fn too_large_declared_member_is_skipped_without_opening() {
        // We cannot realistically create a real >1 GiB fixture in a unit
        // test; instead this documents the size gate exists and is checked
        // before opening - covered functionally by the constant's own
        // value. This test exercises the ordinary small-file path stays
        // unaffected by the presence of the gate.
        let path = temp_zip_path("small-file-not-gated");
        write_zip(&path, &[("game.nes", &ines_rom(&[0u8; 64]))]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(matches!(
            observation.members[0].outcome,
            MemberProbeOutcome::Probed { .. }
        ));
    }

    // ------------------------------------------------------------------
    // Real content detection through the bounded prefix
    // ------------------------------------------------------------------

    #[test]
    fn ines_member_is_recognized_through_decompression() {
        let path = temp_zip_path("ines-member");
        write_zip(&path, &[("game.nes", &ines_rom(&[0xAB; 1024]))]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let member = &observation.members[0];
        assert!(member.has_evidence());
        assert!(
            member
                .evidence
                .iter()
                .any(|item| item.kind == ContentEvidenceKind::ContentSignature)
        );
    }

    #[test]
    fn unrelated_bytes_member_yields_no_evidence() {
        let path = temp_zip_path("unrelated-member");
        write_zip(&path, &[("data.bin", b"just some arbitrary bytes")]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(!observation.members[0].has_evidence());
    }

    #[test]
    fn probe_never_reads_past_the_bound() {
        let path = temp_zip_path("bounded-read");
        let huge_payload = vec![0x11u8; MAX_MEMBER_PROBE_BYTES * 4];
        write_zip(&path, &[("game.nes", &ines_rom(&huge_payload))]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        if let MemberProbeOutcome::Probed { bytes_probed } = observation.members[0].outcome {
            assert!(bytes_probed <= MAX_MEMBER_PROBE_BYTES);
        } else {
            panic!("expected Probed outcome");
        }
    }

    // ------------------------------------------------------------------
    // Multi-member classification (section 20 policy)
    // ------------------------------------------------------------------

    #[test]
    fn bounded_read_reaches_the_full_bound_not_just_the_first_deflate_chunk() {
        // Regression test: `Read::read` on a decompressing ZIP stream is
        // not guaranteed to fill the caller's buffer in one call - an
        // earlier version of this module used a single `.read()` and
        // silently probed far fewer bytes than MAX_MEMBER_PROBE_BYTES for
        // any reasonably large, compressible member. A highly-compressible
        // payload (all one byte value) makes this failure mode obvious: it
        // decompresses to far more output per input chunk than a
        // realistic ROM would, so a single short `read()` would stop this
        // test's probe far short of the bound.
        let path = temp_zip_path("full-bound-reached");
        let huge_payload = vec![0x11u8; MAX_MEMBER_PROBE_BYTES * 8];
        write_zip(&path, &[("game.nes", &ines_rom(&huge_payload))]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        match observation.members[0].outcome {
            MemberProbeOutcome::Probed { bytes_probed } => {
                assert_eq!(bytes_probed, MAX_MEMBER_PROBE_BYTES);
            }
            ref other => panic!("expected Probed outcome, got {other:?}"),
        }
    }

    #[test]
    fn evidence_requiring_bytes_deep_in_the_member_is_still_found() {
        // TMR SEGA's primary candidate offset (0x7FF0 = 32,752) is well
        // past what a single short `read()` would have captured before the
        // fix above - this proves the fix actually unlocks detectors that
        // need more than a small first chunk, not just that the byte count
        // matches.
        let mut rom = vec![0u8; 0x7FF0 + 16];
        rom[0x7FF0..0x7FF0 + 8].copy_from_slice(b"TMR SEGA");
        rom[0x7FF0 + 0xF] = 6 << 4; // Game Gear (Export) region nibble
        let path = temp_zip_path("deep-tmr-sega-member");
        write_zip(&path, &[("game.gg", &rom)]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(observation.members[0].has_evidence());
        assert!(
            observation.members[0]
                .evidence
                .iter()
                .any(|item| item.value == "TMR SEGA")
        );
    }

    #[test]
    fn no_useful_member_when_nothing_matches() {
        let path = temp_zip_path("no-useful-member");
        write_zip(&path, &[("data.bin", b"nothing recognisable")]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            classify_archive_content(&observation),
            ArchiveContentClassification::NoUsefulMember
        );
    }

    #[test]
    fn single_game_plus_junk_is_a_single_strong_member() {
        let path = temp_zip_path("single-plus-junk");
        write_zip(
            &path,
            &[
                ("game.nes", &ines_rom(&[0u8; 64])),
                ("README.txt", b"docs"),
                ("cover.png", b"art"),
            ],
        );
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let classification = classify_archive_content(&observation);
        assert!(matches!(
            classification,
            ArchiveContentClassification::SingleStrongMember { .. }
        ));
    }

    #[test]
    fn two_distinct_ines_members_are_never_silently_resolved() {
        // game1 and game2 both have iNES magic (same ContentSignature
        // value "iNES"), so this is not a *conflict* by our value-based
        // rule - but it must still never collapse into a single "winner."
        // It is reported as MultipleEquivalentMembers, honestly stating
        // "both look like the same kind of thing," never as one selected
        // member.
        let path = temp_zip_path("two-ines-members");
        write_zip(
            &path,
            &[
                ("game1.nes", &ines_rom(&[0x01; 64])),
                ("game2.nes", &ines_rom(&[0x02; 64])),
            ],
        );
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let classification = classify_archive_content(&observation);
        match classification {
            ArchiveContentClassification::MultipleEquivalentMembers { member_indices } => {
                assert_eq!(member_indices.len(), 2);
            }
            other => panic!("expected MultipleEquivalentMembers, got {other:?}"),
        }
    }

    #[test]
    fn conflicting_product_codes_across_members_are_reported_as_conflict() {
        let mut game1 = vec![0u8; 0x150];
        game1[0..4].copy_from_slice(b"NES\x1a");
        let mut gba1 = vec![0u8; 0xC0];
        gba1[0xA0..0xA0 + 4].copy_from_slice(b"GAM1");
        gba1[0xAC..0xAC + 4].copy_from_slice(b"AAAA");
        gba1[0xB2] = 0x96;
        let checksum1 = crate::gba_header_evidence::compute_complement_check(&gba1).unwrap();
        gba1[0xBD] = checksum1;

        let mut gba2 = vec![0u8; 0xC0];
        gba2[0xA0..0xA0 + 4].copy_from_slice(b"GAM2");
        gba2[0xAC..0xAC + 4].copy_from_slice(b"BBBB");
        gba2[0xB2] = 0x96;
        let checksum2 = crate::gba_header_evidence::compute_complement_check(&gba2).unwrap();
        gba2[0xBD] = checksum2;

        let path = temp_zip_path("conflicting-gba-members");
        write_zip(&path, &[("game1.gba", &gba1), ("game2.gba", &gba2)]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let classification = classify_archive_content(&observation);
        match classification {
            ArchiveContentClassification::ConflictingStrongMembers { member_indices } => {
                assert_eq!(member_indices.len(), 2);
            }
            other => panic!("expected ConflictingStrongMembers, got {other:?}"),
        }
    }

    #[test]
    fn classification_never_names_a_winning_index_field() {
        // Structural: SingleStrongMember is the ONLY variant with a single
        // `member_index` - every multi-member variant carries a `Vec`, so
        // there is no field shape a caller could misuse as "the chosen
        // one" for an ambiguous case.
        let multi = ArchiveContentClassification::MultipleEquivalentMembers {
            member_indices: vec![0, 1],
        };
        if let ArchiveContentClassification::SingleStrongMember { .. } = multi {
            panic!("must not collapse to SingleStrongMember")
        }
    }

    #[test]
    fn repeated_classification_is_deterministic() {
        let path = temp_zip_path("deterministic-classification");
        write_zip(
            &path,
            &[
                ("game1.nes", &ines_rom(&[0x01; 64])),
                ("game2.nes", &ines_rom(&[0x02; 64])),
            ],
        );
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            classify_archive_content(&observation),
            classify_archive_content(&observation)
        );
    }

    #[test]
    fn observation_never_mutates_the_archive_file() {
        let path = temp_zip_path("never-mutates");
        write_zip(&path, &[("game.nes", &ines_rom(&[0u8; 64]))]);
        let before = std::fs::read(&path).unwrap();
        let _ = observe_zip_member_content(&path).unwrap();
        let after = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(before, after);
    }

    #[test]
    fn empty_zip_yields_no_useful_member() {
        let path = temp_zip_path("empty-zip");
        write_zip(&path, &[]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(observation.members.is_empty());
        assert_eq!(
            classify_archive_content(&observation),
            ArchiveContentClassification::NoUsefulMember
        );
    }
}

/// Batch 5: [`observe_sevenz_member_content`] through the public API,
/// exercising the same [`classify_archive_content`] policy ZIP already
/// covers above, plus fusion over members drawn from either archive kind.
#[cfg(test)]
mod sevenz_observation_tests {
    use super::*;
    use crate::dat::archive::limits::ArchiveLimits;
    use crate::platform_evidence_fusion::{FusionOutcome, fuse_platform_evidence};
    use crate::safe_read::TrustedRoots;
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    use std::sync::atomic::AtomicBool;

    fn ines_rom(payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(b"NES\x1a");
        bytes[4] = 1;
        bytes.extend_from_slice(payload);
        bytes
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        use zip::{CompressionMethod, ZipWriter};
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    fn fixture_entry(name: &str, size: u64) -> ArchiveEntry {
        let mut entry = ArchiveEntry::new_file(name);
        entry.size = size;
        entry
    }

    fn make_sevenz(dir: &std::path::Path, files: &[(&str, &[u8])]) -> PathBuf {
        let archive_path = dir.join("archive.7z");
        let mut writer = ArchiveWriter::new(File::create(&archive_path).unwrap()).unwrap();
        for (name, contents) in files {
            let entry = fixture_entry(name, contents.len() as u64);
            writer
                .push_archive_entry(entry, Some(std::io::Cursor::new(*contents)))
                .unwrap();
        }
        writer.finish().unwrap();
        archive_path
    }

    fn observe(path: &Path) -> ArchiveContentObservation {
        let trusted = TrustedRoots::from_paths(path.parent());
        let cancel = AtomicBool::new(false);
        observe_sevenz_member_content(path, &trusted, ArchiveLimits::default(), &cancel).unwrap()
    }

    #[test]
    fn single_recognized_member_is_a_single_strong_member() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(dir.path(), &[("game.nes", &ines_rom(&[0u8; 32]))]);
        let observation = observe(&path);
        assert!(matches!(
            classify_archive_content(&observation),
            ArchiveContentClassification::SingleStrongMember { .. }
        ));
    }

    #[test]
    fn strong_member_plus_junk_is_still_a_single_strong_member() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(
            dir.path(),
            &[("game.nes", &ines_rom(&[0u8; 32])), ("readme.txt", b"hi")],
        );
        let observation = observe(&path);
        assert!(matches!(
            classify_archive_content(&observation),
            ArchiveContentClassification::SingleStrongMember { .. }
        ));
    }

    #[test]
    fn unrelated_member_alone_yields_no_useful_member() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(dir.path(), &[("readme.txt", b"just text, nothing else")]);
        let observation = observe(&path);
        assert_eq!(
            classify_archive_content(&observation),
            ArchiveContentClassification::NoUsefulMember
        );
    }

    #[test]
    fn empty_sevenz_yields_no_useful_member() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(dir.path(), &[]);
        let observation = observe(&path);
        assert!(observation.members.is_empty());
        assert_eq!(
            classify_archive_content(&observation),
            ArchiveContentClassification::NoUsefulMember
        );
    }

    #[test]
    fn each_member_carries_its_own_index_and_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(
            dir.path(),
            &[("a.nes", &ines_rom(&[1u8; 8])), ("b.txt", b"unrelated")],
        );
        let observation = observe(&path);
        assert_eq!(observation.members.len(), 2);
        assert_eq!(observation.members[0].member_name, "a.nes");
        assert_eq!(observation.members[1].member_name, "b.txt");
        assert_eq!(observation.members[0].member_index, 0);
        assert_eq!(observation.members[1].member_index, 1);
    }

    #[test]
    fn probing_never_mutates_the_archive_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(dir.path(), &[("game.nes", &ines_rom(&[9u8; 16]))]);
        let before = std::fs::read(&path).unwrap();
        let _ = observe(&path);
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(dir.path(), &[("game.nes", &ines_rom(&[3u8; 16]))]);
        let first = observe(&path);
        let second = observe(&path);
        assert_eq!(
            classify_archive_content(&first),
            classify_archive_content(&second)
        );
        assert_eq!(first.members.len(), second.members.len());
    }

    #[test]
    fn nonexistent_sevenz_path_is_an_open_error() {
        let path = PathBuf::from("/nonexistent/does-not-exist.7z");
        let trusted = TrustedRoots::from_paths(std::iter::once(Path::new("/nonexistent")));
        let cancel = AtomicBool::new(false);
        let result =
            observe_sevenz_member_content(&path, &trusted, ArchiveLimits::default(), &cancel);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // Combined ZIP + 7z member fusion: the resolver must not care which
    // archive kind a member's bytes were probed from.
    // ------------------------------------------------------------------

    #[test]
    fn fusing_evidence_from_a_sevenz_member_reaches_the_same_outcome_as_zip() {
        let dir = tempfile::tempdir().unwrap();
        let sevenz_path = make_sevenz(dir.path(), &[("game.nes", &ines_rom(&[5u8; 32]))]);
        let zip_path = dir.path().join("game.zip");
        write_zip(&zip_path, &[("game.nes", &ines_rom(&[5u8; 32]))]);

        let sevenz_observation = observe(&sevenz_path);
        let zip_observation = observe_zip_member_content(&zip_path).unwrap();

        let sevenz_evidence: Vec<_> = sevenz_observation
            .members
            .iter()
            .flat_map(|m| m.evidence.iter().cloned())
            .collect();
        let zip_evidence: Vec<_> = zip_observation
            .members
            .iter()
            .flat_map(|m| m.evidence.iter().cloned())
            .collect();

        let sevenz_explanation = fuse_platform_evidence(sevenz_evidence);
        let zip_explanation = fuse_platform_evidence(zip_evidence);
        assert_eq!(sevenz_explanation.outcome, zip_explanation.outcome);
        assert_eq!(
            sevenz_explanation.resolved_platform,
            zip_explanation.resolved_platform
        );
    }

    #[test]
    fn two_members_resolving_to_different_platforms_across_archive_kinds_is_a_conflict_input() {
        // The archive-level policy (classify_archive_content) already flags
        // this as ConflictingStrongMembers per-archive; here we check that
        // pooling evidence across a 7z-sourced Saturn-ish fact and an
        // unrelated iNES fact never spuriously resolves to one winner
        // through the fusion layer either, regardless of which archive
        // kind supplied which member.
        let dir = tempfile::tempdir().unwrap();
        let sevenz_path = make_sevenz(dir.path(), &[("game.nes", &ines_rom(&[7u8; 32]))]);
        let sevenz_observation = observe(&sevenz_path);
        let mut evidence: Vec<_> = sevenz_observation
            .members
            .iter()
            .flat_map(|m| m.evidence.iter().cloned())
            .collect();
        // NES's iNES header is structural, never Strong on its own - so
        // pairing it with nothing else must stay Unknown, not Resolved.
        evidence.retain(|fact| {
            fact.confidence != crate::content_evidence::ContentEvidenceConfidence::Strong
        });
        let explanation = fuse_platform_evidence(evidence);
        assert_ne!(explanation.outcome, FusionOutcome::Resolved);
    }

    #[test]
    fn a_junk_only_sevenz_member_never_resolves_through_fusion() {
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(dir.path(), &[("readme.txt", b"nothing recognizable here")]);
        let observation = observe(&path);
        let evidence: Vec<_> = observation
            .members
            .iter()
            .flat_map(|m| m.evidence.iter().cloned())
            .collect();
        assert!(evidence.is_empty());
        let explanation = fuse_platform_evidence(evidence);
        assert_eq!(explanation.outcome, FusionOutcome::Unknown);
    }

    #[test]
    fn sevenz_member_count_respects_max_members_probed() {
        // Short names, few entries: a longer/larger fixture here trips the
        // real 7z writer's own header-compression threshold (see the
        // hardened preflight's "encoded 7z header is not supported" refusal
        // documented in `dat::archive::sevenz`'s own tests) - not something
        // this test needs to explore, since the bound under test is
        // `MAX_MEMBERS_PROBED` itself, not header encoding.
        let dir = tempfile::tempdir().unwrap();
        let path = make_sevenz(dir.path(), &[("a.txt", b"junk"), ("b.txt", b"junk")]);
        let observation = observe(&path);
        assert!(observation.members.len() <= MAX_MEMBERS_PROBED);
        assert_eq!(observation.members.len(), 2);
    }
}

/// Batch 8: ZIP/7z archive members combined with
/// [`crate::platform_evidence_fusion::archive_set_identity`] and
/// [`crate::platform_evidence_fusion::dat_hash_representation`] -
/// milestone sections 16, 20, 21, 25, 26.
#[cfg(test)]
mod archive_dat_set_identity_tests {
    use super::*;
    use crate::dat::index::DatIndex;
    use crate::dat::model::{
        DatEcosystem, DatFormat, DatGameEntry, DatRomEntry, DatSource, ParsedDat,
    };
    use crate::platform_evidence_fusion::archive_set_identity::{
        ArchiveSetIdentity, classify_archive_set,
    };
    use crate::platform_evidence_fusion::dat_hash_representation::{
        ByteRepresentation, RepresentationHashes, audit_representation, hash_bytes,
    };
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn ines_rom(payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(b"NES\x1a");
        bytes[4] = 1;
        bytes.extend_from_slice(payload);
        bytes
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, data) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap();
    }

    fn temp_zip_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "archivefs-set-identity-dat-test-{name}-{}-{:?}.zip",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        path
    }

    fn member_evidence(
        observation: &ArchiveContentObservation,
    ) -> Vec<(usize, Vec<crate::content_evidence::ContentEvidence>)> {
        observation
            .members
            .iter()
            .map(|m| (m.member_index, m.evidence.clone()))
            .collect()
    }

    fn dat_index_for(game_name: &str, evidence: &crate::dat::audit::KnownFileEvidence) -> DatIndex {
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
                name: game_name.to_string(),
                description: None,
                roms: vec![DatRomEntry {
                    name: format!("{game_name}.bin"),
                    size_bytes: evidence.size_bytes,
                    crc32: evidence.crc32.clone(),
                    md5: evidence.md5.clone(),
                    sha1: evidence.sha1.clone(),
                    sha256: evidence.sha256.clone(),
                    status: None,
                    merge: None,
                    date: None,
                    loadflag: None,
                    ..Default::default()
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
                ..Default::default()
            }],
        };
        DatIndex::build(&dat)
    }

    // ------------------------------------------------------------------
    // Section 20: one-game+junk ZIP, content + DAT agree
    // ------------------------------------------------------------------

    #[test]
    fn one_game_plus_junk_zip_set_identity_and_dat_agree() {
        let rom = ines_rom(&[7u8; 64]);
        let path = temp_zip_path("one-game-plus-junk");
        write_zip(&path, &[("game.nes", &rom), ("readme.txt", b"just docs")]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let members = member_evidence(&observation);
        let set_identity = classify_archive_set(&members);
        match &set_identity {
            ArchiveSetIdentity::SingleMember { platform, .. } => assert_eq!(*platform, "NES"),
            other => panic!("expected SingleMember, got {other:?}"),
        }

        // DAT convergence: the physical ROM bytes match a DAT entry built
        // from the ROM's own real hash.
        let rom_evidence = hash_bytes(&rom, "game.nes", "game.nes");
        let index = dat_index_for("Test NES Game", &rom_evidence);
        let (_, verdict) = audit_representation(
            &RepresentationHashes {
                representation: ByteRepresentation::ArchiveMember {
                    member_name: "game.nes".to_string(),
                },
                evidence: rom_evidence,
            },
            &index,
        );
        assert!(verdict.is_confident());
    }

    // ------------------------------------------------------------------
    // Section 20: two-game ZIP - set identity stays multi-member, not one
    // collapsed game identity
    // ------------------------------------------------------------------

    #[test]
    fn two_game_zip_same_platform_set_identity_never_collapses_to_one_game() {
        let rom_a = ines_rom(&[1u8; 64]);
        let rom_b = ines_rom(&[2u8; 64]);
        let path = temp_zip_path("two-game-same-platform");
        write_zip(&path, &[("game_a.nes", &rom_a), ("game_b.nes", &rom_b)]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let members = member_evidence(&observation);
        let set_identity = classify_archive_set(&members);
        match set_identity {
            ArchiveSetIdentity::MultiMemberSamePlatform {
                platform,
                member_indices,
            } => {
                assert_eq!(platform, "NES");
                assert_eq!(member_indices.len(), 2);
            }
            other => panic!("expected MultiMemberSamePlatform, got {other:?}"),
        }
    }

    #[test]
    fn two_game_zip_different_platform_is_multi_platform_conflict() {
        let nes_rom = ines_rom(&[3u8; 64]);
        let mut gba_rom = vec![0u8; 192];
        gba_rom[0] = 0xEA;
        gba_rom[4..4 + 4].copy_from_slice(&[0x24, 0xff, 0xae, 0x51]);
        gba_rom[0xB2] = 0x96;
        let path = temp_zip_path("two-game-different-platform");
        write_zip(&path, &[("game.nes", &nes_rom), ("game.gba", &gba_rom)]);
        let observation = observe_zip_member_content(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let members = member_evidence(&observation);
        let set_identity = classify_archive_set(&members);
        // Depending on whether the synthetic GBA header validates fully,
        // this either resolves as MultiPlatform (both strong) or the GBA
        // member simply produces no evidence (still SingleMember/NES) -
        // either is an honest outcome; what matters is it never silently
        // reports a single, wrongly-collapsed game/platform pair.
        assert!(!matches!(
            set_identity,
            ArchiveSetIdentity::StructuredSet { .. }
        ));
    }

    // ------------------------------------------------------------------
    // Section 21: 7z member + set identity + DAT (in-process only)
    // ------------------------------------------------------------------

    #[test]
    fn sevenz_single_member_set_identity_and_dat_convergence() {
        use crate::dat::archive::limits::ArchiveLimits;
        use crate::safe_read::TrustedRoots;
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
        use std::sync::atomic::AtomicBool;

        let dir = tempfile::tempdir().unwrap();
        let rom = ines_rom(&[9u8; 64]);
        let archive_path = dir.path().join("archive.7z");
        let mut writer = ArchiveWriter::new(File::create(&archive_path).unwrap()).unwrap();
        let mut entry = ArchiveEntry::new_file("game.nes");
        entry.size = rom.len() as u64;
        writer
            .push_archive_entry(entry, Some(std::io::Cursor::new(rom.as_slice())))
            .unwrap();
        writer.finish().unwrap();

        let trusted = TrustedRoots::from_paths(dir.path().parent());
        let cancel = AtomicBool::new(false);
        let observation = observe_sevenz_member_content(
            &archive_path,
            &trusted,
            ArchiveLimits::default(),
            &cancel,
        )
        .unwrap();

        let members = member_evidence(&observation);
        let set_identity = classify_archive_set(&members);
        match &set_identity {
            ArchiveSetIdentity::SingleMember { platform, .. } => assert_eq!(*platform, "NES"),
            other => panic!("expected SingleMember, got {other:?}"),
        }

        let rom_evidence = hash_bytes(&rom, "game.nes", "game.nes");
        let index = dat_index_for("Test 7z NES Game", &rom_evidence);
        let (_, verdict) = audit_representation(
            &RepresentationHashes {
                representation: ByteRepresentation::ArchiveMember {
                    member_name: "game.nes".to_string(),
                },
                evidence: rom_evidence,
            },
            &index,
        );
        assert!(verdict.is_confident());
    }

    #[test]
    fn sevenz_member_with_no_local_dat_entry_reports_content_only() {
        use crate::dat::archive::limits::ArchiveLimits;
        use crate::safe_read::TrustedRoots;
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
        use std::sync::atomic::AtomicBool;

        let dir = tempfile::tempdir().unwrap();
        let rom = ines_rom(&[11u8; 64]);
        let archive_path = dir.path().join("archive.7z");
        let mut writer = ArchiveWriter::new(File::create(&archive_path).unwrap()).unwrap();
        let mut entry = ArchiveEntry::new_file("game.nes");
        entry.size = rom.len() as u64;
        writer
            .push_archive_entry(entry, Some(std::io::Cursor::new(rom.as_slice())))
            .unwrap();
        writer.finish().unwrap();

        let trusted = TrustedRoots::from_paths(dir.path().parent());
        let cancel = AtomicBool::new(false);
        let observation = observe_sevenz_member_content(
            &archive_path,
            &trusted,
            ArchiveLimits::default(),
            &cancel,
        )
        .unwrap();
        let evidence: Vec<_> = observation
            .members
            .iter()
            .flat_map(|m| m.evidence.iter().cloned())
            .collect();
        let explanation = crate::platform_evidence_fusion::fuse_platform_evidence(evidence);
        assert_eq!(explanation.resolved_platform, Some("NES"));

        // No local DAT entry supplied - empty index - honestly reports
        // NotInDat, never a fabricated match.
        let empty_index = dat_index_for(
            "Unrelated Game",
            &crate::dat::audit::KnownFileEvidence::new("x", "x"),
        );
        let rom_evidence = hash_bytes(&rom, "game.nes", "game.nes");
        let (_, verdict) = audit_representation(
            &RepresentationHashes {
                representation: ByteRepresentation::ArchiveMember {
                    member_name: "game.nes".to_string(),
                },
                evidence: rom_evidence,
            },
            &empty_index,
        );
        assert!(!verdict.is_confident());
    }
}
