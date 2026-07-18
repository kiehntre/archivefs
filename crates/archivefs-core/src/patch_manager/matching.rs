//! Small, emulator-neutral exact-identity comparison.
//!
//! Everything else in catalogue matching - platform filtering, the
//! title/region "probable" heuristic, the filename-similarity "uncertain"
//! heuristic, confidence aggregation across candidates, and ambiguity
//! detection - stays in `patch_manager::mod`, which remains the
//! PCSX2-specific orchestration layer for this milestone (see
//! `docs/PATCH_CHEAT_MANAGER_DESIGN.md`'s "Emulator Adapter Architecture"
//! section: "the current code deliberately stops short of the proposed
//! generic trait ... this is acceptable for the first slice"). Only the
//! exact-identity tier - the one part of matching that used to read
//! `record.serial`/`record.executable_crc` directly - is generic here,
//! parameterized over adapter-namespaced [`super::adapter::AdapterIdentityEvidence`]
//! instead of hardcoded field names.

use super::adapter::AdapterIdentityEvidence;

/// What the exact-identity tier decided for one catalogue row, given one
/// record's adapter-namespaced identity evidence and that row's own
/// adapter-namespaced evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactTierOutcome {
    /// The record declares no identity evidence at all, or declares
    /// evidence in a namespace the catalogue row simply has nothing to
    /// say about (no value present, so neither a match nor a conflict).
    /// The caller should fall through to a weaker confidence tier.
    NotApplicable,
    /// Every namespace the record declares evidence in has a matching
    /// value in the catalogue row. Carries one reason per matched
    /// namespace, in the adapter's own wording.
    Exact(Vec<String>),
    /// At least one namespace the record declares evidence in has a
    /// *different* value in the catalogue row. Carries one reason per
    /// conflicting namespace; the caller must treat this row as
    /// unmatched, not merely low-confidence - a conflict can never be
    /// masked by an unrelated namespace matching.
    Conflict(Vec<String>),
}

/// Compares `record_evidence` against `catalogue_evidence` namespace by
/// namespace. A namespace present on both sides must agree to count as a
/// match; a namespace present only on the record side is neither a match
/// nor a conflict, since the catalogue has no value to compare against.
pub fn exact_tier_outcome(
    record_evidence: &[AdapterIdentityEvidence],
    catalogue_evidence: &[AdapterIdentityEvidence],
) -> ExactTierOutcome {
    if record_evidence.is_empty() {
        return ExactTierOutcome::NotApplicable;
    }

    let mut match_reasons = Vec::new();
    let mut conflict_reasons = Vec::new();
    let mut all_matched = true;

    for item in record_evidence {
        let catalogue_value = catalogue_evidence
            .iter()
            .find(|candidate| candidate.namespace == item.namespace)
            .map(|candidate| candidate.value.as_str());
        match catalogue_value {
            Some(value) if value == item.value => match_reasons.push(item.match_reason.to_string()),
            Some(_) => {
                all_matched = false;
                conflict_reasons.push(item.conflict_reason.to_string());
            }
            None => all_matched = false,
        }
    }

    if !conflict_reasons.is_empty() {
        ExactTierOutcome::Conflict(conflict_reasons)
    } else if all_matched {
        ExactTierOutcome::Exact(match_reasons)
    } else {
        ExactTierOutcome::NotApplicable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(namespace: &'static str, value: &str) -> AdapterIdentityEvidence {
        AdapterIdentityEvidence {
            namespace,
            value: value.to_string(),
            match_reason: "matches",
            conflict_reason: "conflicts",
        }
    }

    #[test]
    fn empty_record_evidence_is_not_applicable() {
        assert_eq!(
            exact_tier_outcome(&[], &[evidence("ns", "a")]),
            ExactTierOutcome::NotApplicable
        );
    }

    #[test]
    fn missing_catalogue_value_is_not_applicable_not_conflict() {
        let record_evidence = [evidence("ns", "a")];
        assert_eq!(
            exact_tier_outcome(&record_evidence, &[]),
            ExactTierOutcome::NotApplicable
        );
    }

    #[test]
    fn agreeing_single_namespace_is_exact() {
        let record_evidence = [evidence("ns", "a")];
        let catalogue_evidence = [evidence("ns", "a")];
        assert_eq!(
            exact_tier_outcome(&record_evidence, &catalogue_evidence),
            ExactTierOutcome::Exact(vec!["matches".to_string()])
        );
    }

    #[test]
    fn disagreeing_single_namespace_is_a_conflict() {
        let record_evidence = [evidence("ns", "a")];
        let catalogue_evidence = [evidence("ns", "b")];
        assert_eq!(
            exact_tier_outcome(&record_evidence, &catalogue_evidence),
            ExactTierOutcome::Conflict(vec!["conflicts".to_string()])
        );
    }

    #[test]
    fn one_conflicting_namespace_blocks_exact_even_if_another_namespace_matches() {
        let record_evidence = [evidence("serial", "a"), evidence("crc", "x")];
        let catalogue_evidence = [evidence("serial", "a"), evidence("crc", "y")];
        assert_eq!(
            exact_tier_outcome(&record_evidence, &catalogue_evidence),
            ExactTierOutcome::Conflict(vec!["conflicts".to_string()])
        );
    }

    #[test]
    fn all_required_namespaces_must_match_for_exact() {
        let record_evidence = [evidence("serial", "a"), evidence("crc", "x")];
        // catalogue has serial but nothing for crc - not a conflict, but
        // not exact either, since not every declared namespace matched.
        let catalogue_evidence = [evidence("serial", "a")];
        assert_eq!(
            exact_tier_outcome(&record_evidence, &catalogue_evidence),
            ExactTierOutcome::NotApplicable
        );
    }
}
