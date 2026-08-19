//! Pure content/container/media evidence - deliberately not platform
//! evidence.
//!
//! # The distinction this module exists to draw
//!
//! Knowing *what kind of object a file is* is a different question from
//! knowing *which canonical platform it belongs to*. A CHD's own metadata
//! can prove, with total confidence, that a file is a valid CHD container
//! holding GD-ROM media - and say nothing at all about whether that GD-ROM
//! is a Dreamcast disc, a Naomi arcade disc, or something else that also
//! happens to use GD-ROM. A TZX header proves "this is a TZX tape
//! container" outright; TZX/CDT is used across more than one 8-bit platform
//! family, so it does not by itself prove "this is a ZX Spectrum tape."
//!
//! [`crate::dat::identity`] already draws exactly this kind of line for DAT
//! text/machine evidence versus canonical platform identity, and
//! [`crate::platform`]'s `MagicConfidence` already separates "this rule's
//! bytes are genuinely distinctive" from "a platform currently matches it."
//! This module is the same discipline applied one level earlier, before any
//! platform is even considered: a detector (a future `chd-rs`, `fluxfox`,
//! `rtzx`, or `opticaldiscs` adapter) reports facts about the *object* -
//! container, media class, tape format, disk encoding, filesystem, boot
//! structure, or a raw content signature - and nothing in this module ever
//! turns one of those facts into a platform. [`crate::platform::PLATFORMS`]
//! remains the only canonical platform registry; nothing here duplicates or
//! competes with it.
//!
//! # Scope
//!
//! This module defines the vocabulary and a small, non-deciding aggregator
//! only. It performs no I/O of any kind, wraps no parsing library, and is
//! not called by anything else in the crate yet - a future chunk wires a
//! real detector's output through [`ContentEvidence`] once one exists.
//!
//! # `ContentEvidenceConfidence` is not `DatPlatformConfidence`
//!
//! These are deliberately two different dimensions with the same three
//! names. [`ContentEvidenceConfidence::Strong`] means "this fact about the
//! *object* is certain" - it never means, and must never be converted into,
//! [`crate::dat::identity::DatPlatformConfidence::Strong`], which is a claim
//! about canonical *platform* identity. A `MediaClass = GD-ROM` fact at
//! `Strong` confidence can coexist with platform identity remaining
//! completely `Unknown`; nothing in this module bridges the two, and no
//! future bridge may make that conversion implicit or automatic.

use serde::{Deserialize, Serialize};

/// Which domain one piece of content evidence describes.
///
/// Declaration order is the tie-break order [`observe_content_evidence`]
/// sorts by; it carries no other meaning. Deliberately a small, fixed set of
/// *domains* rather than one variant per possible fact - the domain is what
/// needs type safety (a caller filtering for `MediaClass` facts should never
/// have to also know every value string that might appear); the fact itself
/// is a plain value (see [`ContentEvidence::value`]), so a new format never
/// requires a new variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentEvidenceKind {
    /// The outer container format: `CHD`, `ISO`, `CueBin`, `GDI`, `Archive`.
    Container,
    /// The class of media the content represents: `CD-ROM`, `GD-ROM`,
    /// `DVD`, `HardDisk`, `LaserDisc`, `Floppy`, `Tape`, `Cartridge`.
    MediaClass,
    /// A tape container format: `TZX`, `CDT`, `Commodore TAP`.
    TapeFormat,
    /// A disk container/structural format, distinct from the raw encoding -
    /// for example a specific floppy image schema.
    DiskFormat,
    /// The physical/logical encoding a disk image carries: `MFM`, `GCR`.
    DiskEncoding,
    /// A filesystem found inside the content: `FAT12`, `AmigaDOS`,
    /// `ISO9660`.
    Filesystem,
    /// A recognised boot-sector or boot-record structure, short of a full
    /// filesystem identification.
    BootStructure,
    /// A raw content signature that doesn't cleanly fit any of the above
    /// domains yet.
    ContentSignature,
    /// A serial/product/catalog code candidate read directly from the
    /// content itself (e.g. a PS1 `SYSTEM.CNF` boot executable filename, a
    /// Dreamcast `IP.BIN` product number). A *candidate* only: the same
    /// code family can span multiple platforms or reissues, and this kind
    /// carries no claim about which canonical release - or even which
    /// platform - the code belongs to. See
    /// [`crate::platform`]/[`crate::dat::identity`] for where that
    /// resolution actually happens.
    ProductCode,
}

impl ContentEvidenceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Container => "Container",
            Self::MediaClass => "Media class",
            Self::TapeFormat => "Tape format",
            Self::DiskFormat => "Disk format",
            Self::DiskEncoding => "Disk encoding",
            Self::Filesystem => "Filesystem",
            Self::BootStructure => "Boot structure",
            Self::ContentSignature => "Content signature",
            Self::ProductCode => "Product code",
        }
    }
}

