//! The first conservative, explainable content-evidence fusion layer.
//!
//! ```text
//! DETECTOR OBSERVES
//!     -> crate::content_evidence::ContentEvidence (already exists)
//!     -> FUSION groups/weighs/compares (this module)
//!     -> ResolutionExplanation / FusionOutcome
//!     -> a later, separately reviewed layer may act
//! ```
//!
//! This module never renames, moves, deletes, or authorizes any action. It
//! turns a bag of [`ContentEvidence`] facts (already produced by this
//! crate's existing `*_boot_evidence`/`*_header_evidence`/digital-package
//! modules - nothing here parses bytes itself) into one of four outcomes -
//! see [`FusionOutcome`] - plus a structured explanation of exactly why.
//!
//! # Why this is not [`crate::platform::identity`]
//!
//! [`crate::platform::identity::resolve_platform_identity`] already fuses
//! evidence, but at a different, downstream layer: its inputs
//! ([`crate::platform::identity::PlatformIdentityEvidence`]) are
//! *already-decided platform strings* from external providers (a RomM
//! match, a verified DAT audit, a manual assignment) - the "was this file
//! ever a Saturn ROM" question has already been answered by the time that
//! resolver sees anything. This module answers that question in the first
//! place, from raw structural facts (`"SEGA SEGASATURN"`, `"XDVDFS"`,
//! `"XBEH"`, ...) that never carry a platform label on their own - see
//! [`crate::content_evidence`]'s own module documentation for why. A
//! [`Resolved`](FusionOutcome::Resolved) [`ResolutionExplanation`] from this
//! module is exactly the kind of fact a caller could offer to
//! `PlatformIdentityEvidence` at `PlatformIdentitySource::Inference` -
//! bridging the two layers explicitly, rather than merging their separate
//! confidence vocabularies into one.
//!
//! # Rule-based, not score-based
//!
//! [`RULES`] is a fixed table of explicit, reviewed combinations - "XDVDFS +
//! XBEH => Xbox," never "Xbox = 37 points." See [`FusionRule`] and the
//! module-level rule catalog for every combination this milestone reviewed,
//! and the module documentation's own worked examples for why: opaque
//! numeric scoring hides exactly the reasoning this milestone exists to
//! surface.
//!
//! # The confidence/scope split
//!
//! [`crate::content_evidence::ContentEvidenceConfidence`] says how reliably
//! a fact was observed; [`crate::content_evidence_scope::EvidenceScope`]
//! (a separate module) says whether that fact could ever discriminate a
//! platform at all. A rule's legs are expressed in terms of confidence
//! (`RequiredFact::min_confidence`) because that is what a real detector
//! actually reports; [`content_evidence_scope`]'s own test suite
//! cross-checks that every fact this module's rules treat as
//! platform-discriminating is not accidentally
//! [`crate::content_evidence_scope::EvidenceScope::Generic`] - see
//! `tests::every_exact_rule_leg_is_at_least_family_scoped`.

use crate::content_evidence::{ContentEvidence, ContentEvidenceConfidence, ContentEvidenceKind};
use crate::platform::equivalent_platform_ids;

/// One fact a [`FusionRule`] requires to be present in the evidence bundle
/// before the rule is considered satisfied.
#[derive(Debug, Clone, Copy)]
pub enum RequiredFact {
    /// A fact of `kind` whose value matches `value` exactly, observed at
    /// `min_confidence` or higher.
    Exact {
        kind: ContentEvidenceKind,
        value: &'static str,
        min_confidence: ContentEvidenceConfidence,
    },
    /// A fact of `kind` whose value *starts with* `prefix`, observed at
    /// `min_confidence` or higher - for facts whose exact value is
    /// dynamic (a Mega Drive console-name field, a Game Gear/Master
    /// System region label) but whose fixed prefix is what a rule
    /// actually cares about.
    ValuePrefix {
        kind: ContentEvidenceKind,
        prefix: &'static str,
        min_confidence: ContentEvidenceConfidence,
    },
    /// Any fact of `kind`, any value, observed at `min_confidence` or
    /// higher - for facts whose value is a genuinely per-title candidate
    /// (a serial number, a title ID) that no rule could reasonably pin to
    /// one literal string.
    AnyOfKind {
        kind: ContentEvidenceKind,
        min_confidence: ContentEvidenceConfidence,
    },
}

