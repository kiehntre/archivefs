//! Configurable ceilings for DAT parsing.
//!
//! Every limit exists to bound resource consumption, not to restrict valid DAT
//! files. A person with a genuinely enormous DAT can raise these through the
//! builder; the defaults are set above any known real-world DAT file.

/// Maximum file size in bytes that will be processed.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 256 * 1024 * 1024;

/// Upper ceiling for file size, beyond which no override is accepted.
pub const ABSOLUTE_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// Maximum number of game entries.
pub const DEFAULT_MAX_ENTRIES: usize = 500_000;

/// Upper ceiling for game entries.
pub const ABSOLUTE_MAX_ENTRIES: usize = 2_000_000;

/// Maximum number of ROM entries per game.
pub const DEFAULT_MAX_ROMS_PER_ENTRY: usize = 256;

/// Upper ceiling for ROMs per game.
pub const ABSOLUTE_MAX_ROMS_PER_ENTRY: usize = 2_048;

/// Maximum length of a game name or ROM name, in bytes.
pub const DEFAULT_MAX_IDENTIFIER_LENGTH: usize = 8_192;

/// Upper ceiling for identifier length.
pub const ABSOLUTE_MAX_IDENTIFIER_LENGTH: usize = 65_536;

/// Maximum length of a description field, in bytes.
pub const DEFAULT_MAX_DESCRIPTION_LENGTH: usize = 65_536;

/// Upper ceiling for description length.
pub const ABSOLUTE_MAX_DESCRIPTION_LENGTH: usize = 1_048_576;

/// Maximum number of parser warnings retained.
pub const DEFAULT_MAX_WARNINGS: usize = 1_024;

/// Upper ceiling for retained warnings.
pub const ABSOLUTE_MAX_WARNINGS: usize = 10_000;

/// XML depth limit for streaming parser.
pub const DEFAULT_MAX_XML_DEPTH: usize = 32;

/// Absolute ceiling for XML depth.
pub const ABSOLUTE_MAX_XML_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatLimits {
    pub max_file_size: u64,
    pub max_entries: usize,
    pub max_roms_per_entry: usize,
    pub max_identifier_length: usize,
    pub max_description_length: usize,
    pub max_warnings: usize,
    pub max_xml_depth: usize,
}

impl Default for DatLimits {
    fn default() -> Self {
        Self {
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            max_entries: DEFAULT_MAX_ENTRIES,
            max_roms_per_entry: DEFAULT_MAX_ROMS_PER_ENTRY,
            max_identifier_length: DEFAULT_MAX_IDENTIFIER_LENGTH,
            max_description_length: DEFAULT_MAX_DESCRIPTION_LENGTH,
            max_warnings: DEFAULT_MAX_WARNINGS,
            max_xml_depth: DEFAULT_MAX_XML_DEPTH,
        }
    }
}

impl DatLimits {
    pub fn builder() -> DatLimitsBuilder {
        DatLimitsBuilder::default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct DatLimitsBuilder {
    limits: DatLimits,
}

impl DatLimitsBuilder {
    pub fn max_file_size(mut self, bytes: u64) -> Self {
        self.limits.max_file_size = bytes.min(ABSOLUTE_MAX_FILE_SIZE);
        self
    }

    pub fn max_entries(mut self, count: usize) -> Self {
        self.limits.max_entries = count.min(ABSOLUTE_MAX_ENTRIES);
        self
    }

    pub fn max_roms_per_entry(mut self, count: usize) -> Self {
        self.limits.max_roms_per_entry = count.min(ABSOLUTE_MAX_ROMS_PER_ENTRY);
        self
    }

    pub fn max_identifier_length(mut self, length: usize) -> Self {
        self.limits.max_identifier_length = length.min(ABSOLUTE_MAX_IDENTIFIER_LENGTH);
        self
    }

    pub fn max_description_length(mut self, length: usize) -> Self {
        self.limits.max_description_length = length.min(ABSOLUTE_MAX_DESCRIPTION_LENGTH);
        self
    }

    pub fn max_warnings(mut self, count: usize) -> Self {
        self.limits.max_warnings = count.min(ABSOLUTE_MAX_WARNINGS);
        self
    }

    pub fn max_xml_depth(mut self, depth: usize) -> Self {
        self.limits.max_xml_depth = depth.min(ABSOLUTE_MAX_XML_DEPTH);
        self
    }

    pub fn build(self) -> DatLimits {
        self.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let limits = DatLimits::default();
        assert!(limits.max_file_size > 0);
        assert!(limits.max_entries > 0);
        assert!(limits.max_roms_per_entry > 0);
        assert!(limits.max_identifier_length > 0);
        assert!(limits.max_description_length > 0);
        assert!(limits.max_warnings > 0);
        assert!(limits.max_xml_depth > 0);
    }

    #[test]
    fn builder_clamps_to_absolute() {
        let limits = DatLimits::builder()
            .max_file_size(u64::MAX)
            .max_entries(usize::MAX)
            .max_xml_depth(usize::MAX)
            .build();
        assert_eq!(limits.max_file_size, ABSOLUTE_MAX_FILE_SIZE);
        assert_eq!(limits.max_entries, ABSOLUTE_MAX_ENTRIES);
        assert_eq!(limits.max_xml_depth, ABSOLUTE_MAX_XML_DEPTH);
    }
}
