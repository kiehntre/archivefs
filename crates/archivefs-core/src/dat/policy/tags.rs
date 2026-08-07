//! Deterministic extraction of region, language and revision markers from
//! catalogue entry names.
//!
//! Preservation catalogues encode a dump's region, language and revision in
//! the entry name, in a de-facto convention: `Sonic the Hedgehog (Europe)
//! (En,Fr,De) (Rev 1)`. The three extractors here read exactly that. They are
//! pure functions of a name, so two runs over the same catalogue produce the
//! same answer and the ranking that consumes them is deterministic by
//! construction.
//!
//! # Token precedence
//!
//! A parenthesised group can hold several comma-separated tokens, and a token
//! like `fr` is both the French language code and a region abbreviation. The
//! convention in real names is that *two-letter codes are languages* and
//! *full words are regions* (`(En,Fr,De)` vs `(France)`), so that is the rule
//! here: a token is a language when it matches a language code, otherwise a
//! region when it matches a region word. A token that matches neither (for
//! example `Demo`, `Beta`, `Rev`) contributes nothing.
//!
//! This is a documented heuristic, not a parse of an authoritative grammar.
//! Catalogues are free-form; the extractors are best-effort and must never
//! panic on any input.

use super::model::{LanguageId, RegionId};

/// Region words that name one of the four canonical regions, mapped to it.
///
/// Anything region-shaped but not in this short list maps to
/// [`RegionId::Other`] via [`region_from_word`] - see [`region_words`].
const CANONICAL_REGION_WORDS: &[(&str, RegionId)] = &[
    ("world", RegionId::World),
    ("usa", RegionId::Usa),
    ("us", RegionId::Usa),
    ("japan", RegionId::Japan),
    ("jp", RegionId::Japan),
    ("europe", RegionId::Europe),
    ("eu", RegionId::Europe),
];

/// Words that look like a region to a catalogue name but are not one of the
/// four canonical regions. They all resolve to [`RegionId::Other`].
const OTHER_REGION_WORDS: &[&str] = &[
    "asia",
    "australia",
    "aus",
    "brazil",
    "br",
    "canada",
    "china",
    "cn",
    "korea",
    "kr",
    "russia",
    "ru",
    "germany",
    "ger",
    "france",
    "fra",
    "spain",
    "esp",
    "italy",
    "ita",
    "netherlands",
    "holland",
    "sweden",
    "swe",
    "finland",
    "fin",
    "norway",
    "nor",
    "denmark",
    "den",
    "poland",
    "pol",
    "uk",
    "united-kingdom",
    "england",
    "mexico",
    "argentina",
    "india",
    "hong-kong",
    "taiwan",
];

/// Maps one normalised name token to a region, or `None` when the token is
/// not region-shaped.
fn region_from_word(word: &str) -> Option<RegionId> {
    if let Some((_, region)) = CANONICAL_REGION_WORDS.iter().find(|(candidate, _)| *candidate == word)
    {
        return Some(*region);
    }
    if OTHER_REGION_WORDS.contains(&word) {
        return Some(RegionId::Other);
    }
    None
}

/// The region identifiers named by `name`'s `(…)` tags, in tag order,
/// deduplicated.
///
/// A name like `Game (USA, Europe)` yields `[Usa, Europe]`. A name with no
/// region tag yields an empty list - which is meaningfully different from
/// "region Other": an unknown tag says "we recognised a region but not a
/// canonical one", while no tag says "nothing in this name is a region".
pub fn regions_of_name(name: &str) -> Vec<RegionId> {
    let mut found: Vec<RegionId> = Vec::new();
    for token in parenthesised_tokens(name) {
        let Some(region) = region_from_word(&token) else {
            continue;
        };
        if !found.contains(&region) {
            found.push(region);
        }
    }
    found
}

/// The language identifiers named by `name`'s `(…)` tags, in tag order,
/// deduplicated.
///
/// `Game (Europe) (En,Fr,De)` yields `[En, Fr, De]`.
pub fn languages_of_name(name: &str) -> Vec<LanguageId> {
    let mut found: Vec<LanguageId> = Vec::new();
    for token in parenthesised_tokens(name) {
        let Some(language) = LanguageId::parse(&token) else {
            continue;
        };
        if !found.contains(&language) {
            found.push(language);
        }
    }
    found
}

/// The revision named by `name`'s `(Rev …)` marker, as an integer, and
/// whether a marker was present at all.
///
/// - `(Rev 1)` / `(Rev 2)` → the number;
/// - `(Rev A)` / `(Rev B)` → 1 / 2 (letters count A = 1 … Z = 26);
/// - anything else, or nothing → `(0, false)`.
///
/// A name may carry more than one marker; the first well-formed one wins.
pub fn revision_of_name(name: &str) -> (u32, bool) {
    for token in parenthesised_tokens(name) {
        if let Some(rest) = token.strip_prefix("rev") {
            let rest = rest.trim();
            if let Ok(number) = rest.parse::<u32>() {
                return (number, true);
            }
            if let Some(letter) = rest.chars().next()
                && letter.is_ascii_alphabetic()
                && rest.chars().count() == 1
            {
                let value = letter.to_ascii_uppercase() as u32 - 'A' as u32 + 1;
                return (value, true);
            }
        }
    }
    (0, false)
}

