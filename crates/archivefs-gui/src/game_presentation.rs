use std::path::PathBuf;

use archivefs_core::{
    ArchiveKind, ArchiveRecord, MountState, PersistedArchive, game_identity::IdentityStatus,
};

use crate::status_wording::{PlainStatus, StatusContext, plain_status};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathAvailability {
    Available(PathBuf),
    Missing(PathBuf),
    Unavailable,
}

impl PathAvailability {
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Available(path) | Self::Missing(path) => Some(path),
            Self::Unavailable => None,
        }
    }
}

/// Raw values intended for Advanced View diagnostics. Beginner wording is
/// stored separately in `SelectedGamePresentation::status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameTechnicalStatus {
    pub archive_kind: String,
    pub mount_state: Option<MountState>,
    pub identity_status: Option<IdentityStatus>,
    pub health: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedGamePresentation {
    pub title: String,
    pub platform: String,
    pub format: String,
    pub path: PathAvailability,
    pub mounting_applies: bool,
    pub direct_use_applies: bool,
    pub cheats_mods_available: bool,
    pub undo_available: bool,
    pub status: PlainStatus,
    pub technical_status: GameTechnicalStatus,
}

impl SelectedGamePresentation {
    /// Builds presentation data from the live `ArchiveRecord` already chosen
    /// by the GUI. Capability booleans are accepted from their existing
    /// backend gates so this layer never reimplements provider or history
    /// policy.
    pub fn from_live(
        record: &ArchiveRecord,
        identity_status: Option<IdentityStatus>,
        cheats_mods_available: bool,
        undo_available: bool,
    ) -> Self {
        let kind = record.mount_plan.archive.kind;
        let mounting_applies = kind.is_mount_input();
        let path = PathAvailability::Available(record.mount_plan.archive.path.clone());
        let status = plain_status(StatusContext {
            path_available: true,
            mounting_applies,
            mount_state: Some(record.mount_state),
            identity_status,
        });
        Self {
            title: record
                .metadata
                .title
                .clone()
                .unwrap_or_else(|| record.identity.display_name.clone()),
            platform: record
                .metadata
                .platform
                .clone()
                .or_else(|| record.identity.platform.clone())
                .unwrap_or_else(|| "Unknown platform".to_string()),
            format: archive_kind_label(kind).to_string(),
            path,
            mounting_applies,
            direct_use_applies: !mounting_applies,
            cheats_mods_available,
            undo_available,
            status,
            technical_status: GameTechnicalStatus {
                archive_kind: format!("{kind:?}"),
                mount_state: Some(record.mount_state),
                identity_status,
                health: Some(record.health.to_string()),
            },
        }
    }

    /// Cache-only counterpart for a selected catalogue row. It deliberately
    /// exposes missing/unavailable paths and never claims a mount state that
    /// the live snapshot did not provide.
    pub fn from_cached(
        archive: &PersistedArchive,
        identity_status: Option<IdentityStatus>,
        cheats_mods_available: bool,
        undo_available: bool,
    ) -> Self {
        let missing = archive.last_verified_missing_at.is_some();
        let path = if missing {
            PathAvailability::Missing(archive.absolute_path.clone())
        } else {
            PathAvailability::Unavailable
        };
        let mounting_applies = persisted_kind_mounts(&archive.archive_kind);
        let status = plain_status(StatusContext {
            path_available: false,
            mounting_applies,
            mount_state: None,
            identity_status,
        });
        Self {
            title: archive.display_name.clone(),
            platform: archive
                .platform
                .clone()
                .unwrap_or_else(|| "Unknown platform".to_string()),
            format: persisted_kind_label(&archive.archive_kind).to_string(),
            path,
            mounting_applies,
            direct_use_applies: !mounting_applies,
            cheats_mods_available,
            undo_available,
            status,
            technical_status: GameTechnicalStatus {
                archive_kind: archive.archive_kind.clone(),
                mount_state: None,
                identity_status,
                health: Some(archive.last_known_health.clone()),
            },
        }
    }
}