impl RequiredFact {
    fn min_confidence(&self) -> ContentEvidenceConfidence {
        match self {
            Self::Exact { min_confidence, .. }
            | Self::ValuePrefix { min_confidence, .. }
            | Self::AnyOfKind { min_confidence, .. } => *min_confidence,
        }
    }

    fn kind(&self) -> ContentEvidenceKind {
        match self {
            Self::Exact { kind, .. }
            | Self::ValuePrefix { kind, .. }
            | Self::AnyOfKind { kind, .. } => *kind,
        }
    }

    /// Whether at least one fact in `facts` satisfies this requirement.
    /// Order-independent and idempotent under duplicate/reordered facts -
    /// both are real inputs this crate's detectors can produce (see
    /// [`crate::content_evidence::observe_content_evidence`]'s own
    /// dedup/order-independence guarantees, which this function inherits
    /// by construction rather than re-deriving).
    fn satisfied_by(&self, facts: &[ContentEvidence]) -> bool {
        facts.iter().any(|fact| {
            if fact.kind != self.kind() || fact.confidence < self.min_confidence() {
                return false;
            }
            match self {
                Self::Exact { value, .. } => fact.value == *value,
                Self::ValuePrefix { prefix, .. } => fact.value.starts_with(prefix),
                Self::AnyOfKind { .. } => true,
            }
        })
    }
}

/// One explicit, reviewed platform-evidence combination - see the module
/// documentation's "rule-based, not score-based" section.
#[derive(Debug, Clone, Copy)]
pub struct FusionRule {
    /// A stable identifier, safe to log/persist.
    pub id: &'static str,
    /// The canonical platform id this rule concludes - checked against
    /// [`crate::platform::PLATFORMS`] by this module's own test suite.
    pub platform: &'static str,
    /// Every fact that must be present for this rule to fire. All legs are
    /// required (a strict AND) - there is no partial credit.
    pub legs: &'static [RequiredFact],
    pub explanation: &'static str,
}

impl FusionRule {
    fn is_satisfied(&self, facts: &[ContentEvidence]) -> bool {
        self.legs.iter().all(|leg| leg.satisfied_by(facts))
    }

    /// Whether at least one leg demands [`ContentEvidenceConfidence::Strong`],
    /// the "reviewed strong leg" that [`crate::content_evidence::ContentEvidenceConfidence`]'s
    /// own documentation and this milestone both require before a rule can
    /// ever produce [`FusionOutcome::Resolved`]. A rule with no such leg can
    /// still fire (all its Corroborated/Weak-tier legs satisfied) and
    /// register as a candidate - see [`FusionOutcome::Ambiguous`].
    pub fn has_strong_leg(&self) -> bool {
        self.legs
            .iter()
            .any(|leg| leg.min_confidence() == ContentEvidenceConfidence::Strong)
    }
}

const STRONG: ContentEvidenceConfidence = ContentEvidenceConfidence::Strong;
const CORROBORATED: ContentEvidenceConfidence = ContentEvidenceConfidence::Corroborated;
const WEAK: ContentEvidenceConfidence = ContentEvidenceConfidence::Weak;

use ContentEvidenceKind::{BootStructure, ContentSignature, Filesystem, ProductCode};
use RequiredFact::{AnyOfKind, Exact, ValuePrefix};

