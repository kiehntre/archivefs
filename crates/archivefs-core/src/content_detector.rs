//! The generic contract future content/media detectors implement.
//!
//! [`crate::content_evidence`] defines the *vocabulary* a detector reports
//! in - facts, never platforms. This module defines the *interface* a
//! detector implements to produce that vocabulary, and nothing else: no
//! detector implementation lives here, no library this crate might one day
//! depend on (`chd-rs`, `fluxfox`, `rtzx`, an Amiga HDF/RDB reader, a CBM
//! disk reader, an N64 header normaliser, a filesystem detector) is added by
//! this module, and no file is ever opened by anything in it.
//!
//! # The pipeline this contract exists to support
//!
//! ```text
//! DETECTOR OBSERVES  ->  EVIDENCE MODEL RECORDS  ->  RESOLVER DECIDES  ->  PLANNING LAYER ACTS
//! (this module)          (content_evidence)          (a future,             (a future, separately
//!                                                      separately reviewed    reviewed chunk;
//!                                                      bridge; not this       nothing here or in
//!                                                      chunk)                 dat::identity today)
//! ```
//!
//! A [`ContentDetector`] may only observe. It cannot resolve a canonical
//! platform (nothing in its signature lets it - see
//! [`ContentDetectionOutcome`]), cannot rename, move, delete, assign a RomM
//! library, write to a database, or mutate its input (`data: &[u8]` is an
//! immutable borrow; there is no other way in). Those are all later,
//! separately reviewed layers.

use crate::content_evidence::{ContentEvidence, observe_content_evidence};

/// A stable, read-only content detector.
///
/// Implementations are expected to be cheap, pure functions of `data`: given
/// the same bytes, [`ContentDetector::detect`] always returns the same
/// [`ContentDetectionOutcome`]. Nothing in this trait performs I/O, and
/// nothing in it can - the only input a detector ever sees is bytes the
/// caller already holds.
pub trait ContentDetector {
    /// A stable identifier for this detector - `"chd_header"`,
    /// `"tzx_header"`, `"n64_header"` - never a display label. Used to
    /// attribute evidence and diagnostics when more than one detector runs
    /// (see [`ContentDiagnostic::detector_id`] and
    /// [`run_content_detectors`]'s `recognized_by`/`not_recognized_by`
    /// lists). Must be the same string every time this detector is asked.
    fn id(&self) -> &'static str;

    /// Examines `data` - bytes the caller already holds in memory - and
    /// reports what was found. Read-only by construction: `data` is an
    /// immutable slice, so nothing a detector does can affect the caller's
    /// copy of it.
    ///
    /// `data` may be a short header prefix or a whole file's bytes; this
    /// trait makes no assumption either way. A detector that needs to
    /// distinguish "not enough bytes to tell" from "these bytes are not
    /// mine" reports that distinction itself, in its own
    /// [`ContentDetectionOutcome`]/[`ContentDiagnostic`] - the trait does
    /// not model it directly.
    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome;
}

/// What one [`ContentDetector::detect`] call found.
///
/// Three states, deliberately not two: "I have no evidence this is my
/// format" ([`Self::NotRecognized`]) is a different claim from "this is
/// unmistakably my format, but its internal structure fails validation"
/// ([`Self::Malformed`]), which is again different from a clean, complete
/// read ([`Self::Recognized`]). Collapsing the last two would let a
/// corrupted file silently look identical to a good one; collapsing the
/// first two would make every random byte string "malformed" instead of
/// simply unrecognised. Neither variant that carries evidence can carry
/// anything platform-shaped - there is no field here a
/// [`crate::dat::identity::DatPlatformEvidence`] could occupy, by
/// construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentDetectionOutcome {
    /// `data` gave this detector no reason to believe it recognises the
    /// format. Not a failure - most detectors will say this about most
    /// inputs.
    NotRecognized,
    /// `data` was validated well enough for this detector to stand behind
    /// the evidence it emits.
    Recognized { evidence: Vec<ContentEvidence> },
    /// `data` is recognisably this detector's format at some level - a
    /// magic number, a container signature - but failed structural
    /// validation beyond that point. `evidence` carries only what was
    /// actually validated (which may be nothing, or may be a genuine
    /// container-level fact even though something nested inside it is
    /// corrupt); `diagnostic` explains what failed and at what stage.
    Malformed {
        evidence: Vec<ContentEvidence>,
        diagnostic: ContentDiagnostic,
    },
}

impl ContentDetectionOutcome {
    /// The evidence this outcome carries, whether [`Self::Recognized`] or
    /// [`Self::Malformed`]. Empty for [`Self::NotRecognized`].
    pub fn evidence(&self) -> &[ContentEvidence] {
        match self {
            Self::NotRecognized => &[],
            Self::Recognized { evidence } | Self::Malformed { evidence, .. } => evidence,
        }
    }

