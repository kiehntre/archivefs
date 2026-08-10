//! What a RomM instance says about itself.
//!
//! Nothing here is assumed from a release. The adapter asks the instance and
//! records the answer, so an older or newer RomM is handled by reporting what it
//! can do rather than by failing on a hard-coded expectation.
//!
//! Two documents are read, both bounded:
//!
//! - `/api/heartbeat`, which needs no authentication and carries the version and
//!   the filesystem platform list. This is what a connection test uses, so a
//!   person can check the address before a token exists.
//! - `/openapi.json`, which is how the presence and shape of the endpoints this
//!   adapter needs is *verified* rather than presumed - including which read
//!   scopes each one declares.

use serde::{Deserialize, Serialize};

/// The RomM releases this adapter has been checked against.
///
/// A version outside this range is not refused: it is reported, and the
/// capability flags below decide what is actually attempted. Refusing an unknown
/// version would make every RomM upgrade an EmuWiz outage.
pub const VERIFIED_AGAINST: &str = "5.1.0";

/// The oldest major version whose endpoint shape this adapter understands.
pub const MINIMUM_SUPPORTED_MAJOR: u32 = 4;

/// The endpoints Stage 1 uses, with the scope each one declares.
pub const REQUIRED_ENDPOINTS: &[(&str, &str)] = &[
    ("/api/platforms", "platforms.read"),
    ("/api/roms", "roms.read"),
];

/// A parsed `/api/heartbeat`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommHeartbeat {
    /// `SYSTEM.VERSION`.
    pub version: String,
    /// `FILESYSTEM.FS_PLATFORMS` - the platform slugs this instance has on disk.
    /// Useful for a capability summary and for previewing what an import covers.
    pub filesystem_platforms: Vec<String>,
    /// Whether the instance has any metadata provider enabled at all, which is
    /// what decides whether its records carry provider ids.
    pub any_metadata_source_enabled: bool,
}