/// How confidently one fact is known - about the *content*, never about a
/// canonical platform. See the module documentation for why this is a
/// deliberately separate dimension from
/// [`crate::dat::identity::DatPlatformConfidence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentEvidenceConfidence {
    /// Plausible, but not decisive on its own.
    Weak,
    /// Real evidence that agrees with something else, or a family-level
    /// convention rather than a fact proven down to one exact value.
    Corroborated,
    /// The fact is certain, within this domain - a valid parsed structure, a
    /// well-formed container header, an unambiguous field value.
    Strong,
}

impl ContentEvidenceConfidence {
    pub fn label(self) -> &'static str {
        match self {
            Self::Weak => "Weak",
            Self::Corroborated => "Corroborated",
            Self::Strong => "Strong",
        }
    }
}

/// One fact about a piece of content, independent of any platform.
///
/// `value` is deliberately a plain string rather than a per-kind enum: the
/// [`ContentEvidenceKind`] domain is what needs type safety, and a plain
/// value keeps this model open to new formats without a breaking enum
/// change. [`crate::content_evidence::value`] names a handful of well-known
/// values as constants for convenience and to avoid typos in callers and
/// tests; using an unlisted string is not an error; it is simply a fact this
/// module has no constant for yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentEvidence {
    pub kind: ContentEvidenceKind,
    pub value: String,
    pub confidence: ContentEvidenceConfidence,
    /// What was actually observed, in a person's words - this module's
    /// equivalent of provenance. Deliberately never a filesystem path: this
    /// model carries no notion of "where a file is," only what was observed
    /// about content a caller already has in hand.
    pub detail: String,
}

impl ContentEvidence {
    pub fn new(
        kind: ContentEvidenceKind,
        value: impl Into<String>,
        confidence: ContentEvidenceConfidence,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            value: value.into(),
            confidence,
            detail: detail.into(),
        }
    }
}

/// Well-known values for [`ContentEvidence::value`], grouped loosely by the
/// [`ContentEvidenceKind`] they're meant for. Not exhaustive, not enforced -
/// see [`ContentEvidence`]'s own documentation for why the value stays a
/// plain string. These exist so real callers and tests spell a fact the same
/// way rather than inventing slightly different strings for the same thing.
pub mod value {
    // Container
    pub const CHD: &str = "CHD";
    pub const ISO: &str = "ISO";
    pub const CUE_BIN: &str = "CueBin";
    pub const GDI: &str = "GDI";
    pub const ARCHIVE: &str = "Archive";

    // MediaClass
    pub const CD_ROM: &str = "CD-ROM";
    pub const GD_ROM: &str = "GD-ROM";
    pub const DVD: &str = "DVD";
    pub const HARD_DISK: &str = "HardDisk";
    pub const LASERDISC: &str = "LaserDisc";
    pub const FLOPPY: &str = "Floppy";
    pub const TAPE: &str = "Tape";
    pub const CARTRIDGE: &str = "Cartridge";

    // TapeFormat
    pub const TZX: &str = "TZX";
    pub const CDT: &str = "CDT";
    pub const COMMODORE_TAP: &str = "Commodore TAP";

    // DiskEncoding
    pub const MFM: &str = "MFM";
    pub const GCR: &str = "GCR";

    // Filesystem
    pub const FAT12: &str = "FAT12";
    pub const AMIGA_DOS: &str = "AmigaDOS";
    pub const ISO9660: &str = "ISO9660";
}

/// Every content fact gathered about one piece of content, deterministically
/// ordered and deduplicated - never resolved into a single answer.
///
/// This is intentionally the opposite shape from
/// [`crate::dat::identity::DatPlatformIdentity`]: that type picks one answer
/// (or reports `Ambiguous`/`Unknown`) because platform identity is something
/// a caller eventually needs a single decision about. Content evidence is
/// not - two detectors disagreeing about media class is itself useful
/// information, and this type keeps both facts visible rather than picking a
/// side. See [`ContentObservation::conflicting_kinds`] for a way to notice a
/// disagreement without either fact being dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentObservation {
    pub facts: Vec<ContentEvidence>,
}

impl ContentObservation {
    /// Every fact recorded for `kind`, in the same deterministic order as
    /// [`ContentObservation::facts`].
    pub fn facts_for(&self, kind: ContentEvidenceKind) -> impl Iterator<Item = &ContentEvidence> {
        self.facts.iter().filter(move |fact| fact.kind == kind)
    }

