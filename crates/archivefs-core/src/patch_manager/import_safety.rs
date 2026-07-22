//! Product-level trust, inspection, and consent models for future cheat and
//! mod imports.
//!
//! This module is deliberately not an import engine or malware scanner. The
//! trusted RetroArch source pipeline already performs its own bounded download,
//! archive, extraction, and catalogue validation. Local/community import
//! inspection is not implemented yet, so callers must represent it as
//! [`LocalSafetyScanningState::PlannedUnavailable`], never as a successful scan.

use serde::{Deserialize, Serialize};

/// The product rule applied to every future imported active-content adapter.
pub const UNKNOWN_CODE_POLICY: &str =
    "ArchiveFS may inspect imported content, but it never executes unknown code automatically.";

/// The three user-facing trust states. Missing provenance alone is not a
/// technical-danger finding and therefore does not turn `Unverified` into
/// `Blocked`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportTrustState {
    Trusted,
    Unverified,
    Blocked,
}

/// Source categories the workspace can represent without implying that every
/// category has an implemented import action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSourceKind {
    EmulatorManagedLibrary,
    ArchiveFsTrustedCatalogue,
    LocalUnverifiedSource,
    RemoteUnverifiedSource,
}

/// Kept separate from trust so an unverified item can pass structural checks
/// without being relabelled as reviewed or malware-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportInspectionState {
    NotAvailable,
    NotInspected,
    Passed,
    PassedWithWarnings,
    Blocked,
}

/// The truthful current setting state. `Enabled`/`Disabled` are reserved for a
/// future pipeline that can actually change inspection behavior; the current
/// GUI must use `PlannedUnavailable` and expose no toggle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalSafetyScanningState {
    PlannedUnavailable,
    Enabled,
    DisabledPendingConfirmation,
    Disabled,
}

impl LocalSafetyScanningState {
    pub const fn current() -> Self {
        Self::PlannedUnavailable
    }

    /// Disabling a future working scanner requires a separate confirmation
    /// state. The currently unavailable setting can never be "disabled" as if
    /// it had previously protected an import.
    pub const fn request_disable(self) -> Self {
        match self {
            Self::Enabled => Self::DisabledPendingConfirmation,
            other => other,
        }
    }

    pub const fn confirm_disable(self) -> Self {
        match self {
            Self::DisabledPendingConfirmation => Self::Disabled,
            other => other,
        }
    }

    pub const fn inspection_state(self) -> ImportInspectionState {
        match self {
            Self::PlannedUnavailable => ImportInspectionState::NotAvailable,
            Self::Enabled => ImportInspectionState::NotInspected,
            Self::DisabledPendingConfirmation | Self::Disabled => {
                ImportInspectionState::NotInspected
            }
        }
    }
}

/// Adapter policy for active content. Recognition never grants execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveContentPolicy {
    /// Active content is incompatible with the selected adapter.
    Incompatible,
    /// The adapter may inspect and expose active content as high risk, but may
    /// not execute it.
    InspectOnlyHighRisk,
    /// The format is passive adapter data (for example a table document), not
    /// an executable payload.
    PassiveData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveContentDisposition {
    BlockedIncompatible,
    InspectableHighRisk,
    CompatiblePassiveData,
}

pub const fn classify_active_content(policy: ActiveContentPolicy) -> ActiveContentDisposition {
    match policy {
        ActiveContentPolicy::Incompatible => ActiveContentDisposition::BlockedIncompatible,
        ActiveContentPolicy::InspectOnlyHighRisk => ActiveContentDisposition::InspectableHighRisk,
        ActiveContentPolicy::PassiveData => ActiveContentDisposition::CompatiblePassiveData,
    }
}

/// A concrete structural block overrides source trust. Every other inspection
/// outcome preserves the original `Trusted` or `Unverified` classification.
pub const fn trust_after_inspection(
    original: ImportTrustState,
    inspection: ImportInspectionState,
) -> ImportTrustState {
    if matches!(original, ImportTrustState::Blocked)
        || matches!(inspection, ImportInspectionState::Blocked)
    {
        ImportTrustState::Blocked
    } else {
        original
    }
}

/// Information a future unverified-source confirmation must be able to show.
/// It contains no execution callback or filesystem mutation authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportConsentSummary {
    pub trust_state: ImportTrustState,
    pub inspection_state: ImportInspectionState,
    pub detected_adapter: Option<String>,
    pub detected_formats: Vec<String>,
    pub expected_files: Vec<String>,
    pub documentation_files: Vec<String>,
    pub unexpected_files: Vec<String>,
    pub executables: Vec<String>,
    pub scripts: Vec<String>,
    pub blocked_items: Vec<String>,
    pub warnings: Vec<String>,
    pub size_and_extraction_limits: Vec<String>,
    pub archivefs_will_do: Vec<String>,
    pub archivefs_will_not_do: Vec<String>,
    pub remaining_risk_confirmation_required: bool,
}

