//! Pure, read-only Sega 32X additional-evidence leg, layered on top of
//! [`crate::megadrive_header_evidence`] rather than duplicating it - a 32X
//! cartridge is, at the base-cartridge level, still a Mega Drive-format
//! cartridge (the 32X slots into the Mega Drive's own cartridge port), so
//! this module reuses [`crate::megadrive_header_evidence::parse_megadrive_header`]
//! for every base field and adds only the one genuinely 32X-specific check.
//!
//! # Why this cannot be "just the Mega Drive `SEGA` header"
//!
//! The milestone this module was written under is explicit that a Mega
//! Drive `SEGA`-at-`0x100` match is never, by itself, 32X evidence - every
//! 32X cartridge's base region carries that same header, and the overwhelming
//! majority of Mega Drive cartridges are not 32X titles. A genuine second,
//! independent leg is required.
//!
//! # What that second leg actually is here, and why it stays `Weak`
//!
//! Real 32X cartridges are widely observed (in the actual on-disk console-
//! name field of real dumps) to declare a variant like `"SEGA 32X"` rather
//! than plain `"SEGA GENESIS"`/`"SEGA MEGA DRIVE"` in the same
//! [`crate::megadrive_header_evidence::MegaDriveHeaderFact::console_name`]
//! field this module reuses unchanged. This research pass could not find a
//! primary specification document (Sega's own 32X Hardware Manual excerpts
//! reviewed describe the SH2-side "MARS" boot-security block at a fixed
//! offset within the *32X-side* boot ROM, not a byte-exact guarantee about
//! the cartridge's console-name string) that pins this convention down as a
//! guaranteed, unique byte pattern the way [`crate::gb_header_evidence`]'s
//! Nintendo logo or [`crate::gba_header_evidence`]'s complement checksum
//! are pinned down. Per this crate's collision-safety discipline (see
//! [`crate::header_normalization::HeaderNormalizationKind::SnesCopier512`]'s
//! precedent for "real but unconfirmed-as-unique" evidence), this leg is
//! therefore reported at [`crate::content_evidence::ContentEvidenceConfidence::Weak`]
//! only - real, additional, non-Mega-Drive-generic evidence, but explicitly
//! never strong enough to be mistaken for proof.

use crate::content_detector::{ContentDetectionOutcome, ContentDetector};
use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::megadrive_header_evidence::{MegaDriveHeaderFact, parse_megadrive_header};

/// The substring this module looks for in the reused
/// [`MegaDriveHeaderFact::console_name`] field - observed convention, not a
/// pinned specification value. See the module documentation.
const CONSOLE_NAME_32X_HINT: &str = "32X";

/// What this module additionally observes about a 32X candidate, layered on
/// an already-parsed [`MegaDriveHeaderFact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sega32xCandidateFact {
    /// Whether the reused Mega Drive `console_name` field's own `"SEGA"`
    /// convention was recognised - the base leg, unchanged from
    /// [`crate::megadrive_header_evidence`].
    pub megadrive_console_name_recognized: bool,
    /// Whether the console-name field additionally contains `"32X"` - the
    /// second, 32X-specific (but `Weak`) leg. See the module documentation.
    pub console_name_mentions_32x: bool,
}

/// Derives a [`Sega32xCandidateFact`] from an already-parsed
/// [`MegaDriveHeaderFact`] - no additional bytes are read; this is pure
/// re-interpretation of the same header fields
/// [`crate::megadrive_header_evidence::parse_megadrive_header`] already
/// extracted.
pub fn observe_sega32x_candidate(megadrive_fact: &MegaDriveHeaderFact) -> Sega32xCandidateFact {
    Sega32xCandidateFact {
        megadrive_console_name_recognized: megadrive_fact.console_name_recognized,
        console_name_mentions_32x: megadrive_fact
            .console_name
            .to_ascii_uppercase()
            .contains(CONSOLE_NAME_32X_HINT),
    }
}

/// Neutral evidence: **only** when both the base Mega Drive leg and the
/// `"32X"` console-name hint are present does this emit anything, and even
/// then only at `Weak` - matching the module documentation's "never Mega
/// Drive-generic, but never proof either" discipline. Neither leg alone
/// (just `SEGA`, or just some other file happening to contain `"32X"`)
/// produces any evidence.
pub fn observe_sega32x_evidence(fact: &Sega32xCandidateFact) -> Vec<ContentEvidence> {
    if !fact.megadrive_console_name_recognized || !fact.console_name_mentions_32x {
        return Vec::new();
    }
    vec![ContentEvidence::new(
        ContentEvidenceKind::ContentSignature,
        "32X",
        ContentEvidenceConfidence::Weak,
        "Mega Drive console-name field additionally mentions \"32X\" - a real, observed \
         convention on genuine 32X cartridges, but not confirmed against a primary specification \
         as a guaranteed unique byte pattern, so this stays Weak and is never treated as proof \
         on its own",
    )]
}

