//! Pure, read-only CHD identity observation.
//!
//! This is the fourth normalization/identity prototype in this series, after
//! [`crate::n64_byte_order`], [`crate::header_normalization`], and
//! [`crate::smd_normalization`] - and structurally the most different one.
//! Those three each produce a *canonical byte view*; this module produces no
//! bytes at all. A CHD's compressed hunks are opaque without decompressing
//! them, and this chunk deliberately does not decompress anything. Instead
//! it exposes the identity facts a CHD v5 header *already records
//! authoritatively*, reusing the existing, already-reviewed
//! [`crate::dat::archive::chd`] header parser rather than adding a new
//! dependency or duplicating its byte-offset logic.
//!
//! # A CHD has more than one identity - verified, not assumed
//!
//! Before writing anything here, the exact semantics of the three SHA-1
//! fields a CHD v5 header carries were verified against MAME's own
//! authoritative source
//! (`https://github.com/mamedev/mame/blob/master/src/lib/util/chd.h`):
//!
//! ```text
//! [ 64] uint8_t  rawsha1[20];    // raw data SHA1
//! [ 84] uint8_t  sha1[20];       // combined raw+meta SHA1
//! [104] uint8_t  parentsha1[20]; // combined raw+meta SHA1 of parent
//! ```
//!
//! and: *"If parentsha1 != 0, we have a parent (no need for flags)"* - which
//! is exactly what [`crate::dat::archive::chd::ChdV5Header::parent_required`]
//! already implements. This module adds a fourth, structurally distinct
//! identity on top of those three - the **physical** identity of the `.chd`
//! file's own compressed bytes - and keeps all four separate everywhere:
//!
//! | Identity | What it measures | Where it lives |
//! |---|---|---|
//! | Physical CHD SHA-256 | the compressed `.chd` file's own bytes | computed by a caller (e.g. [`examples/chd_probe.rs`](../../examples/chd_probe.rs)) over the whole file, via the crate's existing hashing helper |
//! | CHD raw SHA-1 | the logical/raw data stream *inside* the CHD | [`ChdIdentityObservation::raw_sha1`] |
//! | CHD combined SHA-1 | raw data + metadata together - what a MAME-style DAT `<disk sha1="...">` entry actually names | [`ChdIdentityObservation::combined_sha1`] |
//! | CHD parent SHA-1 | the combined SHA-1 a *different* CHD must have to serve as this one's parent | [`ChdIdentityObservation::parent_sha1`] |
//!
//! None of these four are interchangeable, and this module never conflates
//! any pair of them.
//!
//! # What this chunk deliberately does not do
//!
//! - It does not traverse the CHD map or metadata chain - the underlying
//!   [`crate::dat::archive::chd::read_chd_v5_header`] stops at byte 124, and
//!   this module does not extend it. [`ChdIdentityObservation::metadata_summary`]
//!   is always `None` here; real metadata-tag interpretation (media class,
//!   track layout, disc serial) is deferred to a later, separately reviewed
//!   chunk, exactly as the task allows.
//! - It does not support CHD v3/v4: the underlying reader refuses them
//!   outright, so every successfully-parsed [`ChdIdentityObservation`] is a
//!   v5 header with every field structurally present - no field here needs
//!   to be `Option` for a v5 header, because v5 never has a partially-absent
//!   set of these fields. The "genuinely absent for another version" case is
//!   represented at the outcome level instead (a plain parse failure), never
//!   as a null field on a half-parsed struct.
//! - It never claims a canonical platform. See [`ChdIdentityDetector`].

use std::io::Cursor;

use crate::content_detector::{ContentDetectionOutcome, ContentDetector, ContentDiagnostic};
use crate::content_evidence::{
    ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind, value,
};
use crate::dat::archive::chd::{CHD_MAGIC, ChdHeaderError, read_chd_v5_header};

