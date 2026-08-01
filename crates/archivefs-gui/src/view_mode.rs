use std::{fmt, str::FromStr};

/// The two supported GUI experiences. Rendering and navigation deliberately
/// live elsewhere; this type is only the stable mode identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ViewMode {
    #[default]
    Gamer,
    Advanced,
}

impl ViewMode {
    pub const ALL: [Self; 2] = [Self::Gamer, Self::Advanced];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Gamer => "Gamer View",
            Self::Advanced => "Advanced View",
        }
    }

    /// Stable lower-case value suitable for eframe or config persistence.
    pub const fn persisted(self) -> &'static str {
        match self {
            Self::Gamer => "gamer",
            Self::Advanced => "advanced",
        }
    }

    pub fn from_persisted(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl fmt::Display for ViewMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseViewModeError;

impl fmt::Display for ParseViewModeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected 'gamer' or 'advanced'")
    }
}

impl std::error::Error for ParseViewModeError {}

impl FromStr for ViewMode {
    type Err = ParseViewModeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "gamer" => Ok(Self::Gamer),
            "advanced" => Ok(Self::Advanced),
            _ => Err(ParseViewModeError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_gamer_view() {
        assert_eq!(ViewMode::default(), ViewMode::Gamer);
    }

    #[test]
    fn labels_are_plain_and_distinct() {
        assert_eq!(ViewMode::Gamer.label(), "Gamer View");
        assert_eq!(ViewMode::Advanced.label(), "Advanced View");
    }

    #[test]
    fn persisted_values_round_trip() {
        for mode in ViewMode::ALL {
            assert_eq!(ViewMode::from_persisted(mode.persisted()), Some(mode));
        }
        assert_eq!(ViewMode::from_persisted("Gamer View"), None);
        assert_eq!(ViewMode::from_persisted("unknown"), None);
    }
}
