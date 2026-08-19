//! Pure, read-only PlayStation 2 boot evidence: `SYSTEM.CNF` (`BOOT2=`) and
//! a bounded ELF magic check.
//!
//! Almost entirely a thin wrapper: [`crate::playstation_boot_evidence::parse_system_cnf_boot`]
//! already recognizes `BOOT2=` (PS2) exactly as it does `BOOT=` (PS1) - one
//! parser, two keys, no duplication - and [`crate::executable_signatures::ElfDetector`]
//! already provides the generic ELF magic check PS2 executables need. This
//! module exists to combine those two already-reviewed pieces into
//! PS2-shaped evidence (`BOOT2` specifically, not `BOOT`) without
//! reimplementing either.
//!
//! # Collision safety
//!
//! - `SYSTEM.CNF`/`BOOT2=` is not PS2-exclusive proof - see
//!   [`crate::playstation_boot_evidence`]'s own collision notes; the same
//!   file/key convention exists on PS1 (`BOOT=`) and this module only
//!   differs by which key it looks for.
//! - ELF is a generic, cross-platform executable format (also used
//!   outside PlayStation entirely) - [`crate::executable_signatures::ElfDetector`]
//!   emits it at `Weak` confidence for exactly this reason, and this
//!   module never upgrades that confidence on its own.
//! - A `BOOT2=` line *plus* a valid ELF magic together are still only two
//!   independent legs, combined here into one [`Ps2BootObservation`] for a
//!   caller's convenience - not promoted to platform truth. That decision
//!   belongs to a resolver this module has no connection to.

use crate::content_evidence::ContentEvidence;
use crate::executable_signatures::looks_like_elf;
use crate::playstation_boot_evidence::{
    SystemCnfBootFact, observe_system_cnf_evidence, parse_system_cnf_boot,
};

/// What was observed about a PS2-style disc's `SYSTEM.CNF`/executable,
/// combined for convenience - never a platform decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ps2BootObservation {
    pub system_cnf: SystemCnfBootFact,
    /// Whether the executable named by `system_cnf.executable_path` began
    /// with the ELF magic, when a caller supplied its header bytes to
    /// [`observe_ps2_boot`]. `None` when no header was supplied at all
    /// (the caller never located/read the executable).
    pub elf_magic_present: Option<bool>,
}

/// Combines an already-parsed `SYSTEM.CNF` fact with an optional,
/// separately-read executable header. This function does no filesystem
/// lookup itself - a caller (e.g. `disc_probe`) locates `SYSTEM.CNF` and
/// the executable it names via [`crate::iso9660::find_path`] first, exactly
/// as [`crate::playstation_boot_evidence`]'s own callers already do.
pub fn observe_ps2_boot(
    system_cnf: SystemCnfBootFact,
    executable_header: Option<&[u8]>,
) -> Ps2BootObservation {
    let elf_magic_present = executable_header.map(looks_like_elf);
    Ps2BootObservation {
        system_cnf,
        elf_magic_present,
    }
}

/// Only recognises a `BOOT2=` key specifically - a `BOOT=` (PS1) fact
/// parsed from the same `SYSTEM.CNF` bytes is not PS2 evidence and is
/// filtered out here, even though [`parse_system_cnf_boot`] itself is
/// key-agnostic.
pub fn parse_ps2_system_cnf(bytes: &[u8]) -> Option<SystemCnfBootFact> {
    let fact = parse_system_cnf_boot(bytes)?;
    (fact.boot_key == "BOOT2").then_some(fact)
}

/// Neutral evidence for a [`Ps2BootObservation`]: the same
/// `BootStructure`/`ProductCode` facts [`observe_system_cnf_evidence`]
/// already produces for the `SYSTEM.CNF` side, plus a `Weak` ELF
/// `ContentSignature` fact when the executable header was checked and
/// matched - reusing [`crate::executable_signatures::ElfDetector`]'s own
/// evidence shape rather than inventing a second one.
pub fn observe_ps2_evidence(observation: &Ps2BootObservation) -> Vec<ContentEvidence> {
    let mut evidence = observe_system_cnf_evidence(&observation.system_cnf);
    if observation.elf_magic_present == Some(true) {
        evidence.push(ContentEvidence::new(
            crate::content_evidence::ContentEvidenceKind::ContentSignature,
            "ELF",
            crate::content_evidence::ContentEvidenceConfidence::Weak,
            "ELF magic present in the BOOT2 executable - a generic executable-format signature, not platform evidence on its own",
        ));
    }
    evidence
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_evidence::{ContentEvidenceConfidence, ContentEvidenceKind};

    #[test]
    fn boot2_line_is_recognized() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLES_123.45;1\n").unwrap();
        assert_eq!(fact.boot_key, "BOOT2");
        assert_eq!(fact.executable_path.as_deref(), Some("SLES_123.45"));
    }

    #[test]
    fn boot1_line_is_not_ps2_evidence() {
        // A well-formed BOOT= (PS1) line must not be misreported as PS2.
        assert_eq!(parse_ps2_system_cnf(b"BOOT=cdrom:\\SLUS_014.18;1\n"), None);
    }

    #[test]
    fn serial_is_extracted_for_ps2_family() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLUS_205.20;1\n").unwrap();
        assert_eq!(fact.serial_candidate.as_deref(), Some("SLUS-20520"));
    }

    #[test]
    fn unknown_serial_shape_is_not_promoted() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SYSTEM.ELF;1\n").unwrap();
        assert_eq!(fact.serial_candidate, None);
    }

    #[test]
    fn elf_magic_is_observed_when_header_supplied() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLES_123.45;1\n").unwrap();
        let header = [0x7f, b'E', b'L', b'F', 1, 2, 3, 4];
        let observation = observe_ps2_boot(fact, Some(&header));
        assert_eq!(observation.elf_magic_present, Some(true));
    }

    #[test]
    fn missing_executable_header_is_none_not_false() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLES_123.45;1\n").unwrap();
        let observation = observe_ps2_boot(fact, None);
        assert_eq!(observation.elf_magic_present, None);
    }

    #[test]
    fn non_elf_header_is_observed_as_false() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLES_123.45;1\n").unwrap();
        let observation = observe_ps2_boot(fact, Some(b"not an elf"));
        assert_eq!(observation.elf_magic_present, Some(false));
    }

    #[test]
    fn elf_evidence_is_weak_not_strong() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLES_123.45;1\n").unwrap();
        let header = [0x7f, b'E', b'L', b'F'];
        let observation = observe_ps2_boot(fact, Some(&header));
        let evidence = observe_ps2_evidence(&observation);
        let elf = evidence.iter().find(|item| item.value == "ELF").unwrap();
        assert_eq!(elf.confidence, ContentEvidenceConfidence::Weak);
    }

    #[test]
    fn evidence_never_assigns_a_platform() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLES_123.45;1\n").unwrap();
        let header = [0x7f, b'E', b'L', b'F'];
        let observation = observe_ps2_boot(fact, Some(&header));
        for item in observe_ps2_evidence(&observation) {
            assert!(matches!(
                item.kind,
                ContentEvidenceKind::BootStructure
                    | ContentEvidenceKind::ProductCode
                    | ContentEvidenceKind::ContentSignature
            ));
        }
    }

    #[test]
    fn repeated_observation_is_deterministic() {
        let fact = parse_ps2_system_cnf(b"BOOT2=cdrom0:\\SLES_123.45;1\n").unwrap();
        let a = observe_ps2_boot(fact.clone(), None);
        let b = observe_ps2_boot(fact, None);
        assert_eq!(a, b);
    }
}
