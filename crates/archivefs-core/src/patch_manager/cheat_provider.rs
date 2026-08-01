//! Shared, storage-neutral vocabulary for optional cheat catalogue sources.
//!
//! This module deliberately stops at read-only discovery and browsing. A
//! provider may be SQLite-backed (BSFree/possible future CheatBase), a file
//! tree (possible future Libretro), or something else entirely. Installation
//! capabilities do not belong in this interface.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatProviderIdentity {
    pub id: String,
    pub display_name: String,
    pub upstream_project: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatProviderSourceState {
    NotInstalled,
    Downloading,
    Validating,
    Ready,
    UpdateAvailable,
    Invalid,
    UnsupportedSchema,
    DownloadFailed,
    ValidationFailed,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatProviderProvenance {
    pub source: String,
    pub maintainer: String,
    pub origin: String,
    pub distribution_status: String,
    pub verification: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheatProviderLicenceStatus {
    Established,
    NotEstablished,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheatProviderLicence {
    pub status: CheatProviderLicenceStatus,
    pub statement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImmutableSourceFingerprint {
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderValidationStatus {
    Valid,
    Invalid,
    UnsupportedSchema,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderValidationResult {
    pub status: ProviderValidationStatus,
    pub validated_at_unix_seconds: u64,
    pub schema_fingerprint: Option<String>,
    pub source_fingerprint: ImmutableSourceFingerprint,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformMappingStatus {
    Exact,
    Alias,
    Ambiguous,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPlatformMapping {
    pub upstream_id: i64,
    pub upstream_name: String,
    pub archivefs_platform_id: Option<String>,
    /// Human-facing name resolved from ArchiveFS's one canonical registry.
    /// Providers never maintain a second display-name table.
    pub archivefs_platform_display_name: Option<String>,
    pub status: PlatformMappingStatus,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceFormatCompatibility {
    DirectlyInstallable,
    PotentiallyConvertible,
    ReferenceOnly,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDeviceMapping {
    pub upstream_id: i64,
    pub upstream_name: String,
    pub compatibility: DeviceFormatCompatibility,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderGameMatchConfidence {
    ExactHashPlatform,
    ExactSerialPlatformRegion,
    ExactUpstreamRelease,
    ExactTitlePlatformRegionRevision,
    ExactTitlePlatform,
    ProbableTitlePlatform,
    Ambiguous,
    NoMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    pub offset: u32,
    pub limit: u16,
}

impl PageRequest {
    pub const DEFAULT_GAME_LIMIT: u16 = 50;
    pub const DEFAULT_CHEAT_LIMIT: u16 = 100;
    pub const HARD_LIMIT: u16 = 500;

    #[must_use]
    pub fn games(offset: u32) -> Self {
        Self {
            offset,
            limit: Self::DEFAULT_GAME_LIMIT,
        }
    }

    #[must_use]
    pub fn cheats(offset: u32) -> Self {
        Self {
            offset,
            limit: Self::DEFAULT_CHEAT_LIMIT,
        }
    }

    #[must_use]
    pub fn bounded(self) -> Self {
        Self {
            limit: self.limit.clamp(1, Self::HARD_LIMIT),
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPage<T> {
    pub offset: u32,
    pub limit: u16,
    pub total: u64,
    pub rows: Vec<T>,
    pub has_more: bool,
}

/// Small common boundary future providers can implement without inheriting
/// BSFree's SQLite schema or any installation behavior.
pub trait ReadOnlyCheatCatalogue {
    type System;
    type Device;
    type Game;
    type Cheat;
    type Error;

    fn identity(&self) -> CheatProviderIdentity;
    fn systems(&self, page: PageRequest) -> Result<ProviderPage<Self::System>, Self::Error>;
    fn devices(&self, page: PageRequest) -> Result<ProviderPage<Self::Device>, Self::Error>;
    fn game(&self, upstream_uid: i64) -> Result<Option<Self::Game>, Self::Error>;
    fn cheats(
        &self,
        upstream_uid: i64,
        page: PageRequest,
    ) -> Result<ProviderPage<Self::Cheat>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_is_bounded_for_every_provider_storage_kind() {
        let request = PageRequest {
            offset: 42,
            limit: u16::MAX,
        }
        .bounded();
        assert_eq!(request.offset, 42);
        assert_eq!(request.limit, PageRequest::HARD_LIMIT);
        assert_eq!(
            PageRequest {
                offset: 0,
                limit: 0
            }
            .bounded()
            .limit,
            1
        );
    }

    #[test]
    fn shared_source_states_cover_non_network_and_failure_lifecycles() {
        let json = serde_json::to_value([
            CheatProviderSourceState::NotInstalled,
            CheatProviderSourceState::Ready,
            CheatProviderSourceState::UnsupportedSchema,
            CheatProviderSourceState::Disabled,
        ])
        .unwrap();
        assert_eq!(json[0], "not_installed");
        assert_eq!(json[2], "unsupported_schema");
    }
}