/// Every CHD v5 header identity fact this module observes, plus whether a
/// parent CHD is required.
///
/// `raw_sha1`, `combined_sha1`, and `parent_sha1` are never given the same
/// field name or type alias as each other, and none of them is the physical
/// `.chd` file's own hash - see the module documentation's identity table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChdIdentityObservation {
    /// Always `5` today - the only version [`read_chd_v5_header`] parses.
    pub version: u32,
    pub logical_bytes: u64,
    pub hunk_bytes: u32,
    pub unit_bytes: u32,
    /// SHA-1 of the raw/logical data stream only - never metadata, never the
    /// physical file.
    pub raw_sha1: [u8; 20],
    /// SHA-1 of raw data *and* metadata combined. This, not `raw_sha1`, is
    /// what a MAME-style DAT `<disk sha1="...">` entry identifies.
    pub combined_sha1: [u8; 20],
    /// The combined SHA-1 a parent CHD must have for this CHD to attach to
    /// it. All-zero when this CHD is standalone.
    pub parent_sha1: [u8; 20],
    /// `true` exactly when `parent_sha1` is non-zero. A `true` value is not
    /// a corruption signal - see [`ChdIdentityOutcome`] and the module
    /// documentation.
    pub parent_required: bool,
    /// Always `None` in this chunk - see the module documentation's
    /// "what this chunk deliberately does not do" section.
    pub metadata_summary: Option<String>,
}

impl ChdIdentityObservation {
    pub fn raw_sha1_hex(&self) -> String {
        hex(&self.raw_sha1)
    }

    pub fn combined_sha1_hex(&self) -> String {
        hex(&self.combined_sha1)
    }