/// Every parenthesised token in `name`, split on commas, trimmed, normalised,
/// in document order.
///
/// Normalisation is ASCII-alphanumerics-only lowercased, the same folding the
/// platform registry uses, so `(En,Fr)` and `(En, Fr)` and `(EN-FR)` all
/// yield the same tokens.
fn parenthesised_tokens(name: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for group in parenthesised_groups(name) {
        for raw in group.split(',') {
            let token = normalize_token(raw);
            if !token.is_empty() {
                tokens.push(token);
            }
        }
    }
    tokens
}

/// Every `( … )` group in `name`, with the parentheses stripped.
fn parenthesised_groups(name: &str) -> Vec<&str> {
    let mut groups = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in name.char_indices() {
        match ch {
            '(' => {
                if depth == 0 {
                    start = index + 1;
                }
                depth += 1;
            }
            ')' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    groups.push(&name[start..index]);
                }
            }
            _ => {}
        }
    }
    groups
}

/// ASCII alphanumerics only, lowercased, then trimmed of punctuation runs -
/// the same folding as the platform registry's `normalize_alias`, kept local
/// so this module stays self-contained.
fn normalize_token(token: &str) -> String {
    token
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_tags_map_to_canonical_ids() {
        assert_eq!(regions_of_name("Sonic (Europe)"), vec![RegionId::Europe]);
        assert_eq!(regions_of_name("Sonic (USA)"), vec![RegionId::Usa]);
        assert_eq!(regions_of_name("Sonic (Japan)"), vec![RegionId::Japan]);
        assert_eq!(regions_of_name("Sonic (World)"), vec![RegionId::World]);
    }

    #[test]
    fn multi_region_tags_preserve_order_and_deduplicate() {
        assert_eq!(
            regions_of_name("Game (USA, Europe)"),
            vec![RegionId::Usa, RegionId::Europe]
        );
        assert_eq!(
            regions_of_name("Game (Europe, USA, Europe)"),
            vec![RegionId::Europe, RegionId::Usa]
        );
    }

    #[test]
    fn unknown_regions_map_to_other() {
        assert_eq!(regions_of_name("Game (Brazil)"), vec![RegionId::Other]);
        assert_eq!(regions_of_name("Game (Germany)"), vec![RegionId::Other]);
    }

    #[test]
    fn no_region_tag_is_an_empty_list_not_other() {
        assert_eq!(regions_of_name("Game"), Vec::<RegionId>::new());
        assert_eq!(regions_of_name("Game (Demo)"), Vec::<RegionId>::new());
        assert_eq!(regions_of_name("Game (En)"), Vec::<RegionId>::new());
    }

    #[test]
    fn language_tags_are_two_letter_codes() {
        assert_eq!(languages_of_name("Game (En)"), vec![LanguageId::En]);
        assert_eq!(
            languages_of_name("Game (Europe) (En,Fr,De)"),
            vec![LanguageId::En, LanguageId::Fr, LanguageId::De]
        );
        assert_eq!(languages_of_name("Game (Japan) (Ja)"), vec![LanguageId::Ja]);
    }

    #[test]
    fn language_tags_deduplicate() {
        assert_eq!(
            languages_of_name("Game (En,En)"),
            vec![LanguageId::En]
        );
    }

    #[test]
    fn two_letter_region_abbreviations_are_languages_by_precedence() {
        // `fr` is the French language code, so a lone `(Fr)` is a language,
        // while the full word `(France)` is a region.
        assert_eq!(languages_of_name("Game (Fr)"), vec![LanguageId::Fr]);
        assert_eq!(regions_of_name("Game (Fr)"), Vec::<RegionId>::new());
        assert_eq!(regions_of_name("Game (France)"), vec![RegionId::Other]);
    }

    #[test]
    fn numeric_revision_markers() {
        assert_eq!(revision_of_name("Game (Rev 1)"), (1, true));
        assert_eq!(revision_of_name("Game (Rev 12)"), (12, true));
    }

    #[test]
    fn letter_revision_markers_count_from_a() {
        assert_eq!(revision_of_name("Game (Rev A)"), (1, true));
        assert_eq!(revision_of_name("Game (Rev B)"), (2, true));
    }

    #[test]
    fn no_revision_marker_is_the_original() {
        assert_eq!(revision_of_name("Game (USA)"), (0, false));
        assert_eq!(revision_of_name("Game"), (0, false));
    }

    #[test]
    fn revision_is_taken_from_the_first_well_formed_marker() {
        assert_eq!(revision_of_name("Game (Rev 2) (USA)"), (2, true));
    }

    #[test]
    fn punctuation_and_case_do_not_change_extraction() {
        assert_eq!(regions_of_name("Game (USA,Europe)"), regions_of_name("Game (USA, Europe)"));
        assert_eq!(regions_of_name("Game (USA,Europe)"), regions_of_name("game (usa, europe)"));
    }
}