    pub fn is_recognized(&self) -> bool {
        matches!(self, Self::Recognized { .. })
    }

    pub fn is_malformed(&self) -> bool {
        matches!(self, Self::Malformed { .. })
    }
}

/// Why a [`ContentDetector`] reported [`ContentDetectionOutcome::Malformed`].
///
/// Deliberately small: a stable detector id, a short stable category code,
/// and a human-readable message. No timestamp (this crate does not stamp
/// pure evidence with wall-clock time - see [`crate::content_evidence`]'s
/// own reasoning), no path (a diagnostic describes what was wrong with the
/// bytes, never where they came from - the caller already knows that), no
/// user-facing GUI copy, no platform assignment, and no suggested fix or
/// destructive recommendation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentDiagnostic {
    pub detector_id: &'static str,
    /// A short, stable code for what kind of problem this is - `"truncated"`,
    /// `"checksum_mismatch"`, `"bad_header"` - not a fixed enum, because
    /// detectors are added over time and a shared global list would need
    /// editing for every new one. Two detectors may reuse the same code when
    /// the failure really is the same kind of thing.
    pub category: &'static str,
    pub message: String,
}

/// The result of running a set of detectors over the same bytes.
///
/// `evidence` is every fact any detector stood behind - from
/// [`ContentDetectionOutcome::Recognized`] and
/// [`ContentDetectionOutcome::Malformed`] alike - canonicalised through
/// [`crate::content_evidence::observe_content_evidence`], so ordering,
/// deduplication, and conflict-preservation all follow that function's
/// already-proven rules; this report adds no resolution logic of its own.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentDetectionReport {
    pub evidence: Vec<ContentEvidence>,
    /// Every [`ContentDiagnostic`] from a detector that reported
    /// [`ContentDetectionOutcome::Malformed`], sorted by detector id, then
    /// category, then message.
    pub diagnostics: Vec<ContentDiagnostic>,
    /// Ids of detectors that reported [`ContentDetectionOutcome::Recognized`],
    /// sorted.
    pub recognized_by: Vec<&'static str>,
    /// Ids of detectors that reported [`ContentDetectionOutcome::NotRecognized`],
    /// sorted. A detector that reported `Malformed` appears in neither this
    /// list nor `recognized_by` - it is a third, distinct outcome.
    pub not_recognized_by: Vec<&'static str>,
}

