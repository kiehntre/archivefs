//! What a person configured for a RomM source, including the token.
//!
//! # Secret handling, and the gap this had to work around
//!
//! EmuWiz has no secret store. There is no keyring integration, no encrypted
//! settings file, and no existing convention for holding a credential - the
//! GameHacking providers only ever fetch public pages, so the question had never
//! come up. Inventing a home-grown encryption scheme here would be worse than
//! useless: the key would have to live next to the ciphertext, so it would offer
//! the appearance of protection without the substance.
//!
//! So the interim design is deliberately plain about what it is:
//!
//! - the token is held in memory in [`RommToken`], whose `Debug`, `Display` and
//!   `Serialize` implementations all redact it, so it cannot reach a log, an
//!   error string, a diagnostic or the cache by accident;
//! - it is persisted, if at all, to a file the caller names with `0600`
//!   permissions, and [`RommSourceConfig`] serialises *without* it, so the
//!   token never appears in ordinary configuration;
//! - the persisted file is documented as plaintext-on-a-trusted-filesystem, and
//!   the GUI and CLI say so where a person chooses to save it.
//!
//! That is the safest bounded thing available without a real secret store, and
//! it is recorded here rather than being quietly assumed. A proper store is a
//! separate piece of work, and this type is the single place it would land.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::identity_source::net_policy::{
    ApprovedEndpoint, EndpointRefusal, HostResolver, validate_endpoint,
};
use crate::identity_source::path_map::{
    MappingRefusal, PathMapping, PathMappings, ProviderPathKind,
};

/// The longest token this will accept, so a paste accident cannot become an
/// unbounded header.
pub const MAX_TOKEN_BYTES: usize = 4096;

/// The read scopes Stage 1 needs. Recorded so a person can create a token with
/// exactly these and no more, and so a capability report can say whether the
/// token it was given actually carries them.
///
/// Read from the real instance's OpenAPI security requirements: `/api/platforms`
/// declares `platforms.read` and `/api/roms` declares `roms.read`.
pub const REQUIRED_READ_SCOPES: &[&str] = &["platforms.read", "roms.read"];

/// Scopes that would let EmuWiz change something in RomM. Never requested,
/// and reported as a warning if the supplied token happens to carry them, so a
/// person can narrow it.
pub const UNWANTED_WRITE_SCOPES: &[&str] = &[
    "roms.write",
    "platforms.write",
    "assets.write",
    "collections.write",
    "users.write",
    "me.write",
    "firmware.write",
    "tasks.run",
];

/// A RomM client token.
///
/// Every way of rendering this type redacts it. That is the whole point: the
/// token is used in exactly one place - building an `Authorization` header - and
/// there is no accessor that returns it as a plain `String`, only
/// [`RommToken::with_header_value`], which hands a borrowed value to a closure.
#[derive(Clone, PartialEq, Eq)]
pub struct RommToken {
    raw: String,
}

impl RommToken {
    /// Accepts a token, rejecting shapes that cannot be one.
    pub fn parse(raw: &str) -> Result<Self, TokenRefusal> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(TokenRefusal::Empty);
        }
        if trimmed.len() > MAX_TOKEN_BYTES {
            return Err(TokenRefusal::TooLong {
                bytes: trimmed.len(),
                maximum: MAX_TOKEN_BYTES,
            });
        }
        // A header value cannot contain controls or non-ASCII, and refusing them
        // here means the request builder can never produce a split header.
        if trimmed
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || !ch.is_ascii())
        {
            return Err(TokenRefusal::InvalidCharacters);
        }
        Ok(Self {
            raw: trimmed.to_string(),
        })
    }

    /// Runs `use_value` with the `Authorization` header value.
    ///
    /// The only way the secret leaves this type, and it never leaves as an owned
    /// `String`, so it cannot be stored or formatted somewhere else by accident.
    pub fn with_header_value<T>(&self, use_value: impl FnOnce(&str) -> T) -> T {
        use_value(&format!("Bearer {}", self.raw))
    }

    /// A stable, non-secret fingerprint, so two configurations can be compared
    /// and a diagnostic can say "the same token as before" without revealing it.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.raw.as_bytes());
        digest
            .iter()
            .take(4)
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// What a person sees: a length and a fingerprint, never the value.
    pub fn redacted(&self) -> String {
        format!(
            "(redacted, {} chars, id {})",
            self.raw.len(),
            self.fingerprint()
        )
    }

    /// Writes the token to `path` with owner-only permissions.
    ///
    /// Plaintext, and documented as such - see the module notes. Returns the
    /// path so a caller can tell a person exactly where it went.
    pub fn persist_to(&self, path: &Path) -> std::io::Result<PathBuf> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(self.raw.as_bytes())?;
        file.flush()?;
        // Set the mode again in case the file already existed with wider
        // permissions: `mode` on `OpenOptions` only applies at creation.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(path.to_path_buf())
    }

    /// Reads a token previously persisted.
    pub fn load_from(path: &Path) -> std::io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(Self::parse(&contents).ok()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

// Every rendering redacts. Written out rather than derived so it cannot be
// derived back by accident.
impl fmt::Debug for RommToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RommToken")
            .field("value", &"(redacted)")
            .field("id", &self.fingerprint())
            .finish()
    }
}