/// A [`ContentDetector`] wrapping the base Mega Drive header parse plus this
/// module's own `"32X"` console-name leg - lets a multi-detector caller
/// (such as [`crate::archive_member_content_evidence`]) see the 32X-specific
/// fact without hand-composing [`parse_megadrive_header`] and
/// [`observe_sega32x_candidate`] itself. Recognises only when the 32X leg
/// actually fires (see [`observe_sega32x_evidence`]) - a plain Mega Drive
/// header with no `"32X"` hint is
/// [`crate::megadrive_header_evidence::MegaDriveHeaderDetector`]'s territory,
/// not this one's.
pub struct Sega32xDetector;

impl ContentDetector for Sega32xDetector {
    fn id(&self) -> &'static str {
        "sega32x_console_name_leg"
    }

    fn detect(&self, data: &[u8]) -> ContentDetectionOutcome {
        let Some(megadrive_fact) = parse_megadrive_header(data) else {
            return ContentDetectionOutcome::NotRecognized;
        };
        let candidate = observe_sega32x_candidate(&megadrive_fact);
        let evidence = observe_sega32x_evidence(&candidate);
        if evidence.is_empty() {
            return ContentDetectionOutcome::NotRecognized;
        }
        ContentDetectionOutcome::Recognized { evidence }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::megadrive_header_evidence::tests_support::synthetic_header_for_tests;

    #[test]
    fn base_sega_header_alone_yields_no_32x_evidence() {
        let header = synthetic_header_for_tests(b"SEGA GENESIS", "GM 00000000");
        let megadrive_fact = parse_megadrive_header(&header).unwrap();
        let fact = observe_sega32x_candidate(&megadrive_fact);
        assert!(fact.megadrive_console_name_recognized);
        assert!(!fact.console_name_mentions_32x);
        assert!(observe_sega32x_evidence(&fact).is_empty());
    }

    #[test]
    fn console_name_with_32x_and_recognized_sega_yields_weak_evidence() {
        let header = synthetic_header_for_tests(b"SEGA 32X", "MK-84509");
        let megadrive_fact = parse_megadrive_header(&header).unwrap();
        let fact = observe_sega32x_candidate(&megadrive_fact);
        assert!(fact.megadrive_console_name_recognized);
        assert!(fact.console_name_mentions_32x);
        let evidence = observe_sega32x_evidence(&fact);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].confidence, ContentEvidenceConfidence::Weak);
    }

    #[test]
    fn unrecognized_sega_prefix_never_yields_32x_evidence_even_with_the_hint() {
        // "32X" present, but the console-name field does not even start
        // with "SEGA" - the base leg fails, so this must not be promoted.
        let header = synthetic_header_for_tests(b"NOT SEGA 32X AT ALL", "MK-84509");
        let megadrive_fact = parse_megadrive_header(&header).unwrap();
        let fact = observe_sega32x_candidate(&megadrive_fact);
        assert!(!fact.megadrive_console_name_recognized);
        assert!(observe_sega32x_evidence(&fact).is_empty());
    }

    #[test]
    fn evidence_never_reaches_strong_or_corroborated() {
        let header = synthetic_header_for_tests(b"SEGA 32X", "MK-84509");
        let megadrive_fact = parse_megadrive_header(&header).unwrap();
        let fact = observe_sega32x_candidate(&megadrive_fact);
        for item in observe_sega32x_evidence(&fact) {
            assert_eq!(item.confidence, ContentEvidenceConfidence::Weak);
        }
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let header = synthetic_header_for_tests(b"SEGA 32X", "MK-84509");
        let megadrive_fact = parse_megadrive_header(&header).unwrap();
        let fact = observe_sega32x_candidate(&megadrive_fact);
        for item in observe_sega32x_evidence(&fact) {
            assert_eq!(item.kind, ContentEvidenceKind::ContentSignature);
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let header = synthetic_header_for_tests(b"SEGA 32X", "MK-84509");
        let megadrive_fact = parse_megadrive_header(&header).unwrap();
        let a = observe_sega32x_candidate(&megadrive_fact);
        let b = observe_sega32x_candidate(&megadrive_fact);
        assert_eq!(a, b);
    }

    #[test]
    fn case_insensitive_hint_match() {
        let header = synthetic_header_for_tests(b"SEGA 32x", "MK-84509");
        let megadrive_fact = parse_megadrive_header(&header).unwrap();
        let fact = observe_sega32x_candidate(&megadrive_fact);
        assert!(fact.console_name_mentions_32x);
    }

    // ------------------------------------------------------------------
    // Sega32xDetector
    // ------------------------------------------------------------------

    #[test]
    fn detector_recognizes_32x_console_name() {
        let header = synthetic_header_for_tests(b"SEGA 32X", "MK-84509");
        assert!(Sega32xDetector.detect(&header).is_recognized());
    }

    #[test]
    fn detector_does_not_recognize_plain_mega_drive_header() {
        let header = synthetic_header_for_tests(b"SEGA GENESIS", "GM 00000000");
        assert_eq!(
            Sega32xDetector.detect(&header),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn detector_does_not_recognize_unrelated_bytes() {
        assert_eq!(
            Sega32xDetector.detect(b"nothing here"),
            ContentDetectionOutcome::NotRecognized
        );
    }

    #[test]
    fn detector_id_is_stable() {
        assert_eq!(Sega32xDetector.id(), "sega32x_console_name_leg");
    }
}
