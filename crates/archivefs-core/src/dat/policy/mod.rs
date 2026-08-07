//! User-controlled DAT matching preferences and their pure evaluation.
//!
//! A DAT catalogue can verify several entries for one local file - several
//! regional releases, several revisions, a parent and its clone. The rest of
//! the DAT subsystem is deliberately neutral: it reports every verified
//! candidate and never picks one. This module is where the user's *preference*
//! for how such a set is ordered lives, as a persisted policy document (inside
//! `dat_sources.toml`, so the file that owns the sources owns their matching
//! policy too) and as a pure ranking over already-verified candidates.
//!
//! # What the policy answers
//!
//! - [`model::RegionId`] - ordered region preferences (`Europe, USA, Japan,
//!   World, Other` plus a catch-all);
//! - [`model::LanguagePreference`] - ordered language preferences (a specific
//!   language, multi-language, or the release's original language);
//! - [`model::RevisionPolicy`] - how revision markers decide between dumps;
//! - [`model::ClonePolicy`] - how parent/clone relationships are treated;
//! - DAT source priority - platform-local, lower-number-wins, consulted only
//!   between sources that cover the same platform;
//! - per-platform participation and per-platform overrides, keyed by
//!   canonical platform id only.
//!
//! # What the policy never does
//!
//! The policy only ever *ranks already verified candidates* and *explains the
//! rank*. It never weakens evidence, never writes a file, never renames or
//! otherwise alters a ROM, and it never resolves genuine ambiguity silently.
//! Rename safety remains `NeverSuggest`; there is no rename plan type in this
//! module and no rename control anywhere downstream.
//!
//! # Safe defaults preserve today's behaviour
//!
//! The default policy (every field absent) has empty region/language
//! preferences, [`model::RevisionPolicy::AskWhenAmbiguous`] and
//! [`model::ClonePolicy::KeepAllVariants`]. Against it the ranking never
//! separates candidates by revision or clone relationship, preferences do not
//! exist, and a tie stays ambiguous - which is exactly what the DAT audit does
//! today when it reports every candidate without choosing one.
//!
//! # Module map
//!
//! - [`model`] - the typed vocabulary (regions, languages, enums, defaults);
//! - [`tags`] - pure extraction of region/language/revision markers from names;
//! - [`candidate`] - the verified-candidate description the policy ranks;
//! - [`config`] - the persisted, schema-tolerant policy document;
//! - [`evaluate`] - effective-policy resolution, validation, and ranking.

pub mod candidate;
pub mod config;
pub mod evaluate;
pub mod model;
pub mod tags;

pub use candidate::{DatCandidate, candidate_for_rom};
pub use config::{DatPlatformPolicyConfig, DatPolicyConfig, default_dat_policy};
pub use evaluate::{
    CandidateResolution, EffectiveDatPolicy, ExcludedCandidate, ParticipatingSource, PolicyProblem,
    RankedCandidate, original_language_of, participating_sources, rank_candidates, resolve,
    validate_policy_config,
};
pub use model::{
    ClonePolicy, LanguageId, LanguagePreference, MAX_POLICY_PREFERENCE_LEN, PolicyField,
    PolicyScope, RegionId, RevisionPolicy,
};