pub const fn archive_kind_label(kind: ArchiveKind) -> &'static str {
    match kind {
        ArchiveKind::Zip => "ZIP archive",
        ArchiveKind::SevenZip => "7z archive",
        ArchiveKind::Rar => "RAR archive",
        ArchiveKind::MegaDriveRom => "Mega Drive ROM",
        ArchiveKind::DirectGameImage => "Game image",
    }
}

fn persisted_kind_mounts(kind: &str) -> bool {
    !matches!(kind, "megadrive_rom" | "direct_game_image")
}

fn persisted_kind_label(kind: &str) -> &str {
    match kind {
        "zip" => "ZIP archive",
        "sevenzip" => "7z archive",
        "rar" => "RAR archive",
        "megadrive_rom" => "Mega Drive ROM",
        "direct_game_image" => "Game image",
        _ => kind,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use archivefs_core::{
        Archive, ArchiveHealth, ArchiveMetadata, ArchiveRecord, MountPlan, MountState,
    };

    use super::*;

    fn live_record(path: &str, mount_state: MountState) -> ArchiveRecord {
        let archive = Archive::from_path(Path::new(path)).expect("supported fixture extension");
        let plan = MountPlan::new(archive, PathBuf::from("/mnt/game"));
        let metadata = ArchiveMetadata {
            title: Some("Friendly Title".to_string()),
            platform: Some("GameCube".to_string()),
            region: None,
            languages: None,
            version: None,
            disc: None,
            publisher: None,
            developer: None,
            release_year: None,
            genre: None,
            notes: None,
            source: None,
        };
        ArchiveRecord::new(plan, mount_state, metadata, ArchiveHealth::Pending)
    }

    fn cached_archive(missing: bool) -> PersistedArchive {
        PersistedArchive {
            id: 1,
            source_folder_id: 2,
            relative_path: PathBuf::from("Game.rvz"),
            absolute_path: PathBuf::from("/games/Game.rvz"),
            archive_kind: "direct_game_image".to_string(),
            display_name: "Game".to_string(),
            normalized_name: "game".to_string(),
            size_bytes: Some(10),
            modified_time_unix_seconds: Some(20),
            platform: Some("Wii".to_string()),
            platform_source: Some("header-identity".to_string()),
            last_known_health: "present".to_string(),
            last_seen_at: "now".to_string(),
            last_verified_missing_at: missing.then(|| "later".to_string()),
        }
    }

    #[test]
    fn live_mountable_game_uses_existing_record_fields() {
        let model = SelectedGamePresentation::from_live(
            &live_record("/games/Game.zip", MountState::Pending),
            Some(IdentityStatus::Verified),
            true,
            false,
        );
        assert_eq!(model.title, "Friendly Title");
        assert_eq!(model.platform, "GameCube");
        assert_eq!(model.format, "ZIP archive");
        assert!(model.mounting_applies);
        assert!(!model.direct_use_applies);
        assert!(model.cheats_mods_available);
        assert_eq!(model.status.headline, "Ready to mount");
        assert_eq!(
            model.technical_status.mount_state,
            Some(MountState::Pending)
        );
    }

    #[test]
    fn direct_image_is_ready_without_mounting() {
        let model = SelectedGamePresentation::from_live(
            &live_record("/games/Game.rvz", MountState::NotMountable),
            Some(IdentityStatus::Verified),
            false,
            true,
        );
        assert!(!model.mounting_applies);
        assert!(model.direct_use_applies);
        assert!(model.undo_available);
        assert_eq!(model.status.headline, "Ready to use directly");
        assert_eq!(model.status.detail, Some("No mounting needed"));
    }

    #[test]
    fn cached_missing_game_never_claims_live_availability() {
        let model =
            SelectedGamePresentation::from_cached(&cached_archive(true), None, false, false);
        assert!(matches!(model.path, PathAvailability::Missing(_)));
        assert_eq!(model.status.headline, "Game file is unavailable");
        assert_eq!(model.technical_status.mount_state, None);
    }
}