    pub fn parent_sha1_hex(&self) -> String {
        hex(&self.parent_sha1)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether `data`'s first bytes are the fixed CHD magic
/// (`"MComprHD"`) - a cheap, pure pre-check that decides nothing about
/// whether the header is otherwise valid. Reuses the exact magic constant
/// [`read_chd_v5_header`] itself checks (`crate::dat::archive::chd::CHD_MAGIC`),
/// so there is only ever one literal copy of it in the crate.
pub fn looks_like_chd(data: &[u8]) -> bool {
    data.len() >= CHD_MAGIC.len() && &data[..CHD_MAGIC.len()] == CHD_MAGIC.as_slice()
}

/// Parses `data` as a CHD v5 header and returns every identity fact it
/// records.
///
/// Pure and read-only: `data` is an immutable byte slice (wrapped in a
/// `Cursor` only so the existing `Read + Seek`-based
/// [`read_chd_v5_header`] can be reused without duplicating its parsing
/// logic), never mutated, and at most the fixed 124-byte v5 header is read -
/// nothing past it. Fails with [`ChdHeaderError`] exactly when
/// `read_chd_v5_header` would: bad magic, wrong length, an unsupported
/// version, a truncated header, or invalid geometry. A CHD that legitimately
/// requires a parent is **not** a failure case here - `parent_required` is
/// simply `true` in an otherwise-`Ok` observation, because reading a child
/// CHD's own header never requires opening its parent.
pub fn observe_chd_identity(data: &[u8]) -> Result<ChdIdentityObservation, ChdHeaderError> {
    let mut cursor = Cursor::new(data);
    let header = read_chd_v5_header(&mut cursor)?;
    Ok(ChdIdentityObservation {
        version: 5,
        logical_bytes: header.logical_bytes,
        hunk_bytes: header.hunk_bytes,
        unit_bytes: header.unit_bytes,
        raw_sha1: header.raw_sha1,
        combined_sha1: header.overall_sha1,
        parent_sha1: header.parent_sha1,
        parent_required: header.parent_required(),
        metadata_summary: None,
    })
}

/// A [`ContentDetector`] for CHD identity.
///
/// - [`ContentDetectionOutcome::NotRecognized`]: `data` does not begin with
///   the CHD magic at all - no evidence this is a CHD.
/// - [`ContentDetectionOutcome::Recognized`]: a valid, fully-readable CHD v5
///   header, whether standalone or a child requiring a parent. A required
///   parent is reported as an *additional* fact in the evidence list, never
///   as a reason to withhold recognition - see the module documentation.
/// - [`ContentDetectionOutcome::Malformed`]: the magic matched (this is
///   recognizably a CHD) but the header failed to parse - wrong length, an
///   unsupported version, truncation, or invalid geometry.
///
/// Every fact emitted is about the *container*, never a platform:
/// `Container = "CHD"` and a `ContentSignature` naming the header version
/// (and, when applicable, that a parent is required) are the only evidence
/// kinds this detector ever produces. Nothing here infers Dreamcast, MAME,
/// Sega CD, Neo Geo CD, or any other platform from a CHD alone - a CHD can
/// legitimately hold content for many different systems, and
/// [`crate::platform::PLATFORMS`] remains untouched by this module entirely.
pub struct ChdIdentityDetector;

impl ContentDetector for ChdIdentityDetector {
    fn id(&self) -> &'static str {
        "chd_identity"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        if !looks_like_chd(data) {
            return ContentDetectionOutcome::NotRecognized;
        }

        match observe_chd_identity(data) {
            Ok(observation) => {
                let mut evidence = vec![
                    ContentEvidence::new(
                        ContentEvidenceKind::Container,
                        value::CHD,
                        ContentEvidenceConfidence::Strong,
                        "a valid CHD v5 header was parsed",
                    ),
                    ContentEvidence::new(
                        ContentEvidenceKind::ContentSignature,
                        format!("chd-v{}", observation.version),
                        ContentEvidenceConfidence::Strong,
                        "CHD header version field",
                    ),
                ];
                if observation.parent_required {
                    evidence.push(ContentEvidence::new(
                        ContentEvidenceKind::ContentSignature,
                        "chd-parent-required",
                        ContentEvidenceConfidence::Strong,
                        format!(
                            "this CHD's header declares a non-zero parent SHA-1 ({}); it is a \
                             child/differential CHD - this is a structural fact, not a \
                             corruption signal",
                            observation.parent_sha1_hex()
                        ),
                    ));
                }
                ContentDetectionOutcome::Recognized { evidence }
            }
            Err(error) => ContentDetectionOutcome::Malformed {
                evidence: Vec::new(),
                diagnostic: ContentDiagnostic {
                    detector_id: "chd_identity",
                    category: malformed_category(&error),
                    message: error.to_string(),
                },
            },
        }
    }
}

fn malformed_category(error: &ChdHeaderError) -> &'static str {
    match error {
        ChdHeaderError::Truncated { .. } => "truncated",
        ChdHeaderError::InvalidMagic => "invalid_magic",
        ChdHeaderError::InvalidLength { .. } => "invalid_length",
        ChdHeaderError::UnsupportedVersion { .. } => "unsupported_version",
        ChdHeaderError::InvalidGeometry(_) => "invalid_geometry",
        ChdHeaderError::Io(_) => "io_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::archive::hash::hash_member_stream;
    use std::sync::atomic::AtomicBool;

    const RAW_SHA1: [u8; 20] = [0x11; 20];
    const COMBINED_SHA1: [u8; 20] = [0x22; 20];

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }

    /// A synthetic, valid CHD v5 header - the same 124-byte layout
    /// `crate::dat::archive::chd`'s own tests build, constructed
    /// independently here so this module's tests do not depend on that
    /// module's private test helpers.
    fn synthetic_chd_header(parent_sha1: [u8; 20]) -> Vec<u8> {
        let mut bytes = vec![0u8; 124];
        bytes[0..8].copy_from_slice(CHD_MAGIC);
        put_u32(&mut bytes, 8, 124);
        put_u32(&mut bytes, 12, 5);
        put_u64(&mut bytes, 32, 0x1234_5678_0000_0000); // logical_bytes
        put_u64(&mut bytes, 40, 0); // map_offset
        put_u64(&mut bytes, 48, 0); // meta_offset
        put_u32(&mut bytes, 56, 0x0002_0000); // hunk_bytes
        put_u32(&mut bytes, 60, 0x0000_0800); // unit_bytes
        bytes[64..84].copy_from_slice(&RAW_SHA1);
        bytes[84..104].copy_from_slice(&COMBINED_SHA1);
        bytes[104..124].copy_from_slice(&parent_sha1);
        bytes
    }

