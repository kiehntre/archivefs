//! Evidence *scope*: whether an observed [`ContentEvidence`] fact could ever
//! discriminate between platforms, independent of how reliably the fact
//! itself was observed.
//!
//! [`crate::content_evidence::ContentEvidenceConfidence`] already answers
//! "how sure are we this fact is true" (a validated ISO9660 PVD is `Strong`
//! evidence that the bytes really are ISO9660). It deliberately does **not**
//! answer a different question this module exists to answer instead: "does
//! this fact, even when perfectly true, actually narrow down which
//! platform this is." A `Strong` ISO9660 filesystem fact is completely
//! reliable and completely useless for telling a PS1 disc apart from a PC-FX
//! disc - conflating the two questions into one enum is exactly the mistake
//! [`crate::platform_evidence_fusion`] (this module's only consumer) is
//! built to avoid.
//!
//! # Three scopes, not two meanings crammed into `Strong`/`Weak`
//!
//! - [`EvidenceScope::Generic`]: shared across many unrelated platforms
//!   (`ISO9660`, `ELF`, a bare filename convention). Never a rule's
//!   defining leg.
//! - [`EvidenceScope::Family`]: narrows to a small, named group of related
//!   platforms, but not to one (`XDVDFS` -> original Xbox or Xbox 360;
//!   `TMR SEGA` -> Master System or Game Gear; `PS-X EXE` -> the PlayStation
//!   package/executable family).
//! - [`EvidenceScope::PlatformSpecific`]: narrows to exactly one canonical
//!   platform id (`SEGA SEGASATURN`, `XBEH`, `XEX2`).
//!
//! # This is a lookup catalog, not a resolver
//!
//! [`scope_of`] only classifies a `(kind, value)` pair; it never reads a
//! whole evidence bundle, never combines facts, and never produces a
//! platform decision. Combining facts into a decision is
//! [`crate::platform_evidence_fusion`]'s job. This module's only role there
//! is a consistency check: every [`crate::platform_evidence_fusion::FusionRule`]
//! leg that is expected to discriminate a platform must resolve to
//! [`EvidenceScope::Family`] or [`EvidenceScope::PlatformSpecific`] here,
//! never [`EvidenceScope::Generic`] - enforced by
//! `platform_evidence_fusion`'s own test suite, not by this module (which
//! has no notion of "rules" at all).

use crate::content_evidence::ContentEvidenceKind;

