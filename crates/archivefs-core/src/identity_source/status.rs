//! Provider status, and the internal API later stages call.
//!
//! Stage 1B builds no CLI and no GUI, but both will need the same handful of
//! operations. They live here so that adding a command or a card is wiring rather
//! than design, and so the rules - a disabled source makes no request, removal
//! needs confirmation, offline browsing never touches the network - are enforced
//! in one place instead of in each caller.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use serde::Serialize;

use super::cache::{
    CacheRefusal, IdentityCache, IdentityCacheLocation, load_cache, publish_cache, remove_cache,
};
use super::hashing::LocalHashCache;
use super::matching::{IdentityGroup, LocalFileFacts, PathClaims, build_groups, match_record};
use super::model::{ExternalIdentityRecord, IdentityImportCounts, IdentityProvider};
use super::romm::capability::RommCapabilityReport;
use super::romm::client::{RommClient, RommRequestError, RommTransport};
use super::romm::config::{RommSourceConfig, ValidatedRommSource};
use super::romm::import::{ImportFailure, ImportOutcome, ImportScope, import_identity};

/// Where a source is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum ProviderState {
    /// No URL, no token, nothing to do.
    NotConfigured,
    /// Configured but switched off. Nothing connects.
    Disabled,
    /// Configured and enabled, but no import has run yet.
    ///
    /// Distinct from both `Ready` and `Error`: there is no cache to serve, and
    /// nothing has gone wrong either. Reporting this as an error told people to
    /// go looking for a fault that did not exist.
    NeverImported,
    /// An import is running.
    Importing,
    /// A cache is published and the instance was reachable at last check.
    Ready,
    /// A cache is published and being served without the instance being
    /// reachable. Not an error: this is the offline case working as intended.
    ReadyOffline,
    /// A cache is published but something about it no longer fits - the mapping
    /// changed, or many records went stale.
    Stale { detail: String },
    /// The last operation failed. A previous cache may still be serving.
    Error { detail: String },
}

impl ProviderState {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotConfigured => "Not configured",
            Self::Disabled => "Disabled",
            Self::NeverImported => "Enabled, nothing imported yet",
            Self::Importing => "Importing",
            Self::Ready => "Ready",
            Self::ReadyOffline => "Ready (offline)",
            Self::Stale { .. } => "Stale",
            Self::Error { .. } => "Error",
        }
    }

    /// Whether cached identity can be served in this state.
    pub fn can_browse(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::ReadyOffline | Self::Stale { .. } | Self::Error { .. }
        )
    }
}

/// Everything a status view needs. Contains no secret: the token appears
/// nowhere, and the server is identified by its approved origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderStatus {
    pub provider: IdentityProvider,
    pub state: ProviderState,
    /// The approved origin, when configured. Never a token.
    pub server_id: Option<String>,
    pub server_version: Option<String>,
    pub platforms_imported: usize,
    pub records_imported: usize,
    pub counts: IdentityImportCounts,
    pub invalid_hashes: usize,
    pub unknown_platforms: usize,
    /// Translated paths claimed by more than one record.
    pub duplicate_mappings: usize,
    pub multi_file_groups: usize,
    pub last_successful_refresh_unix_seconds: Option<i64>,
    pub last_error: Option<String>,
    pub cache_size_bytes: Option<u64>,
    pub cache_path: Option<PathBuf>,
    /// How many records have a locally computed hash available.
    pub locally_verified: usize,
}

impl ProviderStatus {
    /// The status of a source that has never been configured.
    pub fn not_configured(provider: IdentityProvider) -> Self {
        Self {
            provider,
            state: ProviderState::NotConfigured,
            server_id: None,
            server_version: None,
            platforms_imported: 0,
            records_imported: 0,
            counts: IdentityImportCounts::default(),
            invalid_hashes: 0,
            unknown_platforms: 0,
            duplicate_mappings: 0,
            multi_file_groups: 0,
            last_successful_refresh_unix_seconds: None,
            last_error: None,
            cache_size_bytes: None,
            cache_path: None,
            locally_verified: 0,
        }
    }

