//! External Gecko-code provider model and the official Dolphin upstream provider.
//!
//! Provider responsibilities stop at retrieving and validating inert code data. This module has
//! no Dolphin profile path, destination, staging, transaction, apply, or rollback API. The Dolphin
//! adapter consumes its results later and remains the sole owner of installation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::gecko_document::parse_dolphin_ini;

pub const DOLPHIN_UPSTREAM_PROVIDER_ID: &str = "dolphin_upstream_gamesettings";
pub const DOLPHIN_UPSTREAM_PROVIDER_NAME: &str = "Dolphin upstream GameSettings";
pub const DOLPHIN_UPSTREAM_REPOSITORY: &str = "dolphin-emu/dolphin";
pub const DOLPHIN_UPSTREAM_LICENSE: &str = "GPL-2.0-or-later";
pub const DOLPHIN_UPSTREAM_ATTRIBUTION: &str =
    "Gecko definitions from the Dolphin Emulator upstream GameSettings dataset.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeckoProviderQuery {
    pub game_id: String,
    pub region: GeckoRegion,
    pub revision: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeckoRegion {
    Usa,
    Europe,
    Japan,
    Korea,
    Unknown(String),
}

impl GeckoRegion {
    #[must_use]
    pub fn display_name(&self) -> &str {
        match self {
            Self::Usa => "USA",
            Self::Europe => "Europe",
            Self::Japan => "Japan",
            Self::Korea => "Korea",
            Self::Unknown(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeckoRevisionApplicability {
    Any,
    Exact(u16),
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeckoApplicabilityDecision {
    Offer,
    OfferWithWarning,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeckoProviderEntry {
    /// Dolphin upstream does not publish entry IDs, so this is a deterministic digest of the
    /// provider ID, exact game ID, entry name, and complete code body.
    pub provider_entry_id: String,
    pub name: String,
    pub code_lines: Vec<String>,
    pub notes: Vec<String>,
    pub region: GeckoRegion,
    pub revision_applicability: GeckoRevisionApplicability,
    pub parse_warnings: Vec<String>,
    pub safe_to_offer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeckoProviderResult {
    pub provider_id: String,
    pub provider_display_name: String,
    pub source_identity: String,
    pub retrieved_at_unix_seconds: u64,
    pub game_id: String,
    pub title: Option<String>,
    pub region: GeckoRegion,
    pub revision: u16,
    pub entries: Vec<GeckoProviderEntry>,
    pub warnings: Vec<String>,
    pub attribution: String,
    pub license: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeckoProviderErrorKind {
    InvalidGameId,
    RegionMismatch,
    ResponseNotUtf8,
    ResponseIdentityMissing,
    ResponseIdentityMismatch,
    NoGeckoEntries,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeckoProviderError {
    pub kind: GeckoProviderErrorKind,
    pub detail: String,
}

impl std::fmt::Display for GeckoProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for GeckoProviderError {}

fn provider_error(kind: GeckoProviderErrorKind, detail: impl Into<String>) -> GeckoProviderError {
    GeckoProviderError {
        kind,
        detail: detail.into(),
    }
}

pub trait GeckoCodeProvider {
    fn provider_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn source_url(&self, query: &GeckoProviderQuery) -> Result<String, GeckoProviderError>;
    fn parse_response(
        &self,
        query: &GeckoProviderQuery,
        source_identity: &str,
        retrieved_at_unix_seconds: u64,
        bytes: &[u8],
    ) -> Result<GeckoProviderResult, GeckoProviderError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DolphinUpstreamGeckoProvider;

impl GeckoCodeProvider for DolphinUpstreamGeckoProvider {
    fn provider_id(&self) -> &'static str {
        DOLPHIN_UPSTREAM_PROVIDER_ID
    }

    fn display_name(&self) -> &'static str {
        DOLPHIN_UPSTREAM_PROVIDER_NAME
    }

    fn source_url(&self, query: &GeckoProviderQuery) -> Result<String, GeckoProviderError> {
        validate_query(query)?;
        Ok(format!(
            "https://raw.githubusercontent.com/{DOLPHIN_UPSTREAM_REPOSITORY}/master/Data/Sys/GameSettings/{}.ini",
            query.game_id
        ))
    }

    fn parse_response(
        &self,
        query: &GeckoProviderQuery,
        source_identity: &str,
        retrieved_at_unix_seconds: u64,
        bytes: &[u8],
    ) -> Result<GeckoProviderResult, GeckoProviderError> {
        validate_query(query)?;
        let text = std::str::from_utf8(bytes).map_err(|_| {
            provider_error(
                GeckoProviderErrorKind::ResponseNotUtf8,
                "Dolphin upstream response is not valid UTF-8",
            )
        })?;
        let (response_game_id, title) = response_identity(text).ok_or_else(|| {
            provider_error(
                GeckoProviderErrorKind::ResponseIdentityMissing,
                "Dolphin upstream response does not declare its game ID in the leading comment",
            )
        })?;
        if response_game_id != query.game_id {
            return Err(provider_error(
                GeckoProviderErrorKind::ResponseIdentityMismatch,
                format!(
                    "requested exact game ID {}, but the response declares {response_game_id}",
                    query.game_id
                ),
            ));
        }

        let document = parse_dolphin_ini(text);
        if document.gecko_codes.is_empty() {
            return Err(provider_error(
                GeckoProviderErrorKind::NoGeckoEntries,
                format!(
                    "Dolphin upstream has no Gecko entries for {}",
                    query.game_id
                ),
            ));
        }

        let mut duplicate_names: BTreeMap<String, usize> = BTreeMap::new();
        for code in &document.gecko_codes {
            *duplicate_names.entry(code.name.clone()).or_default() += 1;
        }
        let mut entries = Vec::with_capacity(document.gecko_codes.len());
        for code in document.gecko_codes {
            let mut parse_warnings: Vec<String> = code
                .warnings
                .iter()
                .map(|warning| warning.detail.clone())
                .collect();
            let duplicate_name = duplicate_names
                .get(code.name.as_str())
                .copied()
                .unwrap_or(0)
                > 1;
            if duplicate_name {
                parse_warnings.push(format!(
                    "duplicate Gecko name {:?} is ambiguous and cannot be installed safely",
                    code.name
                ));
            }
            parse_warnings.push(
                "Upstream does not declare disc-revision applicability; review before enabling."
                    .to_string(),
            );
            let safe_to_offer = code.is_selectable() && !duplicate_name;
            entries.push(GeckoProviderEntry {
                provider_entry_id: stable_entry_id(&query.game_id, &code.name, &code.lines),
                name: code.name,
                code_lines: code.lines,
                notes: code.notes,
                region: query.region.clone(),
                revision_applicability: GeckoRevisionApplicability::Uncertain,
                parse_warnings,
                safe_to_offer,
            });
        }

        let blocked = entries.iter().filter(|entry| !entry.safe_to_offer).count();
        let mut warnings = vec![
            "Dolphin upstream identifies this file by exact game ID and region, but does not declare which disc revision each Gecko entry supports."
                .to_string(),
        ];
        if blocked > 0 {
            warnings.push(format!(
                "{blocked} malformed or ambiguous Gecko entr{} blocked",
                if blocked == 1 { "y was" } else { "ies were" }
            ));
        }

        Ok(GeckoProviderResult {
            provider_id: self.provider_id().to_string(),
            provider_display_name: self.display_name().to_string(),
            source_identity: source_identity.to_string(),
            retrieved_at_unix_seconds,
            game_id: query.game_id.clone(),
            title,
            region: query.region.clone(),
            revision: query.revision,
            entries,
            warnings,
            attribution: DOLPHIN_UPSTREAM_ATTRIBUTION.to_string(),
            license: DOLPHIN_UPSTREAM_LICENSE.to_string(),
        })
    }
}

#[must_use]
pub fn revision_applicability(
    applicability: GeckoRevisionApplicability,
    revision: u16,
) -> GeckoApplicabilityDecision {
    match applicability {
        GeckoRevisionApplicability::Any => GeckoApplicabilityDecision::Offer,
        GeckoRevisionApplicability::Exact(expected) if expected == revision => {
            GeckoApplicabilityDecision::Offer
        }
        GeckoRevisionApplicability::Exact(_) => GeckoApplicabilityDecision::Reject,
        GeckoRevisionApplicability::Uncertain => GeckoApplicabilityDecision::OfferWithWarning,
    }
}

#[must_use]
pub fn region_for_game_id(game_id: &str) -> Option<GeckoRegion> {
    if !valid_game_id(game_id) {
        return None;
    }
    match game_id.as_bytes().get(3).copied()? {
        b'E' => Some(GeckoRegion::Usa),
        b'P' | b'D' | b'F' | b'I' | b'S' | b'H' | b'X' | b'Y' | b'Z' => Some(GeckoRegion::Europe),
        b'J' => Some(GeckoRegion::Japan),
        b'K' | b'Q' | b'T' => Some(GeckoRegion::Korea),
        other => Some(GeckoRegion::Unknown(char::from(other).to_string())),
    }
}

fn validate_query(query: &GeckoProviderQuery) -> Result<(), GeckoProviderError> {
    if !valid_game_id(&query.game_id) {
        return Err(provider_error(
            GeckoProviderErrorKind::InvalidGameId,
            "provider lookup requires an exact six-character ASCII GameCube game ID",
        ));
    }
    let encoded_region = region_for_game_id(&query.game_id).expect("validated game ID has region");
    if encoded_region != query.region {
        return Err(provider_error(
            GeckoProviderErrorKind::RegionMismatch,
            format!(
                "game ID {} encodes region {}, not {}",
                query.game_id,
                encoded_region.display_name(),
                query.region.display_name()
            ),
        ));
    }
    Ok(())
}

fn valid_game_id(game_id: &str) -> bool {
    game_id.len() == 6
        && game_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn response_identity(text: &str) -> Option<(String, Option<String>)> {
    let first = text.lines().find(|line| !line.trim().is_empty())?.trim();
    let declaration = first.strip_prefix('#')?.trim();
    let (game_id, title) = declaration
        .split_once(" - ")
        .map_or((declaration, None), |(game_id, title)| {
            (game_id.trim(), Some(title.trim().to_string()))
        });
    valid_game_id(game_id).then(|| (game_id.to_string(), title))
}

fn stable_entry_id(game_id: &str, name: &str, lines: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DOLPHIN_UPSTREAM_PROVIDER_ID.as_bytes());
    hasher.update([0]);
    hasher.update(game_id.as_bytes());
    hasher.update([0]);
    hasher.update(name.as_bytes());
    for line in lines {
        hasher.update([0]);
        hasher.update(line.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAFE01: &str = include_str!("../../tests/fixtures/dolphin_upstream/GAFE01.ini");

    fn query() -> GeckoProviderQuery {
        GeckoProviderQuery {
            game_id: "GAFE01".to_string(),
            region: GeckoRegion::Usa,
            revision: 0,
        }
    }

    #[test]
    fn recorded_gafe01_response_parses_complete_real_gecko_body() {
        let provider = DolphinUpstreamGeckoProvider;
        let result = provider
            .parse_response(
                &query(),
                "fixture:GAFE01.ini",
                1_721_000_000,
                GAFE01.as_bytes(),
            )
            .expect("recorded upstream response parses");

        assert_eq!(result.game_id, "GAFE01");
        assert_eq!(result.title.as_deref(), Some("Animal Crossing"));
        assert_eq!(result.region, GeckoRegion::Usa);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name, "16:9 Widescreen");
        assert_eq!(
            result.entries[0].code_lines,
            [
                "040037A0 3C608000",
                "040037A4 C38337AC",
                "040037A8 4805ACBC",
                "040037AC 3FE38E39",
                "0405E460 4BFA5340",
            ]
        );
        assert!(result.entries[0].safe_to_offer);
        assert_eq!(
            result.entries[0].revision_applicability,
            GeckoRevisionApplicability::Uncertain
        );
    }

    #[test]
    fn exact_game_id_and_region_are_mandatory() {
        let provider = DolphinUpstreamGeckoProvider;
        let mismatch = GAFE01.replacen("GAFE01", "GAFP01", 1);
        let error = provider
            .parse_response(&query(), "fixture:mismatch", 1, mismatch.as_bytes())
            .expect_err("wrong response identity must fail");
        assert_eq!(error.kind, GeckoProviderErrorKind::ResponseIdentityMismatch);

        let mut wrong_region = query();
        wrong_region.region = GeckoRegion::Europe;
        let error = provider
            .source_url(&wrong_region)
            .expect_err("wrong query region must fail");
        assert_eq!(error.kind, GeckoProviderErrorKind::RegionMismatch);
    }

    #[test]
    fn revision_specific_entries_are_filtered_and_uncertain_entries_warn() {
        assert_eq!(
            revision_applicability(GeckoRevisionApplicability::Exact(0), 0),
            GeckoApplicabilityDecision::Offer
        );
        assert_eq!(
            revision_applicability(GeckoRevisionApplicability::Exact(1), 0),
            GeckoApplicabilityDecision::Reject
        );
        assert_eq!(
            revision_applicability(GeckoRevisionApplicability::Uncertain, 0),
            GeckoApplicabilityDecision::OfferWithWarning
        );
    }

    #[test]
    fn malformed_and_duplicate_code_bodies_are_blocked_not_repaired() {
        let provider = DolphinUpstreamGeckoProvider;
        let body = "# GAFE01 - Animal Crossing\n[Gecko]\n$Bad\nnot code\n$Same\n040037A0 3C608000\n$Same\n040037A4 C38337AC\n";
        let result = provider
            .parse_response(&query(), "fixture:bad", 1, body.as_bytes())
            .expect("document remains inspectable");
        assert_eq!(result.entries.len(), 3);
        assert!(result.entries.iter().all(|entry| !entry.safe_to_offer));
        assert!(result.entries[0].code_lines.is_empty());
        assert!(
            result.entries[0]
                .parse_warnings
                .iter()
                .any(|warning| warning.contains("not a valid"))
        );
    }
}