/// Whether an observed fact could ever discriminate between platforms - see
/// the module documentation for the full rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceScope {
    /// Shared across many unrelated platforms/formats.
    Generic,
    /// Narrows to a small, named group of related platforms.
    Family(&'static str),
    /// Narrows to exactly one canonical platform id (from
    /// [`crate::platform::PLATFORMS`]).
    PlatformSpecific(&'static str),
}

impl EvidenceScope {
    pub fn is_generic(self) -> bool {
        matches!(self, Self::Generic)
    }

    pub fn is_discriminating(self) -> bool {
        !self.is_generic()
    }
}

/// One `(kind, exact value)` -> scope entry. `value` matches the fact's
/// `ContentEvidence::value` field exactly (case-sensitive, matching every
/// real detector's own emitted string) - see the module-level catalog for
/// every entry, each cited against the emitting module.
struct ScopeEntry {
    kind: ContentEvidenceKind,
    value: &'static str,
    scope: EvidenceScope,
}

use ContentEvidenceKind::{BootStructure, Container, ContentSignature, Filesystem, MediaClass};

/// Every known `(kind, value)` pair this crate's detectors actually emit,
/// and its scope - verified against each emitting module's own source
/// (see the inline citations). Not exhaustive of every possible future
/// detector output; [`scope_of`] falls back to
/// [`EvidenceScope::Generic`] for anything not listed here, which is the
/// conservative default (an unclassified fact can never discriminate a
/// platform in [`crate::platform_evidence_fusion`] either).
const SCOPE_CATALOG: &[ScopeEntry] = &[
    // -- Generic: shared across many unrelated formats/platforms --
    ScopeEntry {
        kind: Filesystem,
        value: "ISO9660",
        scope: EvidenceScope::Generic,
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "ELF",
        scope: EvidenceScope::Generic,
    },
    // executable_signatures.rs: a bare filename convention, not a format
    // signature - see xbox_boot_evidence.rs/xbox360_boot_evidence.rs docs.
    ScopeEntry {
        kind: BootStructure,
        value: "default.xbe",
        scope: EvidenceScope::Generic,
    },
    ScopeEntry {
        kind: BootStructure,
        value: "default.xex",
        scope: EvidenceScope::Generic,
    },
    ScopeEntry {
        kind: MediaClass,
        value: "CD-ROM",
        scope: EvidenceScope::Generic,
    },
    ScopeEntry {
        kind: MediaClass,
        value: "DVD",
        scope: EvidenceScope::Generic,
    },
    ScopeEntry {
        kind: Container,
        value: "CHD",
        scope: EvidenceScope::Generic,
    },
    ScopeEntry {
        kind: Container,
        value: "ISO",
        scope: EvidenceScope::Generic,
    },
    ScopeEntry {
        kind: Container,
        value: "CueBin",
        scope: EvidenceScope::Generic,
    },
    // -- Family: narrows to a small named group, not one platform --
    // xbox_boot_evidence.rs / xbox360_boot_evidence.rs: shared verbatim.
    ScopeEntry {
        kind: Filesystem,
        value: "XDVDFS",
        scope: EvidenceScope::Family("Xbox"),
    },
    // sms_gg_header_evidence.rs: identical header format on both systems.
    ScopeEntry {
        kind: BootStructure,
        value: "TMR SEGA",
        scope: EvidenceScope::Family("Sega 8-bit"),
    },
    // playstation_boot_evidence.rs: PS-X EXE is a PlayStation-ecosystem
    // executable signature, not proof of PS1 specifically on its own -
    // matches the milestone's own worked example.
    ScopeEntry {
        kind: ContentSignature,
        value: "PS-X EXE",
        scope: EvidenceScope::Family("PlayStation"),
    },
    // playstation_boot_evidence.rs: the SYSTEM.CNF boot_key itself; "BOOT"
    // (PS1) is shared vocabulary with the wider Sony optical family.
    ScopeEntry {
        kind: BootStructure,
        value: "BOOT",
        scope: EvidenceScope::Family("PlayStation"),
    },
    // psp_boot_evidence.rs / ps3_boot_evidence.rs: conventional directory
    // markers shared across the whole Sony digital-package ecosystem.
    ScopeEntry {
        kind: BootStructure,
        value: "PSP_GAME",
        scope: EvidenceScope::Family("Sony optical/package"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "PS3_GAME",
        scope: EvidenceScope::Family("Sony optical/package"),
    },
    // xbox360_stfs_evidence.rs: a signing-chain fact, not a content-class
    // one - see that module's own collision policy documentation.
    ScopeEntry {
        kind: ContentSignature,
        value: "CON",
        scope: EvidenceScope::Family("Xbox360"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "LIVE",
        scope: EvidenceScope::Family("Xbox360"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "PIRS",
        scope: EvidenceScope::Family("Xbox360"),
    },
    // ps3_disc_evidence.rs: a Sony package format, not necessarily a PS3
    // game (could be DLC, a trial, etc.) - see that module's own docs.
    ScopeEntry {
        kind: ContentSignature,
        value: "PKG",
        scope: EvidenceScope::Family("PlayStation"),
    },
    ScopeEntry {
        kind: Container,
        value: "PBP",
        scope: EvidenceScope::Family("PlayStation"),
    },
    // gamecube_wii_boot_evidence.rs: shared between GameCube and Wii.
    ScopeEntry {
        kind: BootStructure,
        value: "main.dol",
        scope: EvidenceScope::Family("GameCube/Wii"),
    },
    // -- PlatformSpecific: narrows to exactly one canonical platform --
    ScopeEntry {
        kind: BootStructure,
        value: "SEGA SEGASATURN",
        scope: EvidenceScope::PlatformSpecific("Saturn"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "SEGA SEGAKATANA",
        scope: EvidenceScope::PlatformSpecific("Dreamcast"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "SEGA SEGAMARIO",
        scope: EvidenceScope::PlatformSpecific("Dreamcast"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "SEGADISCSYSTEM",
        scope: EvidenceScope::PlatformSpecific("Sega CD"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "OperaFS",
        scope: EvidenceScope::PlatformSpecific("3DO"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "XBEH",
        scope: EvidenceScope::PlatformSpecific("Xbox"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "XEX2",
        scope: EvidenceScope::PlatformSpecific("Xbox360"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "SELF",
        scope: EvidenceScope::PlatformSpecific("PS3"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "BOOT2",
        scope: EvidenceScope::PlatformSpecific("PS2"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "GameCube",
        scope: EvidenceScope::PlatformSpecific("GameCube"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "Wii",
        scope: EvidenceScope::PlatformSpecific("Wii"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "LYNX",
        scope: EvidenceScope::PlatformSpecific("Atari Lynx"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "ATARI7800",
        scope: EvidenceScope::PlatformSpecific("Atari7800"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "iNES",
        scope: EvidenceScope::PlatformSpecific("NES"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "NES 2.0",
        scope: EvidenceScope::PlatformSpecific("NES"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "LoROM",
        scope: EvidenceScope::PlatformSpecific("SNES"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "HiROM",
        scope: EvidenceScope::PlatformSpecific("SNES"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "ExHiROM",
        scope: EvidenceScope::PlatformSpecific("SNES"),
    },
    ScopeEntry {
        kind: ContentSignature,
        value: "z64",
        scope: EvidenceScope::PlatformSpecific("N64"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "Nintendo Game Boy logo",
        scope: EvidenceScope::PlatformSpecific("Game Boy"),
    },
    ScopeEntry {
        kind: BootStructure,
        value: "GBA cartridge header",
        scope: EvidenceScope::PlatformSpecific("Game Boy Advance"),
    },
];

/// Classifies `(kind, value)` by exact match against [`SCOPE_CATALOG`].
/// Falls back to [`EvidenceScope::Generic`] for anything not listed -
/// the conservative default, since an unclassified fact can never
/// discriminate a platform in [`crate::platform_evidence_fusion`] either.
pub fn scope_of(kind: ContentEvidenceKind, value: &str) -> EvidenceScope {
    // ProductCode is never in the catalog: its value is a dynamic
    // per-title candidate identifier (a serial number, a title ID) that
    // cannot be exact-matched by any static table entry, and - by this
    // crate's own long-standing discipline (see every `*_boot_evidence`
    // module's "not verified against a canonical release list" wording) -
    // it never proves a platform on its own regardless of its literal
    // value, so it is always Generic here.
    if kind == ContentEvidenceKind::ProductCode {
        return EvidenceScope::Generic;
    }
    SCOPE_CATALOG
        .iter()
        .find(|entry| entry.kind == kind && entry.value == value)
        .map_or(EvidenceScope::Generic, |entry| entry.scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_evidence::ContentEvidenceKind::*;

    #[test]
    fn iso9660_alone_is_generic() {
        assert_eq!(scope_of(Filesystem, "ISO9660"), EvidenceScope::Generic);
    }

    #[test]
    fn elf_alone_is_generic() {
        assert_eq!(scope_of(ContentSignature, "ELF"), EvidenceScope::Generic);
    }

    #[test]
    fn xdvdfs_is_family_xbox_not_platform_specific() {
        let scope = scope_of(Filesystem, "XDVDFS");
        assert!(matches!(scope, EvidenceScope::Family("Xbox")));
        assert!(!matches!(scope, EvidenceScope::PlatformSpecific(_)));
    }

    #[test]
    fn tmr_sega_is_family_not_platform_specific() {
        assert!(matches!(
            scope_of(BootStructure, "TMR SEGA"),
            EvidenceScope::Family(_)
        ));
    }

    #[test]
    fn saturn_boot_signature_is_platform_specific() {
        assert_eq!(
            scope_of(BootStructure, "SEGA SEGASATURN"),
            EvidenceScope::PlatformSpecific("Saturn")
        );
    }

    #[test]
    fn xbeh_is_platform_specific_xbox() {
        assert_eq!(
            scope_of(ContentSignature, "XBEH"),
            EvidenceScope::PlatformSpecific("Xbox")
        );
    }

    #[test]
    fn xex2_is_platform_specific_xbox360_distinct_from_xbeh() {
        assert_eq!(
            scope_of(ContentSignature, "XEX2"),
            EvidenceScope::PlatformSpecific("Xbox360")
        );
        assert_ne!(
            scope_of(ContentSignature, "XEX2"),
            scope_of(ContentSignature, "XBEH")
        );
    }

    #[test]
    fn ps_x_exe_is_family_playstation() {
        assert_eq!(
            scope_of(ContentSignature, "PS-X EXE"),
            EvidenceScope::Family("PlayStation")
        );
    }

    #[test]
    fn self_magic_is_platform_specific_ps3() {
        assert_eq!(
            scope_of(ContentSignature, "SELF"),
            EvidenceScope::PlatformSpecific("PS3")
        );
    }

    #[test]
    fn boot2_is_platform_specific_ps2_distinct_from_boot() {
        assert_eq!(
            scope_of(BootStructure, "BOOT2"),
            EvidenceScope::PlatformSpecific("PS2")
        );
        assert_eq!(
            scope_of(BootStructure, "BOOT"),
            EvidenceScope::Family("PlayStation")
        );
    }

    #[test]
    fn product_code_is_always_generic_regardless_of_value() {
        assert_eq!(scope_of(ProductCode, "SLUS-00594"), EvidenceScope::Generic);
        assert_eq!(
            scope_of(ProductCode, "SEGA SEGASATURN"),
            EvidenceScope::Generic
        );
        assert_eq!(scope_of(ProductCode, ""), EvidenceScope::Generic);
    }

    #[test]
    fn unknown_value_falls_back_to_generic() {
        assert_eq!(
            scope_of(BootStructure, "totally unrecognised string"),
            EvidenceScope::Generic
        );
    }

    #[test]
    fn is_generic_and_is_discriminating_are_opposite() {
        assert!(EvidenceScope::Generic.is_generic());
        assert!(!EvidenceScope::Generic.is_discriminating());
        assert!(!EvidenceScope::Family("x").is_generic());
        assert!(EvidenceScope::Family("x").is_discriminating());
        assert!(!EvidenceScope::PlatformSpecific("x").is_generic());
        assert!(EvidenceScope::PlatformSpecific("x").is_discriminating());
    }

    #[test]
    fn gamecube_and_wii_are_distinct_platform_specific_scopes() {
        assert_eq!(
            scope_of(BootStructure, "GameCube"),
            EvidenceScope::PlatformSpecific("GameCube")
        );
        assert_eq!(
            scope_of(BootStructure, "Wii"),
            EvidenceScope::PlatformSpecific("Wii")
        );
        assert_ne!(
            scope_of(BootStructure, "GameCube"),
            scope_of(BootStructure, "Wii")
        );
    }

    #[test]
    fn main_dol_is_family_not_platform_specific() {
        assert!(matches!(
            scope_of(BootStructure, "main.dol"),
            EvidenceScope::Family(_)
        ));
    }

    #[test]
    fn stfs_signing_variants_are_family_xbox360_not_platform_specific() {
        for value in ["CON", "LIVE", "PIRS"] {
            assert!(
                matches!(scope_of(ContentSignature, value), EvidenceScope::Family(_)),
                "{value} should be Family scope"
            );
        }
    }

    #[test]
    fn nes_content_signatures_share_platform_specific_scope() {
        assert_eq!(
            scope_of(ContentSignature, "iNES"),
            scope_of(ContentSignature, "NES 2.0")
        );
    }

    #[test]
    fn snes_map_modes_share_platform_specific_scope() {
        let lorom = scope_of(ContentSignature, "LoROM");
        let hirom = scope_of(ContentSignature, "HiROM");
        let exhirom = scope_of(ContentSignature, "ExHiROM");
        assert_eq!(lorom, hirom);
        assert_eq!(hirom, exhirom);
        assert_eq!(lorom, EvidenceScope::PlatformSpecific("SNES"));
    }

    #[test]
    fn default_xbe_filename_convention_is_generic() {
        assert_eq!(
            scope_of(BootStructure, "default.xbe"),
            EvidenceScope::Generic
        );
    }

    #[test]
    fn scope_lookup_is_deterministic() {
        assert_eq!(
            scope_of(BootStructure, "SEGA SEGASATURN"),
            scope_of(BootStructure, "SEGA SEGASATURN")
        );
    }
}
