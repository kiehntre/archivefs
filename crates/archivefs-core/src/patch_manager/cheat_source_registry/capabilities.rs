//! What a source can do, independent of how it is implemented.
//!
//! These flags describe a source at registration time, not at runtime. A
//! source that advertises `INSTALL` but whose underlying provider returns
//! errors is still an install-capable source; health is tracked separately.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CheatSourceCapabilities {
    pub browse: bool,
    pub search: bool,
    pub preview: bool,
    pub install: bool,
    pub download: bool,
    pub refresh: bool,
    pub health_check: bool,
    pub remote: bool,
    pub local: bool,
}

impl CheatSourceCapabilities {
    pub const fn none() -> Self {
        Self {
            browse: false,
            search: false,
            preview: false,
            install: false,
            download: false,
            refresh: false,
            health_check: false,
            remote: false,
            local: false,
        }
    }

    pub const fn read_only_browse() -> Self {
        Self {
            browse: true,
            search: true,
            ..Self::none()
        }
    }

    pub const fn remote_download_and_install() -> Self {
        Self {
            browse: true,
            search: true,
            preview: true,
            install: true,
            download: true,
            refresh: true,
            health_check: true,
            remote: true,
            local: false,
        }
    }

    pub const fn remote_download_read_only() -> Self {
        Self {
            browse: true,
            search: true,
            preview: true,
            download: true,
            refresh: true,
            health_check: true,
            remote: true,
            ..Self::none()
        }
    }

    pub const fn local_read_only() -> Self {
        Self {
            browse: true,
            search: true,
            preview: true,
            install: true,
            local: true,
            ..Self::none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_serialize_as_snake_case() {
        let caps = CheatSourceCapabilities::remote_download_and_install();
        let json = serde_json::to_value(caps).unwrap();
        assert_eq!(json["browse"], true);
        assert_eq!(json["remote"], true);
        assert_eq!(json["local"], false);
        assert_eq!(json["install"], true);
    }

    #[test]
    fn none_is_all_false() {
        let caps = CheatSourceCapabilities::none();
        let json = serde_json::to_value(caps).unwrap();
        for (_, v) in json.as_object().unwrap() {
            assert_eq!(v, false);
        }
    }

    #[test]
    fn read_only_browse_has_no_install_or_download() {
        let caps = CheatSourceCapabilities::read_only_browse();
        assert!(caps.browse);
        assert!(!caps.install);
        assert!(!caps.download);
        assert!(!caps.remote);
    }
}