impl RommHeartbeat {
    /// Parses the heartbeat document, tolerating absent optional sections.
    ///
    /// Only `SYSTEM.VERSION` is required: everything else is a capability that
    /// may be missing on an older instance, and a missing capability is reported
    /// rather than treated as a parse failure.
    pub fn parse(document: &serde_json::Value) -> Option<Self> {
        let version = document
            .get("SYSTEM")?
            .get("VERSION")?
            .as_str()?
            .to_string();
        let filesystem_platforms = document
            .get("FILESYSTEM")
            .and_then(|section| section.get("FS_PLATFORMS"))
            .and_then(|value| value.as_array())
            .map(|slugs| {
                slugs
                    .iter()
                    .filter_map(|slug| slug.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let any_metadata_source_enabled = document
            .get("METADATA_SOURCES")
            .and_then(|section| section.get("ANY_SOURCE_ENABLED"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        Some(Self {
            version,
            filesystem_platforms,
            any_metadata_source_enabled,
        })
    }

    /// The major version, when the string looks like `major.minor.patch`.
    pub fn major_version(&self) -> Option<u32> {
        self.version.split('.').next()?.parse().ok()
    }

    /// Whether this version is one the adapter claims to understand.
    pub fn is_supported(&self) -> bool {
        self.major_version()
            .is_some_and(|major| major >= MINIMUM_SUPPORTED_MAJOR)
    }
}

/// What was verified from the instance's own OpenAPI document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommApiCapability {
    /// `info.version` from the OpenAPI document.
    pub api_version: Option<String>,
    /// Endpoints this adapter needs that the instance actually publishes.
    pub available_endpoints: Vec<String>,
    /// Endpoints this adapter needs that are absent - the reason an import
    /// cannot proceed, named precisely.
    pub missing_endpoints: Vec<String>,
    /// Read scopes the required endpoints declare.
    pub declared_read_scopes: Vec<String>,
    /// Whether `/api/roms` accepts `limit`/`offset`, which is the pagination
    /// model Stage 1 relies on.
    pub supports_limit_offset_pagination: bool,
    /// Hash fields the ROM schema publishes, of those EmuWiz can use.
    pub available_hash_fields: Vec<String>,
    /// Artwork fields the ROM schema publishes.
    pub available_artwork_fields: Vec<String>,
    /// Whether the ROM schema exposes a per-file list, which is what multi-disc
    /// and multi-file relationships are read from.
    pub exposes_file_list: bool,
    /// Whether the instance publishes a client-token facility, so a person can
    /// create a read-only token rather than using a password.
    pub supports_client_tokens: bool,
}

impl RommApiCapability {
    /// Reads the capability facts out of an OpenAPI document.
    ///
    /// Everything is discovered by looking, so a field this adapter would like
    /// but the instance does not publish shows up as absent instead of causing a
    /// failure later during import.
    pub fn from_openapi(document: &serde_json::Value) -> Self {
        let mut capability = Self {
            api_version: document
                .get("info")
                .and_then(|info| info.get("version"))
                .and_then(|value| value.as_str())
                .map(str::to_string),
            ..Self::default()
        };
        let paths = document.get("paths").and_then(serde_json::Value::as_object);

        for (path, scope) in REQUIRED_ENDPOINTS {
            let operation = paths
                .and_then(|paths| paths.get(*path))
                .and_then(|entry| entry.get("get"));
            match operation {
                Some(operation) => {
                    capability.available_endpoints.push((*path).to_string());
                    // The declared scope is read from the document rather than
                    // assumed, so a token can be created with exactly what the
                    // instance asks for.
                    let declared = operation
                        .get("security")
                        .and_then(serde_json::Value::as_array)
                        .map(|requirements| {
                            requirements
                                .iter()
                                .filter_map(serde_json::Value::as_object)
                                .flat_map(|requirement| requirement.values())
                                .filter_map(serde_json::Value::as_array)
                                .flatten()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    for name in declared {
                        if !capability.declared_read_scopes.contains(&name) {
                            capability.declared_read_scopes.push(name);
                        }
                    }
                    if *path == "/api/roms" {
                        let parameters = operation
                            .get("parameters")
                            .and_then(serde_json::Value::as_array)
                            .map(|list| {
                                list.iter()
                                    .filter_map(|parameter| {
                                        parameter.get("name").and_then(|name| name.as_str())
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        capability.supports_limit_offset_pagination =
                            parameters.contains(&"limit") && parameters.contains(&"offset");
                    }
                    let _ = scope;
                }
                None => capability.missing_endpoints.push((*path).to_string()),
            }
        }

        capability.supports_client_tokens =
            paths.is_some_and(|paths| paths.contains_key("/api/client-tokens"));

        // The ROM schema's own field list decides what an import can carry.
        let rom_schema = document
            .get("components")
            .and_then(|components| components.get("schemas"))
            .and_then(|schemas| schemas.get("SimpleRomSchema"))
            .and_then(|schema| schema.get("properties"))
            .and_then(serde_json::Value::as_object);
        if let Some(properties) = rom_schema {
            for field in ["md5_hash", "sha1_hash", "crc_hash"] {
                if properties.contains_key(field) {
                    capability.available_hash_fields.push(field.to_string());
                }
            }
            for field in ["url_cover", "path_cover_small", "path_cover_large"] {
                if properties.contains_key(field) {
                    capability.available_artwork_fields.push(field.to_string());
                }
            }
            capability.exposes_file_list = properties.contains_key("files");
        }
        capability
    }

    /// Whether an import can be attempted at all.
    pub fn can_import(&self) -> bool {
        self.missing_endpoints.is_empty() && self.supports_limit_offset_pagination
    }

    /// Why an import cannot be attempted, when it cannot.
    pub fn blocking_reason(&self) -> Option<String> {
        if !self.missing_endpoints.is_empty() {
            return Some(format!(
                "this RomM instance does not publish {}",
                self.missing_endpoints.join(" or ")
            ));
        }
        if !self.supports_limit_offset_pagination {
            return Some(
                "this RomM instance's ROM endpoint does not accept limit/offset paging, which \
                 EmuWiz needs to import in bounded pages"
                    .to_string(),
            );
        }
        None
    }
}

/// The whole answer to "what is this instance, and can we use it?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RommCapabilityReport {
    /// The approved origin - never a token.
    pub server_id: String,
    pub heartbeat: Option<RommHeartbeat>,
    pub api: RommApiCapability,
    /// Facts safe to show a person. Never contains a credential.
    pub notes: Vec<String>,
}

impl RommCapabilityReport {
    pub fn summary(&self) -> String {
        match &self.heartbeat {
            Some(heartbeat) => format!(
                "RomM {} at {} ({} platforms on disk)",
                heartbeat.version,
                self.server_id,
                heartbeat.filesystem_platforms.len()
            ),
            None => format!("RomM at {} (version not reported)", self.server_id),
        }
    }
}
