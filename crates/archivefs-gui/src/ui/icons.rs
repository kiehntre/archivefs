//! The friendly visual language: a small, consistent glyph icon set.
//!
//! These are plain Unicode glyphs rendered by egui's bundled emoji font - no
//! image assets are shipped. The same concept always uses the same glyph, so
//! a user learns one visual cue per idea. Icons are always drawn *alongside*
//! a text label and never replace it, so they stay a secondary cue and a
//! missing glyph on some system can never remove meaning.
//!
//! A larger hand-drawn illustration pass can follow after more beta feedback;
//! this establishes the visual language today without new artwork.

pub(crate) const HOME: &str = "🏠";

// Primary concepts (used on Home cards and the matching page headers).
pub(crate) const GAMES: &str = "🎮"; // My Games / Library
pub(crate) const ORGANISE: &str = "🗂️"; // Organise / Canonical Organisation
pub(crate) const CHECK: &str = "🩺"; // Check Library / Doctor
pub(crate) const CHEATS: &str = "❤️×99"; // Cheats & Mods (the cheat-game identity)
pub(crate) const VERIFY: &str = "🧾"; // Verify Games / DAT verification
pub(crate) const SETTINGS: &str = "⚙️";

// Secondary concepts.
pub(crate) const SOURCES: &str = "📂";
pub(crate) const MOUNT: &str = "💿";
pub(crate) const ARTWORK: &str = "🖼️";
pub(crate) const HISTORY: &str = "📜";
pub(crate) const RECENT: &str = "🕐";
pub(crate) const SELECTED: &str = "🎯";
pub(crate) const ABOUT: &str = "ℹ️";
pub(crate) const ROMM: &str = "🌐";
pub(crate) const CLEAN_UP: &str = "🧹";
pub(crate) const SEARCH: &str = "🔍";

/// The restrained retro cheat-code motif used once (Home or Cheats header) as
/// decoration only - never the primary label.
pub(crate) const CHEAT_CODE: &str = "↑ ↑ ↓ ↓ ← →";

/// `"{glyph} {label}"` - the standard way to put an icon next to a label.
#[must_use]
pub(crate) fn with_icon(glyph: &str, label: &str) -> String {
    format!("{glyph} {label}")
}