    /// The highest confidence recorded for `kind`, if any fact exists for it.
    pub fn strongest_confidence_for(
        &self,
        kind: ContentEvidenceKind,
    ) -> Option<ContentEvidenceConfidence> {
        self.facts_for(kind).map(|fact| fact.confidence).max()
    }

    /// Every domain where more than one distinct value shares that domain's
    /// highest recorded confidence - `MediaClass = CD-ROM` and `MediaClass =
    /// HardDisk` both at `Strong`, for example. Sorted and deduplicated.
    ///
    /// This never removes or alters anything in [`ContentObservation::facts`];
    /// it only names which domains are worth a caller's attention. A
    /// conflict here is not resolved by this module and never will be - it
    /// is reported so a later, explicitly reviewed decision can be made
    /// about it, the same way [`crate::dat::identity`] reports `Ambiguous`
    /// instead of guessing.
    pub fn conflicting_kinds(&self) -> Vec<ContentEvidenceKind> {
        let mut kinds: Vec<ContentEvidenceKind> = self.facts.iter().map(|fact| fact.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();

        kinds
            .into_iter()
            .filter(|kind| {
                let Some(strongest) = self.strongest_confidence_for(*kind) else {
                    return false;
                };
                let mut values_at_strongest: Vec<&str> = self
                    .facts_for(*kind)
                    .filter(|fact| fact.confidence == strongest)
                    .map(|fact| fact.value.as_str())
                    .collect();
                values_at_strongest.sort_unstable();
                values_at_strongest.dedup();
                values_at_strongest.len() > 1
            })
            .collect()
    }
}

/// Canonicalises a bag of gathered [`ContentEvidence`] into a deterministic
/// [`ContentObservation`].
///
/// Pure: no I/O, no ordering dependency on how the caller happened to gather
/// evidence. Sorts by domain, then strongest confidence first, then value,
/// then detail, so the result is stable regardless of insertion order.
/// Exact-duplicate facts (same kind, value, confidence, and detail) collapse
/// to one; a fact recorded at two different confidences for the same
/// kind/value is deliberately **not** collapsed - both occurrences are kept,
/// because losing either one would silently discard a detector's own
/// judgement about how sure it was. This function never resolves a
/// conflict, never drops a disagreeing fact, and never looks at
/// [`crate::platform::PLATFORMS`] or produces any platform-shaped value.
pub fn observe_content_evidence(
    evidence: impl IntoIterator<Item = ContentEvidence>,
) -> ContentObservation {
    let mut facts: Vec<ContentEvidence> = evidence.into_iter().collect();
    facts.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| right.confidence.cmp(&left.confidence))
            .then_with(|| left.value.cmp(&right.value))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    facts.dedup();
    ContentObservation { facts }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(
        kind: ContentEvidenceKind,
        value: &str,
        confidence: ContentEvidenceConfidence,
    ) -> ContentEvidence {
        ContentEvidence::new(kind, value, confidence, format!("test fact: {value}"))
    }

    #[test]
    fn strong_container_evidence_carries_no_platform_meaning() {
        // There is no platform field anywhere on `ContentEvidence` or
        // `ContentObservation` for this to even attempt - this test exists
        // to document that fact plainly, not to exercise any conversion.
        let observation = observe_content_evidence([fact(
            ContentEvidenceKind::Container,
            value::CHD,
            ContentEvidenceConfidence::Strong,
        )]);
        assert_eq!(observation.facts.len(), 1);
        assert_eq!(observation.facts[0].kind, ContentEvidenceKind::Container);
        assert_eq!(observation.facts[0].value, value::CHD);
    }

    #[test]
    fn strong_media_class_gd_rom_coexists_with_unknown_platform() {
        // "Coexists with unknown platform" here means exactly what it says:
        // nothing in this module or crate::dat::identity is invoked at all,
        // and the content fact stands on its own.
        let observation = observe_content_evidence([fact(
            ContentEvidenceKind::MediaClass,
            value::GD_ROM,
            ContentEvidenceConfidence::Strong,
        )]);
        assert_eq!(
            observation.strongest_confidence_for(ContentEvidenceKind::MediaClass),
            Some(ContentEvidenceConfidence::Strong)
        );
    }

    #[test]
    fn strong_media_class_hard_disk_coexists_with_unknown_platform() {
        let observation = observe_content_evidence([fact(
            ContentEvidenceKind::MediaClass,
            value::HARD_DISK,
            ContentEvidenceConfidence::Strong,
        )]);
        assert_eq!(
            observation.strongest_confidence_for(ContentEvidenceKind::MediaClass),
            Some(ContentEvidenceConfidence::Strong)
        );
    }

