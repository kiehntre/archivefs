//! Shared description of an emulator installation and its resolved writable profile.
//!
//! Adapter discovery owns the emulator-specific probing, but install planning and
//! UI reporting consume this common vocabulary instead of treating a generated
//! artifact directory as an emulator destination.

use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulatorInstallationType {
    NativeSystem,
    AppImage,
    Flatpak,
    PortableCustom,
    RetroDeckManaged,
    RetroArchManaged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulatorProfileConfidence {
    Speculative,
    KnownPath,
    SelectedLaunch,
    RunningExplicit,
    UserConfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct EmulatorDestinationDirectories {
    pub cheats: Option<PathBuf>,
    pub patches: Option<PathBuf>,
    pub mods: Option<PathBuf>,
    pub game_settings: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedEmulatorProfile {
    pub emulator_executable: Option<PathBuf>,
    pub installation_type: EmulatorInstallationType,
    pub configuration_root: PathBuf,
    pub data_user_root: PathBuf,
    pub active_explicit_profile: Option<PathBuf>,
    pub destinations: EmulatorDestinationDirectories,
    pub discovery_evidence: Vec<String>,
    pub confidence: EmulatorProfileConfidence,
    pub priority: u16,
    pub writable: bool,
}
