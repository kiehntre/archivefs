use archivefs_core::{MountState, game_identity::IdentityStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusContext {
    pub path_available: bool,
    pub mounting_applies: bool,
    pub mount_state: Option<MountState>,
    pub identity_status: Option<IdentityStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlainStatus {
    pub headline: &'static str,
    pub detail: Option<&'static str>,
}

/// Maps existing backend state to beginner wording without changing or hiding
/// the technical values retained by `GameTechnicalStatus`.
pub fn plain_status(context: StatusContext) -> PlainStatus {
    if !context.path_available {
        return PlainStatus {
            headline: "Game file is unavailable",
            detail: Some("Reconnect its source or scan the library again"),
        };
    }

    match context.identity_status {
        Some(IdentityStatus::Deferred) => {
            return PlainStatus {
                headline: "Identification is not available for this format yet",
                detail: None,
            };
        }
        Some(
            IdentityStatus::Unsupported
            | IdentityStatus::Invalid
            | IdentityStatus::Ambiguous
            | IdentityStatus::ResourceLimitReached,
        ) => {
            return PlainStatus {
                headline: "Could not identify this game",
                detail: None,
            };
        }
        Some(IdentityStatus::Verified | IdentityStatus::Candidate | IdentityStatus::Missing)
        | None => {}
    }

    if !context.mounting_applies {
        return PlainStatus {
            headline: "Ready to use directly",
            detail: Some("No mounting needed"),
        };
    }

    match context.mount_state {
        Some(MountState::Mounted) => PlainStatus {
            headline: "Mounted",
            detail: None,
        },
        Some(MountState::MountPathExists) => PlainStatus {
            headline: "Mount folder needs attention",
            detail: Some("The mount location already exists"),
        },
        Some(MountState::NotMountable) => PlainStatus {
            headline: "This game cannot be mounted",
            detail: None,
        },
        Some(MountState::Pending) | None => PlainStatus {
            headline: "Ready to mount",
            detail: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> StatusContext {
        StatusContext {
            path_available: true,
            mounting_applies: true,
            mount_state: Some(MountState::Pending),
            identity_status: None,
        }
    }

    #[test]
    fn direct_images_need_no_mount() {
        assert_eq!(
            plain_status(StatusContext {
                mounting_applies: false,
                ..context()
            }),
            PlainStatus {
                headline: "Ready to use directly",
                detail: Some("No mounting needed")
            }
        );
    }

    #[test]
    fn mount_states_have_beginner_wording() {
        assert_eq!(plain_status(context()).headline, "Ready to mount");
        assert_eq!(
            plain_status(StatusContext {
                mount_state: Some(MountState::Mounted),
                ..context()
            })
            .headline,
            "Mounted"
        );
    }

    #[test]
    fn unsupported_and_deferred_identity_are_distinct() {
        assert_eq!(
            plain_status(StatusContext {
                identity_status: Some(IdentityStatus::Unsupported),
                ..context()
            })
            .headline,
            "Could not identify this game"
        );
        assert_eq!(
            plain_status(StatusContext {
                identity_status: Some(IdentityStatus::Deferred),
                ..context()
            })
            .headline,
            "Identification is not available for this format yet"
        );
    }

    #[test]
    fn missing_path_takes_precedence() {
        assert_eq!(
            plain_status(StatusContext {
                path_available: false,
                mounting_applies: false,
                identity_status: Some(IdentityStatus::Deferred),
                ..context()
            })
            .headline,
            "Game file is unavailable"
        );
    }
}