/// The reviewed rule catalog. Every rule here was checked against a real
/// emitting module's own documented output (see each rule's `explanation`
/// and this module's doc comment); nothing here is a numeric weight.
///
/// # Platforms deliberately *not* given a Strong-eligible rule
///
/// - **PS2** (as of Batch 6, PS2 *does* have a Strong-eligible rule -
///   `ps2_system_cnf_boot2_strong` - see [`crate::ps2_boot_evidence`]'s own
///   doc comment for the full justification: `BOOT2` is upgraded to
///   `Strong` only once the executable it names is independently confirmed
///   to be a valid ELF, never from the text token alone. The plain ELF
///   `ContentSignature` fact itself is still always `Weak` and never the
///   discriminating leg - it remains true, as this milestone requires,
///   that "generic ELF" is never promoted). The older
///   `ps2_system_cnf_boot2_candidate` rule stays, unchanged, as the
///   fallback for callers that could not confirm the executable header.
/// - **PSP** (as of Batch 6, PSP *does* have a Strong-eligible rule -
///   `psp_umd_data_bin_strong` - see [`crate::psp_boot_evidence`]'s own doc
///   comment: `UMD_DATA.BIN`'s presence, not `PSP_GAME`/`PARAM.SFO`/
///   `EBOOT.BIN` alone, is the platform-specific leg. The older
///   `psp_layout_candidate` rule stays as the fallback for a dump missing
///   the physical-medium file).
/// - **Mega Drive / 32X**: [`crate::megadrive_header_evidence`] emits its
///   console-name fact at `Corroborated` only (matching this crate's
///   existing `MegaDrive` platform registry entry, itself `Corroborated`
///   "the header alone is the only proof"); [`crate::sega32x_header_evidence`]'s
///   `"32X"` leg is `Weak` by its own documented design. Candidate-only.
/// - **STFS alone**: deliberately has **no** rule at all. Per this
///   milestone's own instruction ("STFS alone: Xbox 360 family digital
///   content, NOT automatically a game" - do not confuse package platform
///   with game-content identity), a bare STFS envelope produces
///   [`FusionOutcome::Unknown`], not even a candidate.
/// - **PKG alone**: same reasoning, same outcome - no rule keys off
///   `ContentSignature = "PKG"` by itself; the PS3 rule below requires the
///   full `PS3_GAME` + `SELF` + `PARAM.SFO`-derived product-code combo.
/// - **PC-FX**: [`crate::pcfx_boot_evidence`] emits real, `Strong`
///   `BootStructure` evidence, but no canonical platform id for PC-FX
///   exists in [`crate::platform::PLATFORMS`] today - a genuine registry
///   gap, out of this module's scope to fix (see the crate-level milestone
///   report). No rule references it; PC-FX evidence always resolves
///   [`FusionOutcome::Unknown`] through this module today.
pub const RULES: &[FusionRule] = &[
    // -- Sega optical: single-leg, each already Strong+platform-specific --
    FusionRule {
        id: "saturn_boot_signature",
        platform: "Saturn",
        legs: &[Exact {
            kind: BootStructure,
            value: "SEGA SEGASATURN",
            min_confidence: STRONG,
        }],
        explanation: "SEGA SEGASATURN header magic is Saturn-specific strong evidence on its own",
    },
    FusionRule {
        id: "dreamcast_boot_signature_katana",
        platform: "Dreamcast",
        legs: &[Exact {
            kind: BootStructure,
            value: "SEGA SEGAKATANA",
            min_confidence: STRONG,
        }],
        explanation: "SEGA SEGAKATANA IP.BIN hardware ID is Dreamcast-specific strong evidence",
    },
    FusionRule {
        id: "dreamcast_boot_signature_mario",
        platform: "Dreamcast",
        legs: &[Exact {
            kind: BootStructure,
            value: "SEGA SEGAMARIO",
            min_confidence: STRONG,
        }],
        explanation: "SEGA SEGAMARIO IP.BIN hardware ID is Dreamcast-specific strong evidence",
    },
    FusionRule {
        id: "segacd_boot_signature",
        platform: "Sega CD",
        legs: &[Exact {
            kind: BootStructure,
            value: "SEGADISCSYSTEM",
            min_confidence: STRONG,
        }],
        explanation: "SEGADISCSYSTEM volume header is Sega CD/Mega-CD-specific strong evidence",
    },
    FusionRule {
        id: "threedo_opera_header",
        platform: "3DO",
        legs: &[Exact {
            kind: BootStructure,
            value: "OperaFS",
            min_confidence: STRONG,
        }],
        explanation: "a validated Opera filesystem volume header is 3DO-specific strong evidence",
    },
    // -- Sega 8-bit family: magic alone is Family-scope; region-confirmed
    //    resolves to one of the two systems it names --
    FusionRule {
        id: "master_system_region_confirmed",
        platform: "MasterSystem",
        legs: &[
            Exact {
                kind: BootStructure,
                value: "TMR SEGA",
                min_confidence: STRONG,
            },
            ValuePrefix {
                kind: ContentSignature,
                prefix: "Master System",
                min_confidence: CORROBORATED,
            },
        ],
        explanation: "TMR SEGA magic plus a verified Master System region/system nibble",
    },
    FusionRule {
        id: "game_gear_region_confirmed",
        platform: "GameGear",
        legs: &[
            Exact {
                kind: BootStructure,
                value: "TMR SEGA",
                min_confidence: STRONG,
            },
            ValuePrefix {
                kind: ContentSignature,
                prefix: "Game Gear",
                min_confidence: CORROBORATED,
            },
        ],
        explanation: "TMR SEGA magic plus a verified Game Gear region/system nibble",
    },
    // -- Mega Drive / 32X: candidate-only (see the const-level doc comment) --
    FusionRule {
        id: "megadrive_console_name",
        platform: "MegaDrive",
        legs: &[ValuePrefix {
            kind: BootStructure,
            prefix: "SEGA",
            min_confidence: CORROBORATED,
        }],
        explanation: "Mega Drive/Genesis console-name field begins with SEGA (candidate only, never Strong)",
    },
    FusionRule {
        id: "sega32x_console_name_leg",
        platform: "Sega 32X",
        legs: &[
            ValuePrefix {
                kind: BootStructure,
                prefix: "SEGA",
                min_confidence: CORROBORATED,
            },
            Exact {
                kind: ContentSignature,
                value: "32X",
                min_confidence: WEAK,
            },
        ],
        explanation: "base Mega Drive header plus the \"32X\" console-name hint (candidate only, never Strong)",
    },
    // -- Nintendo cartridges: single-leg, each already Strong+platform-specific --
    FusionRule {
        id: "nes_ines",
        platform: "NES",
        legs: &[Exact {
            kind: ContentSignature,
            value: "iNES",
            min_confidence: STRONG,
        }],
        explanation: "a parsed iNES header is NES-specific strong evidence",
    },
    FusionRule {
        id: "nes_nes20",
        platform: "NES",
        legs: &[Exact {
            kind: ContentSignature,
            value: "NES 2.0",
            min_confidence: STRONG,
        }],
        explanation: "a parsed NES 2.0 header is NES-specific strong evidence",
    },
    FusionRule {
        id: "snes_lorom",
        platform: "SNES",
        legs: &[Exact {
            kind: ContentSignature,
            value: "LoROM",
            min_confidence: STRONG,
        }],
        explanation: "a checksum-validated LoROM candidate header is SNES-specific strong evidence",
    },
    FusionRule {
        id: "snes_hirom",
        platform: "SNES",
        legs: &[Exact {
            kind: ContentSignature,
            value: "HiROM",
            min_confidence: STRONG,
        }],
        explanation: "a checksum-validated HiROM candidate header is SNES-specific strong evidence",
    },
    FusionRule {
        id: "snes_exhirom",
        platform: "SNES",
        legs: &[Exact {
            kind: ContentSignature,
            value: "ExHiROM",
            min_confidence: STRONG,
        }],
        explanation: "a checksum-validated ExHiROM candidate header is SNES-specific strong evidence",
    },
    FusionRule {
        id: "n64_z64_signature",
        platform: "N64",
        legs: &[Exact {
            kind: ContentSignature,
            value: "z64",
            min_confidence: STRONG,
        }],
        explanation: "the canonical z64 byte-order header magic is N64-specific strong evidence",
    },
    FusionRule {
        id: "gb_logo_and_checksum",
        platform: "Game Boy",
        legs: &[Exact {
            kind: BootStructure,
            value: "Nintendo Game Boy logo",
            min_confidence: STRONG,
        }],
        explanation: "Nintendo logo bitmap matched AND the header checksum validated - Game Boy-specific strong evidence",
    },
    FusionRule {
        id: "gb_logo_only_candidate",
        platform: "Game Boy",
        legs: &[Exact {
            kind: BootStructure,
            value: "Nintendo Game Boy logo",
            min_confidence: CORROBORATED,
        }],
        explanation: "Nintendo logo matched but the header checksum did not validate - candidate only",
    },
    // -- Game Boy Color (Batch 6): the CGB-only fact is its own,
    //    genuinely platform-specific evidence - see
    //    gb_header_evidence::observe_gb_evidence's own doc comment for why
    //    CGB-enhanced dual-mode carts deliberately do NOT get an
    //    exclusive-resolving rule here (they resolve as Game Boy, via the
    //    two rules above, with the dual-mode fact only ever corroborating).
    FusionRule {
        id: "gbc_cgb_only_logo_and_checksum",
        platform: "Game Boy Color",
        legs: &[Exact {
            kind: BootStructure,
            value: "Nintendo Game Boy Color logo (CGB-only)",
            min_confidence: STRONG,
        }],
        explanation: "Nintendo logo bitmap matched with cgb_flag=0xC0 (CGB-exclusive) AND the header checksum validated - Game Boy Color-specific strong evidence",
    },
    FusionRule {
        id: "gbc_cgb_only_logo_candidate",
        platform: "Game Boy Color",
        legs: &[Exact {
            kind: BootStructure,
            value: "Nintendo Game Boy Color logo (CGB-only)",
            min_confidence: CORROBORATED,
        }],
        explanation: "CGB-only logo matched but the header checksum did not validate - candidate only",
    },
    FusionRule {
        id: "gba_header_strong",
        platform: "Game Boy Advance",
        legs: &[Exact {
            kind: BootStructure,
            value: "GBA cartridge header",
            min_confidence: STRONG,
        }],
        explanation: "the GBA fixed value AND complement checksum both validated - platform-specific strong evidence",
    },
    FusionRule {
        id: "gba_header_candidate",
        platform: "Game Boy Advance",
        legs: &[Exact {
            kind: BootStructure,
            value: "GBA cartridge header",
            min_confidence: WEAK,
        }],
        explanation: "only one of the GBA fixed value/complement checksum validated - candidate only",
    },
    FusionRule {
        id: "atari_lynx_header",
        platform: "Atari Lynx",
        legs: &[Exact {
            kind: BootStructure,
            value: "LYNX",
            min_confidence: STRONG,
        }],
        explanation: "the LYNX header magic is Atari Lynx-specific strong evidence",
    },
    FusionRule {
        id: "atari7800_header",
        platform: "Atari7800",
        legs: &[Exact {
            kind: BootStructure,
            value: "ATARI7800",
            min_confidence: STRONG,
        }],
        explanation: "the ATARI7800 header magic is Atari 7800-specific strong evidence",
    },
    FusionRule {
        id: "gamecube_disc_header",
        platform: "GameCube",
        legs: &[Exact {
            kind: BootStructure,
            value: "GameCube",
            min_confidence: STRONG,
        }],
        explanation: "nod validated a GameCube disc header - platform-specific strong evidence",
    },
    FusionRule {
        id: "wii_disc_header",
        platform: "Wii",
        legs: &[Exact {
            kind: BootStructure,
            value: "Wii",
            min_confidence: STRONG,
        }],
        explanation: "nod validated a Wii disc header - platform-specific strong evidence",
    },
    // -- Microsoft: XDVDFS (Family(Xbox)) + a platform-specific executable
    //    magic together resolve which Xbox generation --
    FusionRule {
        id: "xbox_original_disc",
        platform: "Xbox",
        legs: &[
            Exact {
                kind: Filesystem,
                value: "XDVDFS",
                min_confidence: STRONG,
            },
            Exact {
                kind: ContentSignature,
                value: "XBEH",
                min_confidence: STRONG,
            },
        ],
        explanation: "XDVDFS filesystem plus default.xbe's own XBEH header magic",
    },
    FusionRule {
        id: "xbox360_disc",
        platform: "Xbox360",
        legs: &[
            Exact {
                kind: Filesystem,
                value: "XDVDFS",
                min_confidence: STRONG,
            },
            Exact {
                kind: ContentSignature,
                value: "XEX2",
                min_confidence: STRONG,
            },
        ],
        explanation: "XDVDFS filesystem plus default.xex's own XEX2 header magic",
    },
    // -- Sony: SYSTEM.CNF boot key + executable magic (+ PS3's package
    //    layout) together resolve which PlayStation generation --
    FusionRule {
        id: "ps1_system_cnf_boot",
        platform: "PSX",
        legs: &[
            Exact {
                kind: ContentSignature,
                value: "PS-X EXE",
                min_confidence: STRONG,
            },
            Exact {
                kind: BootStructure,
                value: "BOOT",
                min_confidence: CORROBORATED,
            },
        ],
        explanation: "SYSTEM.CNF BOOT= plus a validated PS-X EXE executable header",
    },
    // -- PS2 (Batch 6): BOOT2 is PS2-exclusive in this crate's grammar
    //    (content_evidence_scope.rs already scopes it
    //    PlatformSpecific("PS2")) - once ps2_boot_evidence::observe_ps2_evidence
    //    has confirmed the named executable is a real, valid ELF, the
    //    BOOT2 fact itself is upgraded to Strong. ELF stays Weak in its
    //    own right always - it is never the discriminating leg here, the
    //    now-Strong BOOT2 key is. See that module's own doc comment for
    //    the full justification and why this is not "promoting ELF."
    FusionRule {
        id: "ps2_system_cnf_boot2_strong",
        platform: "PS2",
        legs: &[Exact {
            kind: BootStructure,
            value: "BOOT2",
            min_confidence: STRONG,
        }],
        explanation: "SYSTEM.CNF BOOT2= key confirmed against a validated ELF executable at the named path - BOOT2 is PS2-exclusive in this crate's grammar, unlike BOOT (shared with PS1)",
    },
    FusionRule {
        id: "ps2_system_cnf_boot2_candidate",
        platform: "PS2",
        legs: &[
            Exact {
                kind: BootStructure,
                value: "BOOT2",
                min_confidence: CORROBORATED,
            },
            Exact {
                kind: ContentSignature,
                value: "ELF",
                min_confidence: WEAK,
            },
        ],
        explanation: "SYSTEM.CNF BOOT2= plus a generic ELF magic, without a confirmed ELF header on the named executable - candidate only",
    },
    // -- PSP (Batch 6): UMD_DATA.BIN is PSP-UMD-exclusive (see
    //    psp_boot_evidence.rs's own doc comment) - genuinely different
    //    from PSP_GAME/, which PS3 shares the convention style of
    //    (PS3_GAME/). PSP_GAME/ alone, without UMD_DATA.BIN, stays
    //    candidate-only via the rule below.
    FusionRule {
        id: "psp_umd_data_bin_strong",
        platform: "PSP",
        legs: &[
            Exact {
                kind: BootStructure,
                value: "PSP_GAME",
                min_confidence: CORROBORATED,
            },
            Exact {
                kind: BootStructure,
                value: "UMD_DATA.BIN",
                min_confidence: STRONG,
            },
        ],
        explanation: "PSP_GAME layout plus UMD_DATA.BIN - the UMD medium-identification file is PSP-UMD-exclusive, no other Sony optical format in this crate uses it",
    },
    FusionRule {
        id: "psp_layout_candidate",
        platform: "PSP",
        legs: &[
            Exact {
                kind: BootStructure,
                value: "PSP_GAME",
                min_confidence: CORROBORATED,
            },
            AnyOfKind {
                kind: ProductCode,
                min_confidence: CORROBORATED,
            },
        ],
        explanation: "PSP_GAME layout plus a PARAM.SFO-derived product code, without a confirmed UMD_DATA.BIN - candidate only",
    },
    FusionRule {
        id: "ps3_full_layout",
        platform: "PS3",
        legs: &[
            Exact {
                kind: BootStructure,
                value: "PS3_GAME",
                min_confidence: CORROBORATED,
            },
            Exact {
                kind: ContentSignature,
                value: "SELF",
                min_confidence: STRONG,
            },
            AnyOfKind {
                kind: ProductCode,
                min_confidence: CORROBORATED,
            },
        ],
        explanation: "PS3_GAME layout, a PARAM.SFO-derived TITLE_ID, and PS3 SELF executable magic together",
    },
];