    /// Builds a status from a published cache.
    pub fn from_cache(
        cache: &IdentityCache,
        location: &IdentityCacheLocation,
        hashes: &LocalHashCache,
        state: ProviderState,
    ) -> Self {
        let claims = PathClaims::of(&cache.records);
        Self {
            provider: cache.provider,
            state,
            server_id: Some(cache.server_id.clone()),
            server_version: cache.server_version.clone(),
            platforms_imported: cache.platforms.len(),
            records_imported: cache.records.len(),
            counts: cache.counts(),
            invalid_hashes: cache.rejected_hashes.len(),
            unknown_platforms: cache.unknown_platforms.len(),
            duplicate_mappings: claims.contested().len(),
            multi_file_groups: build_groups(&cache.records).len(),
            last_successful_refresh_unix_seconds: Some(cache.imported_at_unix_seconds),
            last_error: None,
            cache_size_bytes: location.cache_size_bytes(),
            cache_path: Some(location.cache_path()),
            locally_verified: cache
                .records
                .iter()
                .filter(|record| {
                    record
                        .archivefs_path
                        .as_deref()
                        .is_some_and(|path| hashes.get(path).is_some())
                })
                .count(),
        }
    }
}

/// The internal API later stages call. One place, so the rules hold for every
/// caller.
pub struct IdentitySourceApi {
    location: IdentityCacheLocation,
}

impl IdentitySourceApi {
    pub fn new(identity_root: &Path, provider: IdentityProvider) -> Self {
        Self {
            location: IdentityCacheLocation::new(identity_root, provider),
        }
    }

    pub fn location(&self) -> &IdentityCacheLocation {
        &self.location
    }

    /// Tests the connection and reports capabilities.
    ///
    /// The only operation that contacts the instance without a cache, and it is
    /// deliberately safe to run before a token exists - the heartbeat needs none.
    pub fn test_connection<T: RommTransport>(
        &self,
        source: &ValidatedRommSource,
        transport: &T,
        cancel: Option<&AtomicBool>,
    ) -> Result<RommCapabilityReport, RommRequestError> {
        RommClient::new(source, transport).capability_report(cancel)
    }

    /// Imports, matches and publishes, in that order.
    ///
    /// Nothing touches the live cache until the very last step, and a failure at
    /// any point leaves it untouched.
    ///
    /// The inputs arrive as a [`RefreshRequest`] rather than as eight parameters:
    /// a caller assembling a refresh has to supply a source, a transport, a
    /// capability report, a hash cache and two callbacks, and naming them at the
    /// call site is what makes that legible.
    pub fn refresh<T: RommTransport>(
        &self,
        request: RefreshRequest<'_, T>,
        facts_for: impl FnMut(&ExternalIdentityRecord) -> LocalFileFacts,
        on_progress: impl FnMut(super::romm::import::ImportProgress),
    ) -> Result<RefreshSummary, ImportFailure> {
        let RefreshRequest {
            source,
            transport,
            scope,
            capability,
            hashes,
            cancel,
        } = request;
        let ImportOutcome {
            mut cache,
            progress,
            normalisation,
        } = import_identity(source, transport, scope, capability, on_progress, cancel)?;

        // Matching happens before publication, so a published cache always has
        // verdicts in it and browsing never has to compute them.
        super::matching::match_all(&mut cache.records, hashes, facts_for, cancel)
            .map_err(|_| ImportFailure::Cancelled)?;
        cache.sort_deterministically();

        let path = publish_cache(&self.location, &cache).map_err(ImportFailure::Publish)?;
        // Only after a successful publication is it safe to tidy up.
        let _ = super::cache::clean_temporary_files(&self.location);
        Ok(RefreshSummary {
            cache_path: path,
            counts: cache.counts(),
            platforms: cache.platforms.len(),
            records: cache.records.len(),
            invalid_hashes: normalisation.rejected_hashes.len(),
            unknown_platforms: normalisation.unknown_platforms.len(),
            groups: build_groups(&cache.records),
            progress,
        })
    }

    /// Opens the published cache. Makes no network request, which is what offline
    /// browsing relies on.
    pub fn open_cache(&self, expected_server: Option<&str>) -> Result<IdentityCache, CacheRefusal> {
        load_cache(&self.location, expected_server)
    }

    /// One bounded page of cached records.
    pub fn list_records(
        &self,
        cache: &IdentityCache,
        offset: usize,
        limit: usize,
    ) -> Vec<ExternalIdentityRecord> {
        cache.page(offset, limit).to_vec()
    }

    /// Matches one ArchiveFS path against the cache, for a details view.
    ///
    /// Uses only a cached hash, never computing one, so opening a game's details
    /// cannot start reading a four-gigabyte file.
    pub fn match_path(
        &self,
        cache: &IdentityCache,
        path: &Path,
        facts: &LocalFileFacts,
        hashes: &LocalHashCache,
    ) -> Option<(ExternalIdentityRecord, super::matching::MatchOutcome)> {
        let record = cache.record_for_path(path)?.clone();
        let claims = PathClaims::of(&cache.records);
        let outcome = match_record(&record, facts, &claims, hashes);
        Some((record, outcome))
    }

