//! External, read-only identity providers.
//!
//! ArchiveFS already derives identity from the bytes it can see. This module is
//! for identity that someone *else* has already established - a RomM instance
//! that has scanned and matched a library, a local DAT catalogue, a Hasheous
//! lookup - so ArchiveFS can use that work instead of redoing or guessing it.
//!
//! Three rules shape the whole design:
//!
//! 1. **Read-only.** No provider in this module writes to its source. There is
//!    no code path that could.
//! 2. **External evidence is evidence, not truth.** An imported record never
//!    silently replaces something ArchiveFS verified locally. Where they
//!    disagree, both are kept and the conflict is shown.
//! 3. **Local network only.** A provider endpoint is validated by
//!    [`net_policy`] before anything connects to it.

pub mod cache;
pub mod hashing;
pub mod matching;
pub mod model;
pub mod net_policy;
pub mod path_map;
pub mod romm;
pub mod settings;
pub mod status;

#[cfg(test)]
mod stage1b_tests;
#[cfg(test)]
mod tests;
