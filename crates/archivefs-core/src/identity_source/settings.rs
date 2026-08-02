//! Persisting a provider's non-secret configuration, and loading its token.
//!
//! Two files, deliberately separate:
//!
//! - `config.json` holds the URL, the enabled flag, the mappings and the page
//!   size. No secret is ever in it - [`RommSourceConfig`] serialises without the
//!   token, and this module never writes one.
//! - the token lives wherever the person put it, referenced by path only.
//!
//! Keeping them apart is what makes it safe to read, log, diff or attach the
//! configuration: there is nothing in it to leak.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::model::IdentityProvider;
use super::romm::config::{RommSourceConfig, RommToken, TokenRefusal};

/// The file name inside the provider's directory.
pub const CONFIG_FILE_NAME: &str = "config.json";

/// The suggested place for a token. Never created automatically - a credential
/// appearing without the person putting it there would be a surprise.
pub const SUGGESTED_TOKEN_PATH: &str = "~/.config/archivefs/romm-token";

/// The largest page size a person may configure. Above the client's own clamp
/// there is nothing to gain, and this makes the refusal explicit rather than
/// silent.
pub const MAX_CONFIGURED_PAGE_SIZE: u32 = 200;
pub const MIN_CONFIGURED_PAGE_SIZE: u32 = 10;

/// Non-secret settings as persisted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSettings {
    #[serde(flatten)]
    pub source: RommSourceConfig,
    /// Preferred page size, within the bounds above. `None` uses the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
}

impl ProviderSettings {
    /// The page size to use, clamped to the safe range whatever is stored.
    pub fn effective_page_size(&self) -> u32 {
        self.page_size
            .unwrap_or(super::romm::import::DEFAULT_PAGE_SIZE)
            .clamp(MIN_CONFIGURED_PAGE_SIZE, MAX_CONFIGURED_PAGE_SIZE)
    }
}

/// Where a provider's settings live.
#[derive(Debug, Clone)]
pub struct SettingsLocation {
    directory: PathBuf,
}

impl SettingsLocation {
    pub fn new(identity_root: &Path, provider: IdentityProvider) -> Self {
        Self {
            directory: identity_root.join(provider.slug()),
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.directory.join(CONFIG_FILE_NAME)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Reads the settings, or the default when nothing is stored.
    ///
    /// A corrupt file is an error rather than a silent reset: quietly forgetting
    /// someone's configuration would be worse than telling them it is broken.
    pub fn load(&self) -> Result<ProviderSettings, SettingsError> {
        match fs::read(self.config_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| SettingsError::Corrupt {
                path: self.config_path(),
                detail: format!("invalid JSON at line {}", error.line()),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ProviderSettings::default())
            }
            Err(error) => Err(SettingsError::Unreadable {
                path: self.config_path(),
                detail: error.kind().to_string(),
            }),
        }
    }

    /// Writes the settings atomically, so an interrupted save cannot leave a
    /// half-written configuration.
    pub fn save(&self, settings: &ProviderSettings) -> Result<PathBuf, SettingsError> {
        fs::create_dir_all(&self.directory).map_err(|error| SettingsError::WriteFailed {
            path: self.directory.clone(),
            detail: error.kind().to_string(),
        })?;
        let bytes =
            serde_json::to_vec_pretty(settings).map_err(|error| SettingsError::WriteFailed {
                path: self.config_path(),
                detail: error.to_string(),
            })?;
        // A last check that nothing secret is being written. Cheap, and it makes
        // the promise structural rather than a matter of reviewing every field.
        let text = String::from_utf8_lossy(&bytes);
        debug_assert!(
            !text.contains("Bearer "),
            "provider settings must never contain a credential"
        );

        let temporary = self
            .directory
            .join(format!(".{CONFIG_FILE_NAME}-{}.tmp", std::process::id()));
        let write = (|| -> std::io::Result<()> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.flush()?;
            file.sync_all()
        })();
        if let Err(error) = write {
            let _ = fs::remove_file(&temporary);
            return Err(SettingsError::WriteFailed {
                path: temporary,
                detail: error.kind().to_string(),
            });
        }
        fs::rename(&temporary, self.config_path()).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            SettingsError::WriteFailed {
                path: self.config_path(),
                detail: error.kind().to_string(),
            }
        })?;
        Ok(self.config_path())
    }

    /// Removes the stored settings. Used by `remove`, alongside the cache.
    pub fn remove(&self) -> std::io::Result<bool> {
        match fs::remove_file(self.config_path()) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum SettingsError {
    Corrupt { path: PathBuf, detail: String },
    Unreadable { path: PathBuf, detail: String },
    WriteFailed { path: PathBuf, detail: String },
}

impl SettingsError {
    pub fn detail(&self) -> String {
        match self {
            Self::Corrupt { path, detail } => format!(
                "{} is not valid provider configuration ({detail}); fix or delete it and \
                 configure the source again",
                path.display()
            ),
            Self::Unreadable { path, detail } => {
                format!("{} could not be read: {detail}", path.display())
            }
            Self::WriteFailed { path, detail } => {
                format!("{} could not be written: {detail}", path.display())
            }
        }
    }
}

/// Why a token file was refused.
///
/// Deliberately never carries the file's contents - only facts about the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum TokenFileRefusal {
    /// No token file has been configured yet.
    NotConfigured {
        suggested: String,
    },
    Missing {
        path: PathBuf,
    },
    /// A symlink. Refused because a credential's location must be exactly where
    /// the person said, not wherever a link now points.
    IsSymlink {
        path: PathBuf,
    },
    NotRegularFile {
        path: PathBuf,
    },
    /// Readable by the group or by everyone.
    PermissionsTooOpen {
        path: PathBuf,
        mode: u32,
    },
    TooLarge {
        path: PathBuf,
        bytes: u64,
    },
    Unreadable {
        path: PathBuf,
        detail: String,
    },
    /// The file exists but holds nothing usable.
    Invalid {
        path: PathBuf,
        refusal: TokenRefusal,
    },
}