    /// Every cached record with a conflict.
    pub fn list_conflicts(&self, cache: &IdentityCache) -> Vec<ExternalIdentityRecord> {
        cache.conflicts().into_iter().cloned().collect()
    }

    /// The status summary, working from whatever is on disk.
    ///
    /// `reachable` says whether the instance answered recently, which is the only
    /// difference between `Ready` and `ReadyOffline`. Passing `false` never makes
    /// this contact anything.
    pub fn status(
        &self,
        config: &RommSourceConfig,
        hashes: &LocalHashCache,
        reachable: bool,
    ) -> ProviderStatus {
        if config.url.trim().is_empty() {
            return ProviderStatus::not_configured(IdentityProvider::Romm);
        }
        if !config.enabled {
            let mut status = ProviderStatus::not_configured(IdentityProvider::Romm);
            status.state = ProviderState::Disabled;
            status.cache_size_bytes = self.location.cache_size_bytes();
            return status;
        }
        match self.open_cache(None) {
            Ok(cache) => {
                let counts = cache.counts();
                // A cache whose records are mostly stale is reported as stale
                // rather than as ready: the identity is there, but it no longer
                // describes the library.
                let state = if counts.total > 0 && counts.stale * 2 > counts.total {
                    ProviderState::Stale {
                        detail: format!(
                            "{} of {} records point at files that are missing or changed",
                            counts.stale, counts.total
                        ),
                    }
                } else if reachable {
                    ProviderState::Ready
                } else {
                    ProviderState::ReadyOffline
                };
                ProviderStatus::from_cache(&cache, &self.location, hashes, state)
            }
            Err(CacheRefusal::Missing) => {
                let mut status = ProviderStatus::not_configured(IdentityProvider::Romm);
                // Configured and enabled but never imported. Neither `Ready` -
                // there is no fake ready state before a first import - nor an
                // error, because nothing has failed.
                status.state = ProviderState::NeverImported;
                status
            }
            Err(refusal) => {
                let mut status = ProviderStatus::not_configured(IdentityProvider::Romm);
                status.state = ProviderState::Error {
                    detail: refusal.detail(),
                };
                status.last_error = Some(refusal.detail());
                status.cache_size_bytes = self.location.cache_size_bytes();
                status
            }
        }
    }

    /// Disables the source. Configuration only - the cache is kept, so browsing
    /// can be re-enabled without another import.
    pub fn disable(&self, config: &mut RommSourceConfig) {
        config.enabled = false;
    }

    /// Removes the cached identity.
    ///
    /// `confirmed` is the boundary a CLI or GUI must cross: this function refuses
    /// unless the caller has already asked, so the confirmation cannot be
    /// forgotten in one caller and remembered in another.
    pub fn remove_cached_identity(&self, confirmed: bool) -> Result<bool, RemovalRefusal> {
        if !confirmed {
            return Err(RemovalRefusal::NotConfirmed);
        }
        let removed = remove_cache(&self.location).map_err(|error| RemovalRefusal::Failed {
            detail: error.kind().to_string(),
        })?;
        let _ = super::cache::clean_temporary_files(&self.location);
        Ok(removed)
    }
}

/// Why cached identity was not removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum RemovalRefusal {
    /// The caller did not confirm.
    NotConfirmed,
    Failed {
        detail: String,
    },
}

impl RemovalRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::NotConfirmed => {
                "removing cached identity needs explicit confirmation; nothing was removed"
                    .to_string()
            }
            Self::Failed { detail } => format!("the cache could not be removed: {detail}"),
        }
    }
}

/// Everything one refresh needs.
///
/// A struct rather than a long parameter list, so a call site reads as a
/// description of the operation and a later stage cannot transpose two arguments
/// of the same type.
pub struct RefreshRequest<'a, T: RommTransport> {
    pub source: &'a ValidatedRommSource,
    pub transport: &'a T,
    pub scope: ImportScope,
    pub capability: &'a RommCapabilityReport,
    /// Already-computed local hashes. Never added to by a refresh: matching uses
    /// what is there and does not start hashing.
    pub hashes: &'a LocalHashCache,
    pub cancel: Option<&'a AtomicBool>,
}

/// What a successful refresh produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshSummary {
    pub cache_path: PathBuf,
    pub counts: IdentityImportCounts,
    pub platforms: usize,
    pub records: usize,
    pub invalid_hashes: usize,
    pub unknown_platforms: usize,
    pub groups: Vec<IdentityGroup>,
    pub progress: super::romm::import::ImportProgress,
}
