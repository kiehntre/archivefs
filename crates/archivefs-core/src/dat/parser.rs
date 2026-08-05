//! Shared parser types for DAT format parsing.
//!
//! Every parser returns a `ParseResult`, which is either a complete `ParsedDat`
//! or a structured error. Warnings are accumulated alongside the result.

use std::path::PathBuf;

use serde::Serialize;

use super::model::ParsedDat;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParseWarning {
    pub byte_offset: Option<usize>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub context: String,
    pub message: String,
}

impl ParseWarning {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            byte_offset: None,
            line: None,
            column: None,
            context: String::new(),
            message: message.into(),
        }
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.byte_offset = Some(offset);
        self
    }

    pub fn with_line_column(mut self, line: usize, column: usize) -> Self {
        self.line = Some(line);
        self.column = Some(column);
        self
    }

    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = context.into();
        self
    }
}

impl std::fmt::Display for ParseWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(offset) = self.byte_offset {
            write!(f, " (byte {offset})")?;
        }
        if let (Some(line), Some(column)) = (self.line, self.column) {
            write!(f, " at line {line}:{column}")?;
        }
        if !self.context.is_empty() {
            write!(f, " near {})", self.context)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ParseError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    FileTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },
    XmlDepthExceeded {
        depth: usize,
        limit: usize,
    },
    EntryLimitExceeded {
        count: usize,
        limit: usize,
    },
    RomsPerEntryExceeded {
        game_name: String,
        count: usize,
        limit: usize,
    },
    IdentifierTooLong {
        field: String,
        length: usize,
        limit: usize,
        content_snippet: String,
    },
    DescriptionTooLong {
        length: usize,
        limit: usize,
    },
    WarningLimitExceeded {
        count: usize,
        limit: usize,
    },
    MalformedXml {
        detail: String,
        byte_offset: Option<usize>,
    },
    DoctypeRejected {
        root_name: String,
    },
    EntityDeclarationRejected {
        entity_name: String,
    },
    UnsupportedEntityReference {
        name: String,
        byte_offset: Option<usize>,
    },
    UnknownFormat,
    UnsupportedFormat {
        detail: String,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "cannot read {}: {error}", path.display()),
            Self::FileTooLarge { path, size, limit } => {
                write!(
                    f,
                    "{} is {size} bytes, above the {limit}-byte limit",
                    path.display()
                )
            }
            Self::XmlDepthExceeded { depth, limit } => {
                write!(f, "XML depth {depth} exceeds limit of {limit}")
            }
            Self::EntryLimitExceeded { count, limit } => {
                write!(f, "entry count {count} exceeds limit of {limit}")
            }
            Self::RomsPerEntryExceeded {
                game_name,
                count,
                limit,
            } => {
                write!(
                    f,
                    "game {game_name:?} has {count} ROMs, exceeding the {limit} limit"
                )
            }
            Self::IdentifierTooLong {
                field,
                length,
                limit,
                content_snippet,
            } => {
                write!(
                    f,
                    "{field} is {length} bytes (limit {limit}); starts with \"{content_snippet}\""
                )
            }
            Self::DescriptionTooLong { length, limit } => {
                write!(
                    f,
                    "description is {length} bytes, exceeding the {limit} limit"
                )
            }
            Self::WarningLimitExceeded { count, limit } => {
                write!(f, "warning count {count} exceeds limit of {limit}")
            }
            Self::MalformedXml {
                detail,
                byte_offset,
            } => {
                if let Some(offset) = byte_offset {
                    write!(f, "malformed XML at byte {offset}: {detail}")
                } else {
                    write!(f, "malformed XML: {detail}")
                }
            }
            Self::DoctypeRejected { root_name } => {
                write!(f, "DOCTYPE declaration rejected (root={root_name})")
            }
            Self::EntityDeclarationRejected { entity_name } => {
                write!(f, "entity declaration rejected ({entity_name})")
            }
            Self::UnsupportedEntityReference { name, byte_offset } => {
                if let Some(offset) = byte_offset {
                    write!(f, "unsupported entity reference &{name}; at byte {offset}")
                } else {
                    write!(f, "unsupported entity reference &{name};")
                }
            }
            Self::UnknownFormat => write!(f, "unknown DAT format"),
            Self::UnsupportedFormat { detail } => {
                write!(f, "unsupported DAT format: {detail}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

pub type ParseResult = Result<ParseOutcome, ParseError>;

#[derive(Debug)]
pub struct ParseOutcome {
    pub dat: ParsedDat,
    pub warnings: Vec<ParseWarning>,
}