impl TokenFileRefusal {
    pub fn detail(&self) -> String {
        match self {
            Self::NotConfigured { suggested } => format!(
                "no token file is configured; create a read-only RomM client token and point \
                 `--token-file` at it (suggested location {suggested})"
            ),
            Self::Missing { path } => format!("{} does not exist", path.display()),
            Self::IsSymlink { path } => format!(
                "{} is a symlink; a credential must be a regular file at the exact path given, \
                 so that where it lives cannot change without you moving it",
                path.display()
            ),
            Self::NotRegularFile { path } => {
                format!("{} is not a regular file", path.display())
            }
            Self::PermissionsTooOpen { path, mode } => format!(
                "{} is readable by others (mode {:04o}); run `chmod 600 {}` so only you can read \
                 the token",
                path.display(),
                mode & 0o7777,
                path.display()
            ),
            Self::TooLarge { path, bytes } => format!(
                "{} is {bytes} bytes, which is far larger than a token; check the path",
                path.display()
            ),
            Self::Unreadable { path, detail } => {
                format!("{} could not be read: {detail}", path.display())
            }
            // The inner refusal explains the shape without echoing the value.
            Self::Invalid { path, refusal } => {
                format!(
                    "{} does not contain a usable token: {}",
                    path.display(),
                    refusal.detail()
                )
            }
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured { .. } => "token_not_configured",
            Self::Missing { .. } => "token_missing",
            Self::IsSymlink { .. } => "token_symlink",
            Self::NotRegularFile { .. } => "token_not_regular_file",
            Self::PermissionsTooOpen { .. } => "token_permissions_too_open",
            Self::TooLarge { .. } => "token_too_large",
            Self::Unreadable { .. } => "token_unreadable",
            Self::Invalid { .. } => "token_invalid",
        }
    }
}

/// The largest a token file may be. A real token is well under a hundred bytes;
/// this only stops a wrong path from being slurped.
pub const MAX_TOKEN_FILE_BYTES: u64 = 8 * 1024;

/// Loads a token from a file, applying every check before reading it.
///
/// The order matters: the file's type and permissions are established before its
/// contents are read, so an unsafe file is refused rather than read and then
/// complained about.
pub fn load_token_file(path: Option<&Path>) -> Result<RommToken, TokenFileRefusal> {
    let Some(path) = path else {
        return Err(TokenFileRefusal::NotConfigured {
            suggested: SUGGESTED_TOKEN_PATH.to_string(),
        });
    };
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(TokenFileRefusal::Missing {
                path: path.to_path_buf(),
            });
        }
        Err(error) => {
            return Err(TokenFileRefusal::Unreadable {
                path: path.to_path_buf(),
                detail: error.kind().to_string(),
            });
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(TokenFileRefusal::IsSymlink {
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_file() {
        return Err(TokenFileRefusal::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() > MAX_TOKEN_FILE_BYTES {
        return Err(TokenFileRefusal::TooLarge {
            path: path.to_path_buf(),
            bytes: metadata.len(),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        // Any group or world bit at all is too open for a credential.
        if mode & 0o077 != 0 {
            return Err(TokenFileRefusal::PermissionsTooOpen {
                path: path.to_path_buf(),
                mode,
            });
        }
    }

    let contents = fs::read_to_string(path).map_err(|error| TokenFileRefusal::Unreadable {
        path: path.to_path_buf(),
        detail: error.kind().to_string(),
    })?;
    // Exactly one trailing newline is removed, because `printf` and a text editor
    // disagree about whether to add one and a person should not have to care.
    // Anything else is left for the token parser to refuse.
    let trimmed = contents
        .strip_suffix('\n')
        .map(|value| value.strip_suffix('\r').unwrap_or(value))
        .unwrap_or(&contents);
    RommToken::parse(trimmed).map_err(|refusal| TokenFileRefusal::Invalid {
        path: path.to_path_buf(),
        refusal,
    })
}

/// The default identity root: `<data dir>/identity`, beside the library
/// database and the other ArchiveFS-owned caches.
pub fn default_identity_root() -> Result<PathBuf, String> {
    let database = crate::database::default_database_path().map_err(|error| error.to_string())?;
    Ok(database
        .parent()
        .ok_or_else(|| "the data directory could not be resolved".to_string())?
        .join("identity"))
}
