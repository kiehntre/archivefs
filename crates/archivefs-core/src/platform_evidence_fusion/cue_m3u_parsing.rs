//! Batch 11: bounded, read-only `.cue`/`.m3u` reference parsing - milestone
//! sections 9-10.
//!
//! No parser for either format existed anywhere in this crate before this
//! module (confirmed by a repo-wide search - `inspector.rs` only names a
//! cue sheet as a companion-file *concept* in a test, and never reads one).
//! Both parsers here are deliberately small and bounded rather than general
//! cue/m3u implementations: they extract exactly the referenced-file
//! strings a grouping decision needs, reject anything that looks like a
//! path-traversal or an absolute reference, and read nothing beyond a
//! fixed byte cap.
//!
//! Both parsers are read-only: they never open, hash, or otherwise touch
//! the referenced files, only the text of the `.cue`/`.m3u` itself
//! (already read by the caller - this module takes `&str`, never a path it
//! would read on its own, so the caller stays in full control of what gets
//! read and how much).

use std::path::{Component, Path, PathBuf};

/// The largest cue/m3u file this parser will accept - both formats are
/// small plain-text listings; anything larger is refused rather than
/// parsed, never silently truncated.
pub const MAX_PARSE_BYTES: usize = 64 * 1024;

/// Why one referenced line was rejected rather than resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceRejection {
    /// The reference is an absolute path - never trusted; a cue/m3u only
    /// ever names files alongside itself.
    AbsolutePath,
    /// The reference contains a `..` component.
    ParentTraversal,
    /// The reference is empty after trimming.
    Empty,
}

/// One parsed reference line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedReference {
    /// The raw referenced string, exactly as it appeared (quotes stripped).
    pub raw: String,
    /// `raw` resolved to a path alongside the cue/m3u's own directory - only
    /// `Some` when `rejection` is `None`.
    pub resolved: Option<PathBuf>,
    pub rejection: Option<ReferenceRejection>,
}

impl ParsedReference {
    pub fn is_safe(&self) -> bool {
        self.rejection.is_none()
    }
}

/// Resolves one raw referenced string against `base_dir` (the cue/m3u's own
/// parent directory), rejecting anything unsafe rather than joining it
/// blindly - milestone section 10's explicit reject list.
fn resolve_reference(raw: &str, base_dir: &Path) -> ParsedReference {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ParsedReference {
            raw: trimmed.to_string(),
            resolved: None,
            rejection: Some(ReferenceRejection::Empty),
        };
    }
    let as_path = Path::new(trimmed);
    if as_path.is_absolute() {
        return ParsedReference {
            raw: trimmed.to_string(),
            resolved: None,
            rejection: Some(ReferenceRejection::AbsolutePath),
        };
    }
    if as_path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return ParsedReference {
            raw: trimmed.to_string(),
            resolved: None,
            rejection: Some(ReferenceRejection::ParentTraversal),
        };
    }
    ParsedReference {
        raw: trimmed.to_string(),
        resolved: Some(base_dir.join(as_path)),
        rejection: None,
    }
}

/// Extracts every `FILE "..." <type>` reference from a `.cue` sheet's
/// already-read text, bounded to [`MAX_PARSE_BYTES`]. `cue_path` supplies
/// the base directory the (safe) references are resolved against; the cue
/// file itself is never re-read here.
///
/// Only the `FILE "name" TYPE` line shape is recognised (the one real shape
/// every cue sheet in practice uses to reference a track file) - anything
/// else in the sheet (`TRACK`, `INDEX`, comments) is ignored, not an error.
pub fn parse_cue_file_references(cue_path: &Path, contents: &str) -> Vec<ParsedReference> {
    if contents.len() > MAX_PARSE_BYTES {
        return Vec::new();
    }
    let base_dir = cue_path.parent().unwrap_or_else(|| Path::new(""));
    contents
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let rest = trimmed
                .strip_prefix("FILE")
                .or_else(|| trimmed.strip_prefix("file"))?;
            let rest = rest.trim_start();
            let quoted = rest.strip_prefix('"')?;
            let end = quoted.find('"')?;
            Some(resolve_reference(&quoted[..end], base_dir))
        })
        .collect()
}

/// Extracts every non-comment, non-blank line from an `.m3u`/`.m3u8`
/// playlist's already-read text, bounded to [`MAX_PARSE_BYTES`] - each
/// surviving line is one referenced disc image. `#EXTM3U`/`#EXTINF` and any
/// other `#`-prefixed line are metadata, never a reference.
pub fn parse_m3u_references(m3u_path: &Path, contents: &str) -> Vec<ParsedReference> {
    if contents.len() > MAX_PARSE_BYTES {
        return Vec::new();
    }
    let base_dir = m3u_path.parent().unwrap_or_else(|| Path::new(""));
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| resolve_reference(line, base_dir))
        .collect()
}

#[cfg(test)]
mod tests;
