//! Provisional emulator-neutral adapter seam - a narrowly-scoped extraction
//! inside what is still a PCSX2-specific preview pipeline (see
//! `patch_manager::mod`'s own doc comment and
//! `docs/PATCH_CHEAT_MANAGER_DESIGN.md`'s "Emulator Adapter Architecture"
//! section). It covers only discovery, capability declaration, and
//! identity/destination derivation - PCSX2-only logic (serial/CRC
//! normalization, PNACH naming, candidate-relative destination
//! calculation) moved behind the PCSX2 implementation of
//! [`EmulatorAdapter`] in `pcsx2.rs`. Platform filtering, confidence
//! aggregation, ambiguity detection, and plan assembly remain
//! PCSX2-specific orchestration in `patch_manager::mod`, not covered by
//! this seam - a second adapter is not yet addable without first
//! generalizing that orchestration.
//!
//! This module deliberately contains only what Phase 1's actual read-only
//! PCSX2 slice needs: discovery, capability declaration, and identity/
//! destination derivation. It has no `DiscoveryContext`, no
//! `ReadOnlyGameContext`, no `plan_advisory`/`validate_advisory`/
//! `health_check` methods, no registry, and no mutation trait - those
//! remain design sketches until a second adapter's own review needs them.

use std::path::PathBuf;

use serde::Serialize;

use super::{CatalogueGameEvidence, PatchMetadataRecord, Result};

/// Stable identifier for one emulator adapter ("pcsx2", eventually
/// "retroarch", ...) - not a display name. Used to namespace identity
/// evidence and to label which adapter produced a candidate/capability.
pub type AdapterId = &'static str;

/// What one adapter declares about itself, independent of any specific
/// installation or plan. Phase 1 has exactly one read-only adapter, so
/// every field here is deliberately minimal - no format list, no version
/// matrix, and no mutation-recipe capability beyond a single `bool`
/// (always `false` until an `EmulatorMutationAdapter` is separately
/// reviewed and implemented).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterCapabilities {
    pub adapter_id: AdapterId,
    pub display_name: &'static str,
    /// The identity-evidence namespaces this adapter can supply, e.g.
    /// `["ps2-serial", "ps2-executable-crc"]`. Purely informational today;
    /// nothing yet validates plans against it.
    pub identity_namespaces: &'static [&'static str],
    pub mutation_supported: bool,
}

/// How confidently discovery believes a standard path is a real emulator
/// installation. `StandardPathCandidate` is the only level any adapter can
/// report today - discovery never inspects a binary or validates a
/// version, so it never claims stronger confidence than "a documented
/// standard directory exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum DiscoveryConfidence {
    StandardPathCandidate,
}

/// One read-only discovered installation candidate, for any adapter. A
/// candidate means only that a documented standard directory exists -
/// never that the emulator is installed, running, has a known version, or
/// is write-capable. `kind` is an adapter-defined label (PCSX2: `"Native"`
/// or `"Flatpak"`) rather than a shared enum, since different adapters
/// have different, non-overlapping candidate kinds and Phase 1 has no
/// need to enumerate them centrally.
///
/// `adapter_id` is `#[serde(skip)]`: the pre-extraction, still-current
/// `format_version = 1` JSON shape has no such field on an installation
/// candidate, and this milestone's own requirement is to change no
/// observable output. It stays on the Rust type (set by every adapter,
/// asserted by tests) purely as an in-process identification aid; nothing
/// reads it back from JSON today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallationCandidate {
    #[serde(skip)]
    pub adapter_id: AdapterId,
    pub kind: String,
    pub data_root: PathBuf,
    pub provenance: &'static str,
    pub discovery_confidence: DiscoveryConfidence,
    pub detected_version: Option<String>,
    pub mutation_readiness: &'static str,
}

/// One piece of adapter-namespaced game identity evidence, extracted
/// either from a metadata record or from a catalogue row. The namespace
/// prevents two different identity schemes (or two different adapters)
/// from ever being compared as if they were the same kind of value.
/// `match_reason`/`conflict_reason` carry the adapter's own human-readable
/// wording for the two outcomes this evidence can produce, so the shared
/// matching code in `matching.rs` never needs to know what a "PS2 serial"
/// or "executable CRC" actually is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterIdentityEvidence {
    pub namespace: &'static str,
    pub value: String,
    pub match_reason: &'static str,
    pub conflict_reason: &'static str,
}

/// A hypothetical (never created) relative destination path under one
/// installation candidate - informational only, never an approved
/// filesystem capability. `candidate_kind` mirrors the owning
/// [`InstallationCandidate::kind`] so a rendered entry can be traced back
/// to the candidate it was computed for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HypotheticalDestination {
    pub candidate_kind: String,
    pub relative_path: String,
    pub display_path: String,
    pub hypothetical: bool,
}

/// The only adapter trait Phase 1 needs: read-only discovery, capability
/// declaration, and the exact PCSX2-specific behaviors the design review
/// identified as the extraction seam (serial/CRC normalization and PNACH
/// naming, via identity evidence and hypothetical-path derivation).
///
/// Deliberately absent: `discover_installations(context: &DiscoveryContext)`,
/// `collect_game_evidence`, `plan_advisory`, `validate_advisory`,
/// `health_check`, and any mutation method. Those are aspirational shapes
/// from the design document's full future trait, not required by the
/// current read-only PCSX2 slice - adding them now would scaffold
/// capabilities this milestone does not use.
pub trait EmulatorAdapter {
    fn id(&self) -> AdapterId;
    fn capabilities(&self) -> AdapterCapabilities;
    fn discover_installations(&self) -> Result<Vec<InstallationCandidate>>;
    /// Extracts this adapter's namespaced identity evidence from one
    /// metadata record (PCSX2: the serial/CRC already normalized at fetch
    /// time from a `patches/<serial>_<crc>.pnach` filename).
    fn identity_evidence_from_record(
        &self,
        record: &PatchMetadataRecord,
    ) -> Vec<AdapterIdentityEvidence>;
    /// Extracts this adapter's namespaced identity evidence from one
    /// catalogue row (PCSX2: the raw serial/CRC fields, normalized the
    /// same way as the record side).
    fn identity_evidence_from_catalogue(
        &self,
        game: &CatalogueGameEvidence,
    ) -> Vec<AdapterIdentityEvidence>;
    /// Computes this adapter's relative in-candidate path for one record,
    /// if any (PCSX2: `patches/<file>.pnach`, recombined under a
    /// candidate's own `data_root` by the caller). `None` if this record
    /// cannot be mapped to a destination - never reached by any record
    /// Phase 1's metadata parser currently accepts, but kept as an
    /// `Option` so a future adapter is not forced to fabricate a path.
    fn hypothetical_relative_path(&self, record: &PatchMetadataRecord) -> Option<String>;
}
