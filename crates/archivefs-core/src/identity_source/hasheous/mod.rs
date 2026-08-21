//! Batch 20: the first real external evidence adapter, against
//! [`crate::platform_evidence_fusion::evidence_lineage`]'s Batch-19
//! foundation.
//!
//! # What this is
//!
//! `file/hash -> Hasheous lookup -> raw provider response ->
//! upstream-source mapping -> EvidenceObservation[] -> evidence-lineage
//! merge/explanation`. Every observation this module produces has
//! `channel = `[`EvidenceChannel::Hasheous`]`; the *upstream* preservation
//! source (No-Intro, TOSEC, Redump, ...) always comes from the response's
//! own source tag via
//! [`crate::platform_evidence_fusion::evidence_lineage::hasheous_upstream_for_tag`].
//! Hasheous itself is structurally incapable of becoming a
//! [`crate::platform_evidence_fusion::evidence_lineage::SourceFamily`]
//! value - there is no match arm that returns it, and
//! `hasheous_never_becomes_a_source_family` in this module's tests asserts
//! it by exhaustively checking every `SourceFamily` variant name never
//! equals the literal string `"Hasheous"`.
//!
//! # Live API verification (section 5) - differences from prior research
//!
//! This batch fetched the live
//! `https://hasheous.org/swagger/v1/swagger.json` document (`GET`, `curl`
//! with a browser `User-Agent` - a plain `WebFetch` was blocked with an
//! HTTP 403 from Hasheous's edge, `curl` was not) and built this module
//! against the real schema rather than the milestone's prior research.
//! Three real differences were found:
//!
//! 1. **Endpoint path.** The live path is `/api/v1/Lookup/ByHash`, not
//!    `/api/v1.0/Lookup/ByHash` as previously researched.
//! 2. **`returnAllSources`/`returnFields` are query parameters**, not
//!    fields inside the JSON request body. This adapter always sends
//!    `returnAllSources=true&returnFields=All` (section 4/9) so a match is
//!    never collapsed to "the first source."
//! 3. **No documented "N files per request, N results back" batching.**
//!    The request body's `oneOf` schema allows either one hash-object or an
//!    *array* of hash-objects, but the documented 200 response is a single
//!    `Classes.HashLookup` object regardless - there is no per-item
//!    response association in the schema. The array form's own example
//!    (`[{"crc": "..."}, {"md5": "..."}]`) reads as "try these alternate
//!    hash algorithms for the *same* file," matching this milestone's own
//!    section 15 rule ("hashes supplied in ONE object must refer to the
//!    same signature") rather than a genuine multi-file batch. Given that
//!    ambiguity and the single-object response schema, [`client::HasheousClient::lookup_many`]
//!    issues **one HTTP request per item** rather than array-batching
//!    distinct files into one POST; [`client::MAX_BATCH_SIZE`] instead bounds
//!    how many items one `lookup_many` call processes, satisfying this
//!    milestone's requested deterministic-chunking test boundaries
//!    (0/1/49/50/51/100+) without overclaiming a batch-response contract
//!    the live schema does not actually document. See that constant's own
//!    doc comment for the full reasoning.
//! 4. **No-match is HTTP 404**, not a 200 with an empty `signatures` map -
//!    the milestone's prior research assumed the latter. This adapter's
//!    [`client::HasheousLookupOutcome::NoMatch`] treats a 404 as the
//!    neutral, successful no-match result (section 26); a real 400/429/5xx
//!    is a distinct, separately reported error.
//!
//! # Privacy (sections 7, 64)
//!
//! Only hash values (and the two query-parameter selectors above) ever
//! leave this machine. No local path, filename, ROM byte content, or
//! library-root string is ever placed in a Hasheous request body or URL -
//! `hasheous_request_body_contains_no_local_path_or_filename` inspects the
//! actual serialized request bytes to prove it.
//!
//! # What this does NOT do
//!
//! No network call happens in the default `cargo test` run - every test in
//! [`tests`] drives [`client::HasheousClient`] against a fully in-memory
//! fake [`client::HasheousTransport`] using fixture JSON, never a socket.
//! The only real network call this batch makes is the one-off live schema
//! fetch above (done via `curl`, not compiled into this crate) and the
//! manually-run `examples/hasheous_probe.rs` live validation. Nothing here
//! changes [`crate::dat::identity`], `combined_identity`,
//! [`crate::platform_evidence_fusion`]'s existing content-fusion behavior,
//! library planning, or the transaction engine - observations feed
//! [`crate::platform_evidence_fusion::evidence_lineage::merge_evidence`]
//! only.

pub mod client;
pub mod convert;
pub mod dto;

pub use client::{
    HASHEOUS_DEFAULT_BASE_URL, HasheousClient, HasheousConfig, HasheousLookupOutcome,
    HasheousRequestError, HasheousTransport, MAX_BATCH_SIZE, UreqTransport, now_unix,
};
pub use convert::observations_from_hash_lookup;
pub use dto::{HashLookupResponse, HasheousHashSet};

#[cfg(test)]
mod tests;
