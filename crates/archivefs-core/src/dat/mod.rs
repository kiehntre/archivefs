//! Provider-neutral DAT catalogue parsing and read-only audit.
//!
//! A DAT ("datafile") catalogue is a list of known ROM dumps published by
//! preservation communities such as No-Intro, Redump, and TOSEC. This module
//! parses DAT files in both Logiqx XML and ClrMamePro text formats, indexes
//! their contents for fast lookup, and provides a read-only audit that
//! compares known hashes against the catalogue.
//!
//! # Design
//!
//! 1. **Provider-agnostic model.** Every DAT entry maps to the same
//!    `DatGameEntry`/`DatRomEntry` shape, whether it came from Logiqx or
//!    ClrMamePro, No-Intro or TOSEC.
//! 2. **Streaming XML parsing.** The Logiqx parser reads XML in bounded
//!    events, never loading the full document into memory, and explicitly
//!    rejects DOCTYPE, entity declarations, and unsupported entity references.
//! 3. **Collision-aware indexes.** When two entries share a CRC32 (or any
//!    other hash), both are kept. The audit reports the collision rather than
//!    silently picking one.
//! 4. **Read-only audit.** The audit takes hashes _already known_ to
//!    ArchiveFS as input; it never hashes local files. Every verdict
//!    distinguishes between exact, probable, filename-only, ambiguous, and
//!    no-evidence outcomes.
//!
//! # Security
//!
//! - DOCTYPE declarations are rejected.
//! - Entity declarations and unsupported entity references are rejected.
//! - XML depth is manually enforced.
//! - Configurable ceilings bound file size, entry count, ROM count, and
//!   identifier lengths.
//! - Every parse warning includes byte offset and source context.
//! - Unknown status values are preserved rather than discarded.

pub mod audit;
pub mod hash;
pub mod index;
pub mod limits;
pub mod model;
pub mod parser;
pub mod parsers;
#[cfg(test)]
mod regression;