/// The four outcomes this milestone requires - see the module documentation
/// and each variant's own notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusionOutcome {
    /// Exactly one canonical platform (after equivalence-folding - see
    /// [`crate::platform::equivalent_platform_ids`]) has a fired rule with
    /// [`FusionRule::has_strong_leg`], and no other platform does.
    Resolved,
    /// No platform reached [`Resolved`](Self::Resolved), but at least one
    /// rule fired (candidate-only, or multiple genuinely plausible
    /// candidates). Evidence is exposed, never discarded - see
    /// [`ResolutionExplanation::fired_candidates`].
    Ambiguous,
    /// Two or more *non-equivalent* canonical platforms each have a fired
    /// rule with [`FusionRule::has_strong_leg`] - the non-negotiable
    /// strong-vs-strong fail-closed rule. Never resolved by majority, by
    /// rule order, or by any other implicit tiebreak.
    Conflict,
    /// No rule fired at all.
    Unknown,
}

/// One rule that fired (every leg satisfied), whether or not it was
/// strong-eligible - see [`ResolutionExplanation::fired_candidates`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FiredCandidate {
    pub rule_id: &'static str,
    pub platform: &'static str,
    pub has_strong_leg: bool,
}

/// The full, structured explanation for one [`fuse_platform_evidence`]
/// call - never a prose string as the core result (a developer probe may
/// render one from this, but this is the real, structured answer). See the
/// module documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionExplanation {
    pub outcome: FusionOutcome,
    /// Present only for [`FusionOutcome::Resolved`].
    pub resolved_platform: Option<&'static str>,
    /// Every rule whose legs were all satisfied, in [`RULES`] order -
    /// never trimmed, even for [`FusionOutcome::Resolved`] (a caller can
    /// see every corroborating/candidate rule that also fired alongside
    /// the winner).
    pub fired_candidates: Vec<FiredCandidate>,
    /// Present only for [`FusionOutcome::Conflict`]: one representative
    /// canonical id per conflicting (non-equivalent) platform group,
    /// sorted for determinism.
    pub conflicting_platforms: Vec<&'static str>,
    /// The exact input evidence this explanation was computed from -
    /// canonicalised (sorted, deduplicated) via
    /// [`crate::content_evidence::observe_content_evidence`], never
    /// silently dropped or summarised away. Provenance stays attached to
    /// the decision, not just to the winning fact.
    pub input_evidence: Vec<ContentEvidence>,
}

