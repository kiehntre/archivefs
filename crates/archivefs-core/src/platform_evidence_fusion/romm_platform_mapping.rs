//! Batch 11: the production canonical-platform -> RomM slug mapping.
//!
//! # Direction, non-negotiable
//!
//! ```text
//! EmuWiz canonical platform id  ->  RomM slug
//! ```
//!
//! never the reverse. [`crate::identity_source::romm::normalise::canonical_platform_for_romm_slug`]
//! already owns the opposite (inbound-import) direction and is never called
//! from here - RomM is never identity authority for this crate, only a
//! downstream integration target. See [`production_romm_slug`]'s tests for
//! the enforced separation.
//!
//! # Why this is not a from-scratch guess
//!
//! Batch 10 found zero production canonical -> slug mapping and considered
//! (then rejected) inventing one. Batch 11's audit of
//! `library_views::resolve_romm_platform_slug` found the *real* production
//! forward resolver already exists, with two tiers (explicit user override,
//! then a locally-cached live RomM instance's own reported slug) and a
//! doc comment that deliberately, explicitly declines to add a bundled
//! static table as a third tier - because the crate's only existing slug
//! data (`ROMM_SLUG_ALIASES`, inbound-only) contains several intentionally
//! approximate many-to-one entries (`fds -> NES`, `pc-fx -> PC Engine`,
//! `xboxone -> Xbox`) that would be actively wrong if blindly inverted.
//!
//! [`STATIC_TABLE`] below is exactly the "genuinely 1:1, vetted forward
//! table" that doc comment says may be added as a third tier later. It is
//! built by manually reviewing every entry in `ROMM_SLUG_ALIASES` (the only
//! *observed*, not invented, RomM slug data in this repo) and keeping only
//! the platforms where:
//!
//! - exactly one slug maps to that canonical platform (a platform named by
//!   *two different* observed slugs - `Amstrad CPC` via both `acpc` and
//!   `cpc`; `Commodore 64` via both `c-plus-4` and `c16`; `Sega CD` via both
//!   `sega-cd` and `segacd` - has no single defensible "current" choice
//!   without guessing, so it is left `Ambiguous` here, not resolved); and
//! - that slug is not one of the three entries `ROMM_SLUG_ALIASES`'s own
//!   doc comment already names as an approximate/many-to-one association
//!   (`fds`, `pc-fx`, `xboxone`).
//!
//! Every other canonical platform (the large majority of the 74) has no
//! observed RomM slug anywhere in this repo and is honestly `Unmapped`
//! here - never guessed from its display name or folder alias.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::identity_source::cache::IdentityCache;
use crate::library_views::resolve_romm_platform_slug;
// `library_views` is a private module - re-exported here so external
// callers of this module's public functions (which take a
// `FrontendPlatformMapping` by reference) never need to reach into it
// directly.
pub use crate::library_views::FrontendPlatformMapping;
use crate::platform::platform_by_id;

/// The outcome of resolving one canonical platform to a RomM slug -
/// milestone section 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RommMappingSupportStatus {
    /// A slug is known, from a live override/cache tier or the vetted
    /// static table.
    Mapped,
    /// No slug is known from any tier - the honest default.
    Unmapped,
    /// More than one distinct slug has been observed for this canonical
    /// platform with no defensible way to pick one - never resolved to
    /// either.
    Ambiguous,
    /// Reserved for a platform this crate has *positive* evidence RomM
    /// does not model the way this planner expects (none exist yet - see
    /// this module's own doc comment on why `Unmapped` is the default
    /// instead of guessing `Unsupported`).
    Unsupported,
}

/// One canonical platform's static, reviewed RomM mapping entry -
/// milestone section 5's required shape.
#[derive(Debug, Clone, Copy)]
pub struct RommPlatformMapping {
    pub canonical_platform_id: &'static str,
    pub slug: Option<&'static str>,
    pub aliases: &'static [&'static str],
    pub status: RommMappingSupportStatus,
    pub provenance: &'static str,
}

