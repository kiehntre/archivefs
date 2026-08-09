//! The friendly visual language: a small, consistent text icon set.
//!
//! Sunshine/XFCE testing showed that several emoji which existed in source
//! still rendered as missing-glyph squares in the actual desktop font stack.
//! Primary navigation therefore uses printable ASCII compositions only. They
//! need no emoji fallback font, are stable in egui's proportional font, and
//! always sit beside a text label.
//!
//! A larger hand-drawn illustration pass can follow after more beta feedback;
//! this establishes the visual language today without new artwork.

pub(crate) const HOME: &str = "[H]";

// Primary concepts (used on Home cards and the matching page headers).
pub(crate) const GAMES: &str = ">"; // My Games / Library
pub(crate) const ORGANISE: &str = "A-Z"; // Organise / Canonical Organisation
pub(crate) const CHECK: &str = "[OK]"; // Check Library / Doctor
pub(crate) const CHEATS: &str = "<3 x99"; // Cheats & Mods (the cheat-game identity)
pub(crate) const VERIFY: &str = "[V]"; // Verify Games / DAT verification
pub(crate) const SETTINGS: &str = "[*]";

// Secondary concepts.
pub(crate) const SOURCES: &str = "[+]";
pub(crate) const MOUNT: &str = "[D]";
pub(crate) const ARTWORK: &str = "[IMG]";
pub(crate) const HISTORY: &str = "[LOG]";
pub(crate) const RECENT: &str = "[T]";
pub(crate) const SELECTED: &str = "[>]";
pub(crate) const ABOUT: &str = "[i]";
pub(crate) const ROMM: &str = "[R]";
pub(crate) const CLEAN_UP: &str = "[C]";
pub(crate) const SEARCH: &str = "[?]";

/// The restrained retro cheat-code motif used once (Home or Cheats header) as
/// decoration only - never the primary label.
pub(crate) const CHEAT_CODE: &str = "UP UP DOWN DOWN LEFT RIGHT";

/// `"{glyph} {label}"` - the standard way to put an icon next to a label.
#[must_use]
pub(crate) fn with_icon(glyph: &str, label: &str) -> String {
    format!("{glyph} {label}")
}

/// True when a primary icon is guaranteed to stay within the basic printable
/// ASCII repertoire. This is a renderability contract, not merely a string
/// presence check.
#[cfg(test)]
pub(crate) fn is_font_stack_safe(glyph: &str) -> bool {
    !glyph.is_empty() && glyph.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_icons_need_no_emoji_fallback_font() {
        for icon in [GAMES, ORGANISE, CHECK, CHEATS, VERIFY, SETTINGS] {
            assert!(is_font_stack_safe(icon), "unsafe primary icon {icon:?}");
            assert!(!icon.contains('\u{fffd}'));
        }
    }
}
