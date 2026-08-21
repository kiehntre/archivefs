//! Batch 21: direct local No-Intro DAT ingestion.
//!
//! Turns a DAT file *already present on disk* into a lineage-aware
//! [`crate::platform_evidence_fusion::evidence_lineage`] evidence source.
//! No network access of any kind exists in this module - nothing here
//! downloads, scrapes, or auto-updates anything. A caller supplies an
//! explicit path (or directory of paths); everything else is local parsing
//! and hashing.
//!
//! # Reuse, not reinvention
//!
//! This module is deliberately thin:
//!
//! - DAT parsing reuses [`crate::dat::parsers::parse_dat_file`] (Logiqx XML /
//!   ClrMamePro) unchanged.
//! - No-Intro identification reuses [`crate::dat::model::DatEcosystem`],
//!   which is already detected conservatively from *internal* DAT metadata
//!   (`<header><name>`/`<author>`), never from a filename - see
//!   `detect_logiqx_ecosystem` in `dat::parsers::logiqx`.
//! - Hash indexing reuses [`crate::dat::index::DatIndex`] verbatim, which
//!   already preserves full multiplicity (every hash maps to a `Vec`, never
//!   collapsed to one hit).
//!
//! The only new work is: (1) recording the DAT *file's own* artifact
//! provenance (its SHA-256, and the version/variant metadata a No-Intro DAT
//! carries), and (2) converting a `DatIndex` lookup into
//! [`crate::platform_evidence_fusion::evidence_lineage::EvidenceObservation`]s
//! with `channel = LocalNoIntro`, `upstream_source = NoIntro`,
//! `lineage = Independent`.
//!
//! # Scope freeze
//!
//! Exactly like Batch 20's Hasheous adapter, observations from this module
//! feed the evidence-lineage summary only. Nothing here touches
//! [`crate::platform_evidence_fusion::combined_identity`],
//! [`crate::dat::identity`], library planning, or transaction execution.

pub mod convert;
pub mod import;

pub use convert::{claim_for_representation, lookup_no_intro, observations_from_no_intro_matches};
pub use import::{ImportedNoIntroSource, NoIntroImportError, NoIntroVariant, import_no_intro_dat};

#[cfg(test)]
mod tests;
