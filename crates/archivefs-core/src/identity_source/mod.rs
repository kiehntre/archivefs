//! External, read-only identity providers.
//!
//! EmuWiz already derives identity from the bytes it can see. This module is
//! for identity that someone *else* has already established - a RomM instance
//! that has scanned and matched a library, a local DAT catalogue, a Hasheous
//! lookup - so EmuWiz can use that work instead of redoing or guessing it.
//!
//! Three rules shape the whole design:
//!
//! 1. **Read-only.** No provider in this module writes to its source. There is
//!    no code path that could.
//! 2. **External evidence is evidence, not truth.** An imported record never
//!    silently replaces something EmuWiz verified locally. Where they
//!    disagree, both are kept and the conflict is shown.
//! 3. **Local network only, for a user-typed endpoint with a bearer token
//!    attached.** [`net_policy`]'s SSRF gate exists specifically for that
//!    combination (a URL a person entered, then sent a secret to) - see its
//!    own module doc.
//!
//! [`hasheous`] is this module's one deliberate exception to rule 3, not a
//! violation of it: it talks to a single fixed, non-user-configurable public
//! host, sends no bearer token or other secret, and its request body is
//! exhaustively proven to carry only hash values (see that module's own doc
//! for the exact privacy proof). The SSRF threat `net_policy` defends
//! against - a user-typed address plus an attached secret - does not exist
//! here, so routing it through that gate would only refuse the one request
//! this adapter is meant to make.

pub mod artwork;
pub mod cache;
pub mod hasheous;
pub mod hashing;
pub mod matching;
pub mod model;
pub mod net_policy;
pub mod no_intro;
pub mod path_map;
pub mod romm;
pub mod settings;
pub mod stale;
pub mod status;
pub mod verification;

#[cfg(test)]
mod stage1b_tests;
#[cfg(test)]
mod tests;
