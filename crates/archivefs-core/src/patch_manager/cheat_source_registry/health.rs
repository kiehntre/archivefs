//! Read-only health snapshot for a single cheat source.
//!
//! This is a lightweight status record. It does not perform any network or
//! filesystem access itself; the caller updates it after performing a
//! health-specific operation (e.g. fetch, validation).

use serde::{Deserialize, Serialize};

use super::super::CheatProviderSourceState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CheatSourceHealth {
    pub state: CheatProviderSourceState,
    pub last_checked_unix_seconds: Option<u64>,
    pub last_error: Option<String>,
    pub entry_count: Option<u64>,
    pub freshness_seconds: Option<u64>,
}

impl CheatSourceHealth {
    pub const fn unknown() -> Self {
        Self {
            state: CheatProviderSourceState::NotInstalled,
            last_checked_unix_seconds: None,
            last_error: None,
            entry_count: None,
            freshness_seconds: None,
        }
    }

    pub fn ready(entry_count: u64) -> Self {
        Self {
            state: CheatProviderSourceState::Ready,
            last_checked_unix_seconds: Some(now_unix()),
            last_error: None,
            entry_count: Some(entry_count),
            freshness_seconds: None,
        }
    }

    pub fn error(state: CheatProviderSourceState, message: String) -> Self {
        Self {
            state,
            last_checked_unix_seconds: Some(now_unix()),
            last_error: Some(message),
            ..Self::unknown()
        }
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_not_installed() {
        let h = CheatSourceHealth::unknown();
        assert_eq!(h.state, CheatProviderSourceState::NotInstalled);
        assert!(h.last_checked_unix_seconds.is_none());
        assert!(h.last_error.is_none());
    }

    #[test]
    fn ready_has_timestamp() {
        let h = CheatSourceHealth::ready(42);
        assert_eq!(h.state, CheatProviderSourceState::Ready);
        assert_eq!(h.entry_count, Some(42));
        assert!(h.last_checked_unix_seconds.is_some());
        assert!(h.last_error.is_none());
    }

    #[test]
    fn error_carries_message() {
        let h = CheatSourceHealth::error(
            CheatProviderSourceState::DownloadFailed,
            "timeout".to_string(),
        );
        assert_eq!(h.state, CheatProviderSourceState::DownloadFailed);
        assert_eq!(h.last_error.as_deref(), Some("timeout"));
    }
}
