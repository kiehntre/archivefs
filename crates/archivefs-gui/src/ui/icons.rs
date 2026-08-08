//! A small, consistent glyph icon set for navigation and headers.
//!
//! These are plain Unicode glyphs rendered by egui's bundled emoji font - no
//! image assets are shipped. The same concept always uses the same glyph, so
//! a user learns one visual cue per idea. Icons are always drawn *alongside*
//! a text label and never replace it, so they stay a secondary cue.
//!
//! A larger hand-drawn icon/illustration pass can follow after more beta
//! feedback; this keeps the app recognisable today without new artwork.

pub(crate) const HOME: &str = "🏠";
pub(crate) const LIBRARY: &str = "🎮";
pub(crate) const SOURCES: &str = "📂";
pub(crate) const MOUNT: &str = "💿";
pub(crate) const CHEATS: &str = "🧩";
pub(crate) const DAT_CATALOGUES: &str = "📚";
pub(crate) const ORGANISE: &str = "🗂️";
pub(crate) const CLEAN_UP: &str = "🧹";
pub(crate) const DOCTOR: &str = "🩺";
pub(crate) const SETTINGS: &str = "⚙️";
pub(crate) const HISTORY: &str = "📜";
pub(crate) const RECENT: &str = "🕐";
pub(crate) const SELECTED: &str = "🎯";
pub(crate) const ABOUT: &str = "ℹ️";
pub(crate) const ROMM: &str = "🌐";
pub(crate) const SEARCH: &str = "🔍";
pub(crate) const EMPTY_BOX: &str = "📭";

/// `"{glyph} {label}"` - the standard way to put an icon next to a label.
#[must_use]
pub(crate) fn with_icon(glyph: &str, label: &str) -> String {
    format!("{glyph} {label}")
}