impl fmt::Display for RommToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.redacted())
    }
}

impl Serialize for RommToken {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Never the value. A cache or a settings file that somehow reaches this
        // type gets the redaction, not the secret.
        serializer.serialize_str("(redacted)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum TokenRefusal {
    Empty,
    TooLong { bytes: usize, maximum: usize },
    InvalidCharacters,
}

impl TokenRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::Empty => "a read-only RomM client token is required".to_string(),
            Self::TooLong { bytes, maximum } => {
                format!("that token is {bytes} characters, over the {maximum} limit")
            }
            Self::InvalidCharacters => {
                "a token must be printable ASCII with no spaces; check for a stray newline or a \
                 partial paste"
                    .to_string()
            }
        }
    }
}

/// A configured RomM source, as persisted.
///
/// The token is deliberately absent from this type's serialised form - it is
/// held separately in [`RommToken`] - so ordinary configuration can be written,
/// read, logged and diffed without ever carrying a secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommSourceConfig {
    /// Whether the source is enabled.
    ///
    /// Defaults to `false`, which is the whole reason this is persisted: Stage 1
    /// never connects during startup unless someone has already turned the
    /// source on, so a fresh install makes no network request of any kind.
    pub enabled: bool,
    /// The address a person entered, before validation.
    pub url: String,
    pub mappings: Vec<PathMapping>,
    /// Which shape of path this instance reports.
    ///
    /// Declared, never inferred per path. Absent in a configuration written
    /// before the setting existed, which means absolute - the only shape those
    /// mappings could have been.
    #[serde(default)]
    pub provider_path_kind: ProviderPathKind,
    /// Where the token was persisted, if the person chose to save it.
    pub token_path: Option<PathBuf>,
}

/// A configuration that has passed every check, and is therefore usable.
///
/// Holding one means the endpoint was resolved and approved, the mappings were
/// validated, and a token is present. There is no way to build one otherwise.
#[derive(Debug, Clone)]
pub struct ValidatedRommSource {
    endpoint: ApprovedEndpoint,
    mappings: PathMappings,
    token: RommToken,
}

/// Why a configuration cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ConfigRefusal {
    Endpoint(EndpointRefusal),
    Mapping(MappingRefusal),
    Token(TokenRefusal),
    Disabled,
}

impl ConfigRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::Endpoint(refusal) => refusal.detail(),
            Self::Mapping(refusal) => refusal.detail(),
            Self::Token(refusal) => refusal.detail(),
            Self::Disabled => "the RomM source is disabled".to_string(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Endpoint(refusal) => refusal.code(),
            Self::Mapping(refusal) => refusal.code(),
            Self::Token(_) => "token",
            Self::Disabled => "disabled",
        }
    }
}

impl ValidatedRommSource {
    /// Validates everything a request needs, in one place.
    ///
    /// `trusted_roots` are the configured source folders; every mapping
    /// destination must be inside one.
    pub fn validate(
        config: &RommSourceConfig,
        token: &RommToken,
        trusted_roots: &[PathBuf],
        resolver: &impl HostResolver,
    ) -> Result<Self, ConfigRefusal> {
        let endpoint = validate_endpoint(&config.url, resolver).map_err(ConfigRefusal::Endpoint)?;
        let mappings =
            PathMappings::validate(&config.mappings, trusted_roots, config.provider_path_kind)
                .map_err(ConfigRefusal::Mapping)?;
        Ok(Self {
            endpoint,
            mappings,
            token: token.clone(),
        })
    }

    pub fn endpoint(&self) -> &ApprovedEndpoint {
        &self.endpoint
    }

    pub fn mappings(&self) -> &PathMappings {
        &self.mappings
    }

    pub fn token(&self) -> &RommToken {
        &self.token
    }

    /// The stable, non-secret identifier for this instance, used to keep two
    /// servers' cached records apart. The approved origin - never the token.
    pub fn server_id(&self) -> &str {
        self.endpoint.origin()
    }
}
