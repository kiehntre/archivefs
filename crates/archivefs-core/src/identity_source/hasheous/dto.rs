//! Raw Hasheous wire types (Batch 20, section 9).
//!
//! These mirror `POST /api/v1/Lookup/ByHash` exactly as verified against the
//! live `https://hasheous.org/swagger/v1/swagger.json` document during this
//! batch - see the module doc on [`super`] for the exact differences found
//! against this milestone's prior research. Deliberately narrow: only the
//! fields this adapter actually converts into observations are
//! deserialized, and `#[serde(default)]` everywhere means an unrecognised
//! or missing field is tolerated rather than a hard parse failure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The request body for one hash-set: `POST /api/v1/Lookup/ByHash` accepts
/// either one object (this type) or an array of them. Every field is
/// `skip_serializing_if` so only hashes actually known are ever put on the
/// wire - never a local path, filename, or byte content (section 7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HasheousHashSet {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl HasheousHashSet {
    pub fn is_empty(&self) -> bool {
        self.crc.is_none() && self.md5.is_none() && self.sha1.is_none() && self.sha256.is_none()
    }
}

/// `Classes.HashLookup` - the 200 response body. `signatures` is the
/// per-upstream-source map produced when the request used
/// `returnAllSources=true` (which this adapter always sends, per section 4,
/// so provenance/multiplicity is never collapsed to "the first source").
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct HashLookupResponse {
    #[serde(default)]
    pub platform: Option<MiniDataObjectItem>,
    #[serde(default)]
    pub publisher: Option<MiniDataObjectItem>,
    #[serde(default)]
    pub signature: Option<SignatureResult>,
    #[serde(default)]
    pub signatures: Option<BTreeMap<String, Vec<SignatureResult>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct MiniDataObjectItem {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct SignatureResult {
    #[serde(default)]
    pub game: Option<GameItem>,
    #[serde(default)]
    pub rom: Option<RomItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct GameItem {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct RomItem {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub crc: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    /// `RomSignatureObject_Game_Rom_SignatureSourceType` as a raw string -
    /// kept for the `signature` (singular, first-source-only) fallback
    /// path; the primary `signatures` map already carries the source as
    /// its own key, which is the value this adapter actually trusts
    /// (section 24).
    #[serde(default, rename = "signatureSource")]
    pub signature_source: Option<String>,
}

/// `Microsoft.AspNetCore.Mvc.ProblemDetails` - the 400/404 error body. Only
/// `title`/`detail` are read; nothing here is trusted as identity.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ProblemDetails {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
}