/// Runs every detector in `detectors` over the same `data` and combines the
/// results deterministically.
///
/// Pure and read-only: `data` is borrowed immutably throughout, every
/// detector is called at most once, and nothing here decides a platform,
/// resolves a conflict between detectors, or turns "two Weak facts" into a
/// Strong one - conflicting or merely differently-confident facts about the
/// same [`crate::content_evidence::ContentEvidenceKind`]/value are preserved
/// exactly as [`crate::content_evidence::observe_content_evidence`] already
/// preserves them. Detector order never affects the result: every list this
/// produces is sorted independently of the order `detectors` was given in.
pub fn run_content_detectors<'a>(
    detectors: impl IntoIterator<Item = &'a dyn ContentDetector>,
    data: &[u8],
) -> ContentDetectionReport {
    let mut all_evidence: Vec<ContentEvidence> = Vec::new();
    let mut diagnostics: Vec<ContentDiagnostic> = Vec::new();
    let mut recognized_by: Vec<&'static str> = Vec::new();
    let mut not_recognized_by: Vec<&'static str> = Vec::new();

    for detector in detectors {
        match detector.detect(data) {
            ContentDetectionOutcome::NotRecognized => {
                not_recognized_by.push(detector.id());
            }
            ContentDetectionOutcome::Recognized { evidence } => {
                recognized_by.push(detector.id());
                all_evidence.extend(evidence);
            }
            ContentDetectionOutcome::Malformed {
                evidence,
                diagnostic,
            } => {
                all_evidence.extend(evidence);
                diagnostics.push(diagnostic);
            }
        }
    }

    recognized_by.sort_unstable();
    not_recognized_by.sort_unstable();
    diagnostics.sort_by(|left, right| {
        left.detector_id
            .cmp(right.detector_id)
            .then_with(|| left.category.cmp(right.category))
            .then_with(|| left.message.cmp(&right.message))
    });

    ContentDetectionReport {
        evidence: observe_content_evidence(all_evidence).facts,
        diagnostics,
        recognized_by,
        not_recognized_by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_evidence::{ContentEvidenceConfidence, ContentEvidenceKind, value};

    /// Never recognises anything. Stands in for the common case: most
    /// detectors say `NotRecognized` about most inputs.
    struct AlwaysUnrecognized;
    impl ContentDetector for AlwaysUnrecognized {
        fn id(&self) -> &'static str {
            "test_always_unrecognized"
        }
        fn detect(&self, _data: &[u8]) -> ContentDetectionOutcome {
            ContentDetectionOutcome::NotRecognized
        }
    }

    /// Recognises anything starting with `MAGIC`, at Strong confidence, as a
    /// stand-in for a real container/header detector.
    struct MagicContainerDetector;
    impl ContentDetector for MagicContainerDetector {
        fn id(&self) -> &'static str {
            "test_magic_container"
        }
        fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
            if data.starts_with(b"MAGIC") {
                ContentDetectionOutcome::Recognized {
                    evidence: vec![ContentEvidence::new(
                        ContentEvidenceKind::Container,
                        value::CHD,
                        ContentEvidenceConfidence::Strong,
                        "test fixture: `MAGIC` prefix",
                    )],
                }
            } else {
                ContentDetectionOutcome::NotRecognized
            }
        }
    }

    /// Recognises the `MAGIC` prefix but always reports that the structure
    /// past it fails validation - a stand-in for "valid header, corrupt
    /// body."
    struct MalformedBodyDetector;
    impl ContentDetector for MalformedBodyDetector {
        fn id(&self) -> &'static str {
            "test_malformed_body"
        }
        fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
            if !data.starts_with(b"MAGIC") {
                return ContentDetectionOutcome::NotRecognized;
            }
            ContentDetectionOutcome::Malformed {
                evidence: vec![ContentEvidence::new(
                    ContentEvidenceKind::Container,
                    value::CHD,
                    ContentEvidenceConfidence::Strong,
                    "test fixture: container header valid",
                )],
                diagnostic: ContentDiagnostic {
                    detector_id: "test_malformed_body",
                    category: "truncated",
                    message: "test fixture: body shorter than declared".to_string(),
                },
            }
        }
    }

    /// Emits several independent facts at once, as a stand-in for a real
    /// detector that learns more than one thing from one header.
    struct MultiFactDetector;
    impl ContentDetector for MultiFactDetector {
        fn id(&self) -> &'static str {
            "test_multi_fact"
        }
        fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
            if !data.starts_with(b"MAGIC") {
                return ContentDetectionOutcome::NotRecognized;
            }
            ContentDetectionOutcome::Recognized {
                evidence: vec![
                    ContentEvidence::new(
                        ContentEvidenceKind::Container,
                        value::CHD,
                        ContentEvidenceConfidence::Strong,
                        "test fixture",
                    ),
                    ContentEvidence::new(
                        ContentEvidenceKind::MediaClass,
                        value::GD_ROM,
                        ContentEvidenceConfidence::Strong,
                        "test fixture",
                    ),
                ],
            }
        }
    }

    /// Disagrees with [`MultiFactDetector`] about media class, to exercise
    /// contradictory-evidence preservation.
    struct ContradictingMediaClassDetector;
    impl ContentDetector for ContradictingMediaClassDetector {
        fn id(&self) -> &'static str {
            "test_contradicting_media_class"
        }
        fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
            if !data.starts_with(b"MAGIC") {
                return ContentDetectionOutcome::NotRecognized;
            }
            ContentDetectionOutcome::Recognized {
                evidence: vec![ContentEvidence::new(
                    ContentEvidenceKind::MediaClass,
                    value::HARD_DISK,
                    ContentEvidenceConfidence::Strong,
                    "test fixture: disagrees with MultiFactDetector",
                )],
            }
        }
    }

    #[test]
    fn a_detector_can_return_not_recognized_with_no_evidence() {
        let outcome = AlwaysUnrecognized.detect(b"anything");
        assert_eq!(outcome, ContentDetectionOutcome::NotRecognized);
        assert!(outcome.evidence().is_empty());
        assert!(!outcome.is_recognized());
        assert!(!outcome.is_malformed());
    }

    #[test]
    fn a_detector_can_return_recognized_with_strong_evidence() {
        let outcome = MagicContainerDetector.detect(b"MAGICxyz");
        assert!(outcome.is_recognized());
        assert_eq!(outcome.evidence().len(), 1);
        assert_eq!(
            outcome.evidence()[0].confidence,
            ContentEvidenceConfidence::Strong
        );
    }

    #[test]
    fn a_detector_can_return_malformed_without_claiming_a_platform() {
        let outcome = MalformedBodyDetector.detect(b"MAGICxyz");
        assert!(outcome.is_malformed());
        // The evidence this variant carries is still ordinary
        // ContentEvidence - there is no field anywhere in this outcome a
        // platform could occupy.
        assert_eq!(outcome.evidence().len(), 1);
        match outcome {
            ContentDetectionOutcome::Malformed { diagnostic, .. } => {
                assert_eq!(diagnostic.detector_id, "test_malformed_body");
                assert_eq!(diagnostic.category, "truncated");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn a_detector_not_recognizing_unrelated_bytes_still_reports_malformed_for_its_own_prefix() {
        assert_eq!(
            MalformedBodyDetector.detect(b"not the magic"),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn multiple_content_evidence_records_can_be_returned() {
        let outcome = MultiFactDetector.detect(b"MAGIC!");
        assert_eq!(outcome.evidence().len(), 2);
        assert!(
            outcome
                .evidence()
                .iter()
                .any(|fact| fact.kind == ContentEvidenceKind::Container)
        );
        assert!(
            outcome
                .evidence()
                .iter()
                .any(|fact| fact.kind == ContentEvidenceKind::MediaClass)
        );
    }

    #[test]
    fn detector_identity_is_stable() {
        let detector = MagicContainerDetector;
        assert_eq!(detector.id(), detector.id());
        assert_eq!(detector.id(), "test_magic_container");
    }

    #[test]
    fn input_is_never_mutated() {
        let original: Vec<u8> = b"MAGIC and then some bytes".to_vec();
        let before = original.clone();
        let _ = MultiFactDetector.detect(&original);
        assert_eq!(original, before, "detect() must never mutate its input");
    }

    #[test]
    fn the_core_interface_needs_no_path_or_filesystem_access() {
        // No `std::path::Path`, no `std::fs`, no temp file anywhere in this
        // test - a plain in-memory byte vector is the entire input a
        // detector ever sees.
        let bytes: Vec<u8> = vec![0x4d, 0x41, 0x47, 0x49, 0x43];
        let outcome = MagicContainerDetector.detect(&bytes);
        assert!(outcome.is_recognized());
    }

    #[test]
    fn orchestration_evidence_ordering_is_deterministic_regardless_of_detector_order() {
        let magic: Box<dyn ContentDetector> = Box::new(MagicContainerDetector);
        let multi: Box<dyn ContentDetector> = Box::new(MultiFactDetector);
        let unrecognized: Box<dyn ContentDetector> = Box::new(AlwaysUnrecognized);

        let forward: Vec<&dyn ContentDetector> =
            vec![magic.as_ref(), multi.as_ref(), unrecognized.as_ref()];
        let reversed: Vec<&dyn ContentDetector> =
            vec![unrecognized.as_ref(), multi.as_ref(), magic.as_ref()];

        let data = b"MAGIC payload";
        let forward_report = run_content_detectors(forward, data);
        let reversed_report = run_content_detectors(reversed, data);
        assert_eq!(forward_report, reversed_report);
    }

    #[test]
    fn orchestration_preserves_contradictory_evidence() {
        let multi: Box<dyn ContentDetector> = Box::new(MultiFactDetector);
        let contradicting: Box<dyn ContentDetector> = Box::new(ContradictingMediaClassDetector);
        let detectors: Vec<&dyn ContentDetector> = vec![multi.as_ref(), contradicting.as_ref()];

        let report = run_content_detectors(detectors, b"MAGIC payload");

        let media_class_values: Vec<&str> = report
            .evidence
            .iter()
            .filter(|fact| fact.kind == ContentEvidenceKind::MediaClass)
            .map(|fact| fact.value.as_str())
            .collect();
        assert!(media_class_values.contains(&value::GD_ROM));
        assert!(media_class_values.contains(&value::HARD_DISK));
        assert_eq!(
            report.recognized_by,
            vec!["test_contradicting_media_class", "test_multi_fact"]
        );
    }

    #[test]
    fn orchestration_preserves_diagnostics_and_never_upgrades_confidence() {
        let malformed: Box<dyn ContentDetector> = Box::new(MalformedBodyDetector);
        let unrecognized: Box<dyn ContentDetector> = Box::new(AlwaysUnrecognized);
        let detectors: Vec<&dyn ContentDetector> = vec![malformed.as_ref(), unrecognized.as_ref()];

        let report = run_content_detectors(detectors, b"MAGIC payload");

        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].detector_id, "test_malformed_body");
        assert_eq!(report.not_recognized_by, vec!["test_always_unrecognized"]);
        assert!(report.recognized_by.is_empty());
        // One Strong fact from one detector stays exactly Strong - nothing
        // here combines evidence into a higher confidence than any single
        // detector reported.
        assert_eq!(report.evidence.len(), 1);
        assert_eq!(
            report.evidence[0].confidence,
            ContentEvidenceConfidence::Strong
        );
    }

    #[test]
    fn not_recognized_by_and_recognized_by_are_disjoint_from_malformed() {
        let malformed: Box<dyn ContentDetector> = Box::new(MalformedBodyDetector);
        let report = run_content_detectors(vec![malformed.as_ref()], b"MAGIC payload");
        assert!(report.recognized_by.is_empty());
        assert!(report.not_recognized_by.is_empty());
        assert_eq!(report.diagnostics.len(), 1);
    }
}