/// The vetted static table - see the module doc comment for how each entry
/// was derived. `aliases` lists every *other* slug this repo has observed
/// for the same canonical platform that was deliberately not chosen as the
/// primary outbound slug (kept for provenance, never used as the resolved
/// value).
const STATIC_TABLE: &[RommPlatformMapping] = &[
    RommPlatformMapping {
        canonical_platform_id: "Dreamcast",
        slug: Some("dc"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in identity_source::romm::normalise::ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "Game Boy",
        slug: Some("gb"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "Game Boy Advance",
        slug: Some("gba"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "Game Boy Color",
        slug: Some("gbc"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "MegaDrive",
        slug: Some("genesis-slash-megadrive"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "N64",
        slug: Some("n64"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "Nintendo DS",
        slug: Some("nds"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "Neo Geo CD",
        slug: Some("neo-geo-cd"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "GameCube",
        slug: Some("ngc"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "PSX",
        slug: Some("ps"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "PlayStation Vita",
        slug: Some("psvita"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "Sega 32X",
        slug: Some("sega32"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "SNES",
        slug: Some("snes"),
        aliases: &["sfam"],
        status: RommMappingSupportStatus::Mapped,
        provenance: "two slugs observed (snes, sfam - the Super Famicom regional name); \
                     'snes' chosen as the non-regional primary, 'sfam' kept as a known alias only",
    },
    RommPlatformMapping {
        canonical_platform_id: "MasterSystem",
        slug: Some("sms"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "PC Engine CD",
        slug: Some("turbografx-16-slash-pc-engine-cd"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    RommPlatformMapping {
        canonical_platform_id: "PC",
        slug: Some("win"),
        aliases: &[],
        status: RommMappingSupportStatus::Mapped,
        provenance: "single slug observed in ROMM_SLUG_ALIASES",
    },
    // Ambiguous: two distinct observed slugs, no defensible default.
    RommPlatformMapping {
        canonical_platform_id: "Amstrad CPC",
        slug: None,
        aliases: &["acpc", "cpc"],
        status: RommMappingSupportStatus::Ambiguous,
        provenance: "two distinct slugs observed (acpc, cpc) for the same canonical platform; \
                     neither chosen without external confirmation",
    },
    RommPlatformMapping {
        canonical_platform_id: "Commodore 64",
        slug: None,
        aliases: &["c-plus-4", "c16"],
        status: RommMappingSupportStatus::Ambiguous,
        provenance: "two distinct slugs observed (c-plus-4, c16) for the same canonical \
                     platform, and both names reference different real Commodore models - \
                     left unresolved rather than guessed",
    },
    RommPlatformMapping {
        canonical_platform_id: "Sega CD",
        slug: None,
        aliases: &["sega-cd", "segacd"],
        status: RommMappingSupportStatus::Ambiguous,
        provenance: "two distinct slugs observed (sega-cd, segacd) for the same canonical \
                     platform; neither chosen without external confirmation",
    },
];

/// Looks up `canonical_platform_id` in the vetted static table only (tier
/// 3 - see the module doc comment). Callers that also have a live
/// `FrontendPlatformMapping`/`IdentityCache` should prefer
/// [`production_romm_slug`] instead, which checks the real production
/// override/cache tiers first.
pub fn static_table_entry(canonical_platform_id: &str) -> Option<&'static RommPlatformMapping> {
    STATIC_TABLE
        .iter()
        .find(|entry| entry.canonical_platform_id == canonical_platform_id)
}

/// The full, reviewed static table - milestone section 6's "mapping table
/// count machine-derived" requirement reads this directly rather than a
/// separately maintained count.
pub fn static_table() -> &'static [RommPlatformMapping] {
    STATIC_TABLE
}

/// The production resolution: tier 1 (explicit user override), tier 2 (a
/// locally cached, previously imported live RomM instance's own reported
/// slug), tier 3 (this module's vetted static table). Never inverts
/// [`crate::identity_source::romm::normalise::canonical_platform_for_romm_slug`]
/// and never asks RomM anything - `identity_cache` is read entirely
/// offline.
pub fn production_romm_slug(
    canonical_platform_id: &str,
    overrides: &FrontendPlatformMapping,
    identity_cache: Option<&IdentityCache>,
) -> Option<String> {
    if let Some(slug) = resolve_romm_platform_slug(canonical_platform_id, overrides, identity_cache)
    {
        return Some(slug);
    }
    static_table_entry(canonical_platform_id)
        .and_then(|entry| entry.slug)
        .map(str::to_string)
}

/// The production support status for one canonical platform - milestone
/// section 5's status vocabulary, resolved the same tiered way
/// [`production_romm_slug`] resolves the slug itself.
pub fn production_romm_status(
    canonical_platform_id: &str,
    overrides: &FrontendPlatformMapping,
    identity_cache: Option<&IdentityCache>,
) -> RommMappingSupportStatus {
    if resolve_romm_platform_slug(canonical_platform_id, overrides, identity_cache).is_some() {
        return RommMappingSupportStatus::Mapped;
    }
    static_table_entry(canonical_platform_id)
        .map(|entry| entry.status)
        .unwrap_or(RommMappingSupportStatus::Unmapped)
}

/// Every canonical platform id known to [`crate::platform::PLATFORMS`],
/// paired with its production status under an empty override map and no
/// live cache (the static-table-only view) - milestone section 6's "every
/// canonical ID exists in platform registry" check reads this.
pub fn static_coverage_by_status() -> BTreeMap<RommMappingSupportStatus, Vec<&'static str>> {
    let empty_overrides = FrontendPlatformMapping::default();
    let mut by_status: BTreeMap<RommMappingSupportStatus, Vec<&'static str>> = BTreeMap::new();
    for platform in crate::platform::PLATFORMS {
        let status = production_romm_status(platform.id, &empty_overrides, None);
        by_status.entry(status).or_default().push(platform.id);
    }
    by_status
}

// `RommMappingSupportStatus` needs `Ord`/`PartialOrd` only for its use as a
// `BTreeMap` key above - not part of its public equality/debug contract.
impl PartialOrd for RommMappingSupportStatus {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RommMappingSupportStatus {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        fn rank(status: &RommMappingSupportStatus) -> u8 {
            match status {
                RommMappingSupportStatus::Mapped => 0,
                RommMappingSupportStatus::Unmapped => 1,
                RommMappingSupportStatus::Ambiguous => 2,
                RommMappingSupportStatus::Unsupported => 3,
            }
        }
        rank(self).cmp(&rank(other))
    }
}

/// Confirms every canonical platform id this module's static table names
/// really exists in the platform registry - milestone section 6.
pub fn every_static_entry_platform_exists() -> bool {
    STATIC_TABLE
        .iter()
        .all(|entry| platform_by_id(entry.canonical_platform_id).is_some())
}

#[cfg(test)]
mod tests;