    #[test]
    fn strong_tape_format_tzx_coexists_with_unknown_platform() {
        let observation = observe_content_evidence([fact(
            ContentEvidenceKind::TapeFormat,
            value::TZX,
            ContentEvidenceConfidence::Strong,
        )]);
        assert_eq!(
            observation.strongest_confidence_for(ContentEvidenceKind::TapeFormat),
            Some(ContentEvidenceConfidence::Strong)
        );
    }

    #[test]
    fn independent_facts_coexist() {
        let observation = observe_content_evidence([
            fact(
                ContentEvidenceKind::Container,
                value::CHD,
                ContentEvidenceConfidence::Strong,
            ),
            fact(
                ContentEvidenceKind::MediaClass,
                value::CD_ROM,
                ContentEvidenceConfidence::Strong,
            ),
        ]);
        assert_eq!(observation.facts.len(), 2);
        assert!(
            observation
                .facts_for(ContentEvidenceKind::Container)
                .any(|f| f.value == value::CHD)
        );
        assert!(
            observation
                .facts_for(ContentEvidenceKind::MediaClass)
                .any(|f| f.value == value::CD_ROM)
        );
    }

    #[test]
    fn duplicate_identical_evidence_dedups_deterministically() {
        let one = fact(
            ContentEvidenceKind::Container,
            value::CHD,
            ContentEvidenceConfidence::Strong,
        );
        let observation = observe_content_evidence([one.clone(), one.clone(), one]);
        assert_eq!(observation.facts.len(), 1);
    }

    #[test]
    fn the_same_fact_at_two_confidences_is_never_collapsed() {
        // Explicit, chosen behaviour: a fact recorded Weak by one detector
        // and Strong by another for the same kind/value is not the same
        // struct, so both are kept. Losing either would silently discard a
        // detector's own judgement about how sure it was.
        let observation = observe_content_evidence([
            fact(
                ContentEvidenceKind::MediaClass,
                value::CD_ROM,
                ContentEvidenceConfidence::Weak,
            ),
            fact(
                ContentEvidenceKind::MediaClass,
                value::CD_ROM,
                ContentEvidenceConfidence::Strong,
            ),
        ]);
        assert_eq!(observation.facts.len(), 2);
        assert_eq!(
            observation.strongest_confidence_for(ContentEvidenceKind::MediaClass),
            Some(ContentEvidenceConfidence::Strong)
        );
        assert!(
            !observation
                .conflicting_kinds()
                .contains(&ContentEvidenceKind::MediaClass),
            "differing confidence for the same value is not a conflict - only differing values are"
        );
    }

    #[test]
    fn conflicting_media_facts_are_visible_not_resolved() {
        let observation = observe_content_evidence([
            fact(
                ContentEvidenceKind::MediaClass,
                value::CD_ROM,
                ContentEvidenceConfidence::Strong,
            ),
            fact(
                ContentEvidenceKind::MediaClass,
                value::HARD_DISK,
                ContentEvidenceConfidence::Strong,
            ),
        ]);
        // Both facts survive - nothing is silently dropped.
        assert_eq!(observation.facts.len(), 2);
        assert_eq!(
            observation.conflicting_kinds(),
            vec![ContentEvidenceKind::MediaClass]
        );
    }

    #[test]
    fn evidence_ordering_is_deterministic_regardless_of_insertion_order() {
        let a = fact(
            ContentEvidenceKind::Container,
            value::CHD,
            ContentEvidenceConfidence::Strong,
        );
        let b = fact(
            ContentEvidenceKind::MediaClass,
            value::GD_ROM,
            ContentEvidenceConfidence::Strong,
        );
        let c = fact(
            ContentEvidenceKind::TapeFormat,
            value::TZX,
            ContentEvidenceConfidence::Weak,
        );

        let forward = observe_content_evidence([a.clone(), b.clone(), c.clone()]);
        let reversed = observe_content_evidence([c, b, a]);
        assert_eq!(forward, reversed);
    }

    #[test]
    fn no_conversion_path_exists_to_dat_platform_evidence() {
        // There is no function in this module that takes a ContentEvidence
        // or ContentObservation and produces a
        // crate::dat::identity::DatPlatformEvidence - this test documents
        // that boundary. If such a function is ever added, it belongs in a
        // separately reviewed bridge, never an implicit conversion.
        let observation = observe_content_evidence([fact(
            ContentEvidenceKind::MediaClass,
            value::GD_ROM,
            ContentEvidenceConfidence::Strong,
        )]);
        // The only thing a caller can do with this is read it back - there
        // is no `.to_platform_evidence()`, `.into_dat_evidence()`, or
        // similar on either `ContentEvidence` or `ContentObservation`.
        assert_eq!(observation.facts.len(), 1);
    }
}