/// Groups `platforms` into canonical-equivalence classes using
/// [`crate::platform::equivalent_platform_ids`] (e.g. `PC Engine` and
/// `TurboGrafx-16` fold into one group) - never a second equivalence
/// table. Each returned group is sorted for determinism.
pub(crate) fn group_by_equivalence(platforms: &[&'static str]) -> Vec<Vec<&'static str>> {
    let mut groups: Vec<Vec<&'static str>> = Vec::new();
    'outer: for &platform in platforms {
        for group in &mut groups {
            let already_in_group = group.contains(&platform);
            let equivalent_to_group = group
                .iter()
                .any(|member| equivalent_platform_ids(member).contains(&platform));
            if already_in_group || equivalent_to_group {
                if !already_in_group {
                    group.push(platform);
                    group.sort_unstable();
                }
                continue 'outer;
            }
        }
        groups.push(vec![platform]);
    }
    groups.sort_by(|a, b| a[0].cmp(b[0]));
    groups
}

/// Fuses `facts` (from any of this crate's detectors, in any order, with
/// any duplicates) into one [`ResolutionExplanation`]. Pure and read-only:
/// never opens a file, never mutates `facts`, never authorizes a rename/
/// move/delete/library action - see the module documentation.
pub fn fuse_platform_evidence(
    facts: impl IntoIterator<Item = ContentEvidence>,
) -> ResolutionExplanation {
    let input_evidence = crate::content_evidence::observe_content_evidence(facts).facts;

    let fired: Vec<&FusionRule> = RULES
        .iter()
        .filter(|rule| rule.is_satisfied(&input_evidence))
        .collect();

    let fired_candidates: Vec<FiredCandidate> = fired
        .iter()
        .map(|rule| FiredCandidate {
            rule_id: rule.id,
            platform: rule.platform,
            has_strong_leg: rule.has_strong_leg(),
        })
        .collect();

    let strong_platforms: Vec<&'static str> = fired
        .iter()
        .filter(|rule| rule.has_strong_leg())
        .map(|rule| rule.platform)
        .collect();
    let strong_groups = group_by_equivalence(&strong_platforms);

    let (outcome, resolved_platform, conflicting_platforms) = match strong_groups.len() {
        0 if fired.is_empty() => (FusionOutcome::Unknown, None, Vec::new()),
        0 => (FusionOutcome::Ambiguous, None, Vec::new()),
        1 => (
            FusionOutcome::Resolved,
            Some(strong_groups[0][0]),
            Vec::new(),
        ),
        _ => {
            let representatives: Vec<&'static str> =
                strong_groups.iter().map(|group| group[0]).collect();
            (FusionOutcome::Conflict, None, representatives)
        }
    };

    ResolutionExplanation {
        outcome,
        resolved_platform,
        fired_candidates,
        conflicting_platforms,
        input_evidence,
    }
}

/// How this module's own content-fusion outcome relates to a separately
/// obtained DAT-audit platform assignment - see
/// [`compare_content_and_dat`]. Deliberately its own type, never merged into
/// [`ResolutionExplanation`]: content resolution and DAT resolution keep
/// their own provenance, per the milestone's own "Content resolution / DAT
/// resolution / Combined resolution" requirement. A DAT match never
/// silently overrides a strong internal-content contradiction - see
/// [`DatContentComparison::Disagree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatContentComparison {
    /// Content fusion resolved a platform, and a verified DAT audit
    /// (assumed already gated on [`crate::dat::audit::AuditVerdict`] by the
    /// caller - this module trusts the caller made that check, exactly as
    /// [`crate::platform::identity::PlatformIdentityEvidence::from_verified_dat`]
    /// already requires of its own callers) named the same or an
    /// equivalent canonical platform. The stronger of the two provenance
    /// claims a caller can currently make.
    Agree {
        content_platform: &'static str,
        dat_platform: &'static str,
    },
    /// Content fusion resolved a platform, and the verified DAT audit named
    /// a *different*, non-equivalent canonical platform. This is a genuine
    /// conflict between two independently strong claims - report it, never
    /// silently prefer either source.
    Disagree {
        content_platform: &'static str,
        dat_platform: &'static str,
    },
    /// Content fusion resolved a platform; no DAT platform was supplied to
    /// compare against.
    ContentOnly { content_platform: &'static str },
    /// A DAT platform was supplied but content fusion did not reach
    /// [`FusionOutcome::Resolved`] (`Unknown`, `Ambiguous`, or `Conflict`).
    DatOnly { dat_platform: &'static str },
    /// Neither side named a platform.
    Neither,
}

/// Compares this module's own [`ResolutionExplanation`] against a
/// separately obtained DAT-audit canonical platform id (already vetted by
/// the caller against [`crate::dat::audit::AuditVerdict`] - this function
/// does not itself touch [`crate::dat::audit`] or [`crate::platform::identity`],
/// keeping the two resolvers' own responsibilities separate; see the module
/// documentation for why this crate has two resolvers at all).
///
/// Equivalent canonical ids (e.g. `"PC Engine"`/`"TurboGrafx-16"`) agree,
/// exactly as fusion's own strong-conflict grouping already does - see
/// [`group_by_equivalence`].
pub fn compare_content_and_dat(
    content: &ResolutionExplanation,
    dat_platform: Option<&'static str>,
) -> DatContentComparison {
    match (content.resolved_platform, dat_platform) {
        (Some(content_platform), Some(dat_platform)) => {
            let equivalent = group_by_equivalence(&[content_platform, dat_platform]).len() == 1;
            if equivalent {
                DatContentComparison::Agree {
                    content_platform,
                    dat_platform,
                }
            } else {
                DatContentComparison::Disagree {
                    content_platform,
                    dat_platform,
                }
            }
        }
        (Some(content_platform), None) => DatContentComparison::ContentOnly { content_platform },
        (None, Some(dat_platform)) => DatContentComparison::DatOnly { dat_platform },
        (None, None) => DatContentComparison::Neither,
    }
}

/// Marks every fact in `evidence` as having been observed through a
/// physical/normalized dual-identity transform (N64 byte-order correction,
/// SNES/Lynx/Atari7800 copier-header stripping, SMD de-interleaving) rather
/// than directly from the file's own physical bytes - per the milestone's
/// own requirement that "provenance must say evidence derived from a
/// normalized view," distinct from the physical identity, which stays
/// separately preserved by the normalization modules themselves (this
/// function does not touch or duplicate their own physical/normalized
/// distinction - it only annotates the [`ContentEvidence::detail`] text
/// fusion already carries as its provenance channel, so
/// [`ResolutionExplanation::input_evidence`] can show the annotation
/// without adding a new field to [`ContentEvidence`] itself).
///
/// Kind, value, and confidence are left untouched - only `detail` gains a
/// prefix - so tagging never changes which [`FusionRule`]s fire or what
/// [`FusionOutcome`] results; see
/// `tests::tagging_as_normalized_never_changes_the_fusion_outcome`.
pub fn tag_normalized_view_evidence(
    evidence: impl IntoIterator<Item = ContentEvidence>,
) -> Vec<ContentEvidence> {
    const PREFIX: &str = "[normalized view] ";
    evidence
        .into_iter()
        .map(|fact| {
            if fact.detail.starts_with(PREFIX) {
                fact
            } else {
                ContentEvidence {
                    detail: format!("{PREFIX}{}", fact.detail),
                    ..fact
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod dat_and_normalization_tests;

/// The Batch 6 bridge into [`crate::platform::identity`] - see that
/// module's own doc comment for the full design.
pub mod identity_bridge;