impl ImportConsentSummary {
    pub fn requires_explicit_consent(&self) -> bool {
        self.trust_state == ImportTrustState::Unverified
            && self.remaining_risk_confirmation_required
    }

    /// No consent state can override a concrete technical block.
    pub fn can_proceed_after_consent(&self, consent_given: bool) -> bool {
        self.trust_state != ImportTrustState::Blocked
            && self.inspection_state != ImportInspectionState::Blocked
            && (!self.requires_explicit_consent() || consent_given)
    }
}

/// There is intentionally no adapter hook that can return `true` here.
pub const fn automatic_execution_allowed() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_unverified_and_blocked_are_distinct_stable_values() {
        assert_ne!(ImportTrustState::Trusted, ImportTrustState::Unverified);
        assert_ne!(ImportTrustState::Unverified, ImportTrustState::Blocked);
        assert_eq!(
            serde_json::to_string(&ImportTrustState::Unverified).unwrap(),
            "\"unverified\""
        );
        assert_eq!(
            serde_json::to_string(&ImportSourceKind::LocalUnverifiedSource).unwrap(),
            "\"local_unverified_source\""
        );
    }

    #[test]
    fn unverified_content_is_not_automatically_malicious_or_blocked() {
        for inspection in [
            ImportInspectionState::NotAvailable,
            ImportInspectionState::NotInspected,
            ImportInspectionState::Passed,
            ImportInspectionState::PassedWithWarnings,
        ] {
            assert_eq!(
                trust_after_inspection(ImportTrustState::Unverified, inspection),
                ImportTrustState::Unverified
            );
        }
        assert_eq!(
            trust_after_inspection(ImportTrustState::Unverified, ImportInspectionState::Blocked),
            ImportTrustState::Blocked
        );
    }

    #[test]
    fn active_content_is_adapter_specific_but_never_auto_executed() {
        assert_eq!(
            classify_active_content(ActiveContentPolicy::Incompatible),
            ActiveContentDisposition::BlockedIncompatible
        );
        assert_eq!(
            classify_active_content(ActiveContentPolicy::InspectOnlyHighRisk),
            ActiveContentDisposition::InspectableHighRisk
        );
        assert_eq!(
            classify_active_content(ActiveContentPolicy::PassiveData),
            ActiveContentDisposition::CompatiblePassiveData
        );
        assert!(!automatic_execution_allowed());
        assert!(UNKNOWN_CODE_POLICY.contains("never executes unknown code automatically"));
    }

    #[test]
    fn unavailable_or_disabled_scanning_never_marks_content_safe() {
        assert_eq!(
            LocalSafetyScanningState::current().inspection_state(),
            ImportInspectionState::NotAvailable
        );
        assert_eq!(
            LocalSafetyScanningState::Enabled
                .request_disable()
                .inspection_state(),
            ImportInspectionState::NotInspected
        );
        assert_eq!(
            LocalSafetyScanningState::Enabled
                .request_disable()
                .confirm_disable(),
            LocalSafetyScanningState::Disabled
        );
        assert_eq!(
            LocalSafetyScanningState::PlannedUnavailable.request_disable(),
            LocalSafetyScanningState::PlannedUnavailable
        );
    }

    #[test]
    fn consent_preserves_freedom_but_cannot_override_a_technical_block() {
        let mut summary = ImportConsentSummary {
            trust_state: ImportTrustState::Unverified,
            inspection_state: ImportInspectionState::PassedWithWarnings,
            detected_adapter: None,
            detected_formats: Vec::new(),
            expected_files: Vec::new(),
            documentation_files: Vec::new(),
            unexpected_files: Vec::new(),
            executables: Vec::new(),
            scripts: Vec::new(),
            blocked_items: Vec::new(),
            warnings: vec!["provenance is not reviewed".to_string()],
            size_and_extraction_limits: Vec::new(),
            archivefs_will_do: Vec::new(),
            archivefs_will_not_do: vec!["execute imported code".to_string()],
            remaining_risk_confirmation_required: true,
        };
        assert!(!summary.can_proceed_after_consent(false));
        assert!(summary.can_proceed_after_consent(true));

        summary.inspection_state = ImportInspectionState::Blocked;
        assert!(!summary.can_proceed_after_consent(true));

        summary.trust_state = ImportTrustState::Trusted;
        summary.inspection_state = ImportInspectionState::Passed;
        assert!(
            summary.can_proceed_after_consent(false),
            "informational responsibility notices must not block ordinary safe use"
        );
    }
}
