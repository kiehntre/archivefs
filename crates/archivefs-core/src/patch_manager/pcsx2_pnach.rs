//! Lossless existing-file preservation plus strict ArchiveFS-managed PNACH blocks.

use std::collections::BTreeSet;

pub const MAX_MANAGED_PNACH_BYTES: usize = 512 * 1024;
pub const MAX_MANAGED_PNACH_BLOCKS: usize = 128;
const BLOCK_START: &str = "// ArchiveFS managed block: ";
const BLOCK_END: &str = "// End ArchiveFS managed block";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PnachDocumentErrorKind {
    TooLarge,
    InvalidUtf8,
    MalformedManagedBlock,
    DuplicateManagedBlock,
    InvalidManagedId,
    InvalidPatchLine,
    NoPatchLines,
    TooManyManagedBlocks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnachDocumentError {
    pub kind: PnachDocumentErrorKind,
    pub line: Option<usize>,
    pub detail: String,
}

impl std::fmt::Display for PnachDocumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for PnachDocumentError {}

fn error(
    kind: PnachDocumentErrorKind,
    line: Option<usize>,
    detail: impl Into<String>,
) -> PnachDocumentError {
    PnachDocumentError {
        kind,
        line,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnachPatchLine {
    rendered: String,
}

impl PnachPatchLine {
    pub fn parse(value: &str) -> Result<Self, PnachDocumentError> {
        let code = value
            .split_once("//")
            .map_or(value, |(code, _)| code)
            .trim();
        let Some(fields) = code.strip_prefix("patch=") else {
            return Err(error(
                PnachDocumentErrorKind::InvalidPatchLine,
                None,
                "managed PNACH line must start with patch=",
            ));
        };
        let fields = fields.split(',').map(str::trim).collect::<Vec<_>>();
        if fields.len() != 5
            || !matches!(fields[0], "0" | "1" | "2")
            || !matches!(fields[1], "EE" | "IOP")
            || !is_hex(fields[2], 8, 8)
            || !matches!(fields[3], "byte" | "short" | "word" | "double" | "extended")
            || !valid_patch_value(fields[3], fields[4])
        {
            return Err(error(
                PnachDocumentErrorKind::InvalidPatchLine,
                None,
                "managed PNACH patch has an unsupported or malformed field",
            ));
        }
        Ok(Self {
            rendered: format!(
                "patch={},{},{},{},{}",
                fields[0],
                fields[1],
                fields[2].to_ascii_uppercase(),
                fields[3],
                fields[4].to_ascii_uppercase()
            ),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.rendered
    }
}

fn valid_patch_value(kind: &str, value: &str) -> bool {
    let maximum = match kind {
        "byte" => 2,
        "short" => 4,
        "word" | "extended" => 8,
        "double" => 16,
        _ => return false,
    };
    is_hex(value, 1, maximum)
}

fn is_hex(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedPnachCheat {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub patch_lines: Vec<PnachPatchLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnachDocument {
    original: Vec<u8>,
    managed_block_ids: BTreeSet<String>,
}

impl PnachDocument {
    pub fn original_bytes(&self) -> &[u8] {
        &self.original
    }

    pub fn managed_block_ids(&self) -> &BTreeSet<String> {
        &self.managed_block_ids
    }
}

/// Parses only ArchiveFS block structure. All user comments, disabled lines,
/// unknown directives, line endings, and formatting remain opaque bytes and
/// are reproduced exactly during a merge.
pub fn parse_pnach_document(bytes: &[u8]) -> Result<PnachDocument, PnachDocumentError> {
    if bytes.len() > MAX_MANAGED_PNACH_BYTES {
        return Err(error(
            PnachDocumentErrorKind::TooLarge,
            None,
            "existing PNACH exceeds the managed-file byte limit",
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        error(
            PnachDocumentErrorKind::InvalidUtf8,
            None,
            "existing PNACH is not valid UTF-8 and will not be rewritten",
        )
    })?;
    let mut ids = BTreeSet::new();
    let mut open: Option<(String, usize)> = None;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim_end_matches('\r');
        if let Some(id) = trimmed.strip_prefix(BLOCK_START) {
            if open.is_some() || !valid_managed_id(id) {
                return Err(error(
                    PnachDocumentErrorKind::MalformedManagedBlock,
                    Some(line_number),
                    "managed block start is nested or has an invalid ID",
                ));
            }
            if !ids.insert(id.to_string()) {
                return Err(error(
                    PnachDocumentErrorKind::DuplicateManagedBlock,
                    Some(line_number),
                    "managed block ID appears more than once",
                ));
            }
            open = Some((id.to_string(), line_number));
        } else if trimmed == BLOCK_END && open.take().is_none() {
            return Err(error(
                PnachDocumentErrorKind::MalformedManagedBlock,
                Some(line_number),
                "managed block end has no matching start",
            ));
        }
    }
    if let Some((_, line)) = open {
        return Err(error(
            PnachDocumentErrorKind::MalformedManagedBlock,
            Some(line),
            "managed block is not terminated",
        ));
    }
    Ok(PnachDocument {
        original: bytes.to_vec(),
        managed_block_ids: ids,
    })
}

pub fn merge_managed_pnach_cheats(
    document: &PnachDocument,
    cheats: &[ManagedPnachCheat],
) -> Result<Vec<u8>, PnachDocumentError> {
    if cheats.is_empty() || cheats.iter().all(|cheat| cheat.patch_lines.is_empty()) {
        return Err(error(
            PnachDocumentErrorKind::NoPatchLines,
            None,
            "at least one selected cheat with patch lines is required",
        ));
    }
    if document
        .managed_block_ids
        .len()
        .saturating_add(cheats.len())
        > MAX_MANAGED_PNACH_BLOCKS
    {
        return Err(error(
            PnachDocumentErrorKind::TooManyManagedBlocks,
            None,
            "managed block limit reached",
        ));
    }
    let mut seen = document.managed_block_ids.clone();
    let mut appended = String::new();
    for cheat in cheats {
        if !valid_managed_id(&cheat.id) {
            return Err(error(
                PnachDocumentErrorKind::InvalidManagedId,
                None,
                "managed cheat ID contains unsupported characters",
            ));
        }
        if !seen.insert(cheat.id.clone()) {
            return Err(error(
                PnachDocumentErrorKind::DuplicateManagedBlock,
                None,
                format!("managed cheat {} is already installed", cheat.id),
            ));
        }
        if cheat.patch_lines.is_empty() {
            return Err(error(
                PnachDocumentErrorKind::NoPatchLines,
                None,
                format!("managed cheat {} has no patch lines", cheat.id),
            ));
        }
        appended.push_str(BLOCK_START);
        appended.push_str(&cheat.id);
        appended.push('\n');
        appended.push_str("// ");
        appended.push_str(&sanitize_comment(&cheat.name));
        appended.push('\n');
        if let Some(description) = &cheat.description {
            appended.push_str("// ");
            appended.push_str(&sanitize_comment(description));
            appended.push('\n');
        }
        for patch in &cheat.patch_lines {
            appended.push_str(patch.as_str());
            appended.push('\n');
        }
        appended.push_str(BLOCK_END);
        appended.push('\n');
    }
    let mut output = document.original.clone();
    if !output.is_empty() {
        if !output.ends_with(b"\n") {
            output.push(b'\n');
        }
        output.push(b'\n');
    }
    output.extend_from_slice(appended.as_bytes());
    if output.len() > MAX_MANAGED_PNACH_BYTES {
        return Err(error(
            PnachDocumentErrorKind::TooLarge,
            None,
            "merged PNACH exceeds the managed-file byte limit",
        ));
    }
    Ok(output)
}

fn valid_managed_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn sanitize_comment(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\r' | '\n' | '\0' => ' ',
            _ => character,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheat(id: &str) -> ManagedPnachCheat {
        ManagedPnachCheat {
            id: id.to_string(),
            name: "Infinite health".to_string(),
            description: Some("Test only".to_string()),
            patch_lines: vec![
                PnachPatchLine::parse("patch=1,EE,20123456,word,00000001 // source note").unwrap(),
            ],
        }
    }

    #[test]
    fn strict_patch_parser_normalizes_supported_lines() {
        let patch = PnachPatchLine::parse("patch=1,EE,20abcdef,extended,00aa00bb").unwrap();
        assert_eq!(patch.as_str(), "patch=1,EE,20ABCDEF,extended,00AA00BB");
        assert!(PnachPatchLine::parse("patch=1,EE,not-hex,word,1").is_err());
        assert!(PnachPatchLine::parse("patch=1,EE,20123456,byte,0001").is_err());
        assert!(PnachPatchLine::parse("encrypted=DEADBEEF").is_err());
    }

    #[test]
    fn merge_preserves_comments_unknown_lines_disabled_entries_and_formatting() {
        let existing = b"// user comment\r\nunknown = keep me\r\npatch=0,EE,00100000,word,00000000";
        let document = parse_pnach_document(existing).unwrap();
        let merged = merge_managed_pnach_cheats(&document, &[cheat("health")]).unwrap();
        assert!(merged.starts_with(existing));
        let merged = String::from_utf8(merged).unwrap();
        assert!(merged.contains("unknown = keep me\r\n"));
        assert!(merged.contains("patch=0,EE,00100000,word,00000000\n\n"));
        assert!(merged.contains("// ArchiveFS managed block: health"));
    }

    #[test]
    fn two_managed_blocks_are_deterministic_and_independent() {
        let document = parse_pnach_document(b"").unwrap();
        let merged = merge_managed_pnach_cheats(&document, &[cheat("one"), cheat("two")]).unwrap();
        let reparsed = parse_pnach_document(&merged).unwrap();
        assert_eq!(
            reparsed.managed_block_ids(),
            &BTreeSet::from(["one".to_string(), "two".to_string()])
        );
        assert_eq!(
            merge_managed_pnach_cheats(&reparsed, &[cheat("one")])
                .unwrap_err()
                .kind,
            PnachDocumentErrorKind::DuplicateManagedBlock
        );
    }

    #[test]
    fn malformed_managed_boundaries_block_rewrite() {
        let error =
            parse_pnach_document(b"// ArchiveFS managed block: one\npatch=1,EE,00100000,word,1\n")
                .unwrap_err();
        assert_eq!(error.kind, PnachDocumentErrorKind::MalformedManagedBlock);
    }

    #[test]
    fn invalid_utf8_is_never_lossily_rewritten() {
        assert_eq!(
            parse_pnach_document(&[0xff]).unwrap_err().kind,
            PnachDocumentErrorKind::InvalidUtf8
        );
    }
}