    fn sha256_hex(data: &[u8]) -> String {
        hash_member_stream(data, data.len() as u64, &AtomicBool::new(false))
            .expect("hashing an in-memory buffer never fails")
            .hashes
            .sha256
    }

    // ------------------------------------------------------------------
    // Recognition
    // ------------------------------------------------------------------

    #[test]
    fn non_chd_data_is_not_recognized() {
        let data = b"this is definitely not a CHD file at all, just text";
        assert!(!looks_like_chd(data));
        assert_eq!(
            ChdIdentityDetector.detect(data),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn empty_input_is_not_recognized() {
        assert!(!looks_like_chd(&[]));
        assert_eq!(
            ChdIdentityDetector.detect(&[]),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn valid_chd_is_recognized() {
        let data = synthetic_chd_header([0; 20]);
        assert!(looks_like_chd(&data));
        let outcome = ChdIdentityDetector.detect(&data);
        assert!(outcome.is_recognized());
    }

    // ------------------------------------------------------------------
    // Identity fields
    // ------------------------------------------------------------------

    #[test]
    fn chd_version_is_recorded() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.version, 5);
    }

    #[test]
    fn logical_size_is_recorded() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.logical_bytes, 0x1234_5678_0000_0000);
    }

    #[test]
    fn raw_sha1_is_exposed_distinctly() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.raw_sha1, RAW_SHA1);
        assert_ne!(observation.raw_sha1, observation.combined_sha1);
    }

    #[test]
    fn combined_sha1_is_exposed_distinctly() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert_eq!(observation.combined_sha1, COMBINED_SHA1);
        assert_ne!(observation.combined_sha1, observation.raw_sha1);
    }

    #[test]
    fn physical_sha256_remains_distinct_from_chd_logical_hashes() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        let physical = sha256_hex(&data);
        // Different algorithms (SHA-256 vs SHA-1) and different lengths
        // (64 hex chars vs 40) already make these impossible to conflate by
        // type, but the point being proven is conceptual: the physical hash
        // covers the *compressed file bytes*, while raw/combined SHA-1 cover
        // only the identity the header itself declares.
        assert_eq!(physical.len(), 64);
        assert_eq!(observation.raw_sha1_hex().len(), 40);
        assert_eq!(observation.combined_sha1_hex().len(), 40);
        assert_ne!(physical, observation.raw_sha1_hex());
        assert_ne!(physical, observation.combined_sha1_hex());
    }

    // ------------------------------------------------------------------
    // Parent handling
    // ------------------------------------------------------------------

    #[test]
    fn zero_parent_hash_is_standalone() {
        let data = synthetic_chd_header([0; 20]);
        let observation = observe_chd_identity(&data).unwrap();
        assert!(!observation.parent_required);
        assert_eq!(observation.parent_sha1, [0; 20]);
    }

    #[test]
    fn nonzero_parent_hash_requires_a_parent() {
        let mut parent = [0u8; 20];
        parent[19] = 1;
        let data = synthetic_chd_header(parent);
        let observation = observe_chd_identity(&data).unwrap();
        assert!(observation.parent_required);
        assert_eq!(observation.parent_sha1, parent);
    }

    #[test]
    fn missing_parent_is_not_called_corrupt() {
        let mut parent = [0u8; 20];
        parent[0] = 0xaa;
        let data = synthetic_chd_header(parent);

        // Observing this CHD's own header never requires its parent to be
        // present anywhere - it succeeds outright.
        let observation = observe_chd_identity(&data).unwrap();
        assert!(observation.parent_required);

        // And the detector reports it as Recognized, not Malformed.
        let outcome = ChdIdentityDetector.detect(&data);
        assert!(outcome.is_recognized());
        assert!(!outcome.is_malformed());
        assert!(
            outcome
                .evidence()
                .iter()
                .any(|fact| fact.value == "chd-parent-required")
        );
    }

    // ------------------------------------------------------------------
    // Fail-closed behaviour
    // ------------------------------------------------------------------

    #[test]
    fn malformed_recognizable_chd_fails_closed() {
        let mut data = synthetic_chd_header([0; 20]);
        put_u32(&mut data, 56, 0); // hunk_bytes = 0: invalid geometry
        assert!(looks_like_chd(&data));
        assert!(observe_chd_identity(&data).is_err());

        let outcome = ChdIdentityDetector.detect(&data);
        assert!(outcome.is_malformed());
        match outcome {
            ContentDetectionOutcome::Malformed { diagnostic, .. } => {
                assert_eq!(diagnostic.detector_id, "chd_identity");
                assert_eq!(diagnostic.category, "invalid_geometry");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn truncated_chd_header_fails_closed() {
        let data = synthetic_chd_header([0; 20]);
        let truncated = &data[..20];
        assert!(looks_like_chd(truncated));
        assert!(observe_chd_identity(truncated).is_err());
        assert!(ChdIdentityDetector.detect(truncated).is_malformed());
    }

    #[test]
    fn unsupported_version_fails_closed_and_is_not_fabricated() {
        let mut data = synthetic_chd_header([0; 20]);
        // A v4 header is a different, shorter layout; this module does not
        // (and must not) invent v4 semantics from a v5-shaped buffer.
        put_u32(&mut data, 12, 4);
        let outcome = ChdIdentityDetector.detect(&data[..16]);
        match outcome {
            ContentDetectionOutcome::Malformed { diagnostic, .. } => {
                assert_eq!(diagnostic.category, "unsupported_version");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // General
    // ------------------------------------------------------------------

    #[test]
    fn original_bytes_are_never_modified() {
        let data = synthetic_chd_header([0; 20]);
        let before = data.clone();
        let _ = observe_chd_identity(&data);
        assert_eq!(
            data, before,
            "observe_chd_identity must never mutate its input"
        );
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let data = synthetic_chd_header([0; 20]);
        let first = observe_chd_identity(&data).unwrap();
        let second = observe_chd_identity(&data).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn chd_evidence_never_resolves_a_platform() {
        // Structural: there is no platform-shaped field anywhere on
        // ChdIdentityObservation or in the evidence this detector emits -
        // every value is `Container`/`ContentSignature`, never a canonical
        // platform id, and this module imports nothing from
        // `crate::platform` or `crate::dat::identity`.
        let data = synthetic_chd_header([0; 20]);
        let outcome = ChdIdentityDetector.detect(&data);
        for fact in outcome.evidence() {
            assert!(matches!(
                fact.kind,
                ContentEvidenceKind::Container | ContentEvidenceKind::ContentSignature
            ));
        }
    }

    #[test]
    fn container_evidence_is_chd() {
        let data = synthetic_chd_header([0; 20]);
        let outcome = ChdIdentityDetector.detect(&data);
        assert!(
            outcome
                .evidence()
                .iter()
                .any(|fact| fact.kind == ContentEvidenceKind::Container
                    && fact.value == value::CHD
                    && fact.confidence == ContentEvidenceConfidence::Strong)
        );
    }

    #[test]
    fn detector_id_is_stable() {
        assert_eq!(ChdIdentityDetector.id(), ChdIdentityDetector.id());
        assert_eq!(ChdIdentityDetector.id(), "chd_identity");
    }
}
