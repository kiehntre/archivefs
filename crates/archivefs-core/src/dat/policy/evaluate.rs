//! Effective-policy resolution and verified-candidate ranking.
//!
//! This is the pure core of the DAT matching policy. It turns the persisted
//! [`DatPolicyConfig`] plus a platform context into an [`EffectiveDatPolicy`]
//! (merging global preferences with per-platform overrides), and ranks a set
//! of *already verified* [`DatCandidate`]s against that policy. Nothing here
//! reads a file, writes a file, or renames anything; the ranking is a pure
//! function of its inputs so the same candidates and policy always produce
//! the same answer.
//!
//! # The ranking is a tie-breaker, never a substitute for evidence
//!
//! Candidates reach this function only after a cryptographic hash verified
//! them against the local file. Within that already-verified set the policy
//! decides the *order of preference*; it can never promote a weaker-evidence
//! candidate, because weaker candidates are not here. This is the design
//! document's rule that "a preference never weakens a match".
//!
//! # Source priority is platform-local
//!
//! Candidate sources are filtered to those that *participate* in the context
//! platform before any comparison, so two sources covering disjoint platforms
//! can never be ranked against each other and a DAT priority is never
//! compared with a cheat priority (there is no shared space to compare in).
//! The comparator then applies source priority only between participating
//! sources, lower number first - the design document's rule, unchanged.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use super::candidate::DatCandidate;
use super::config::{DatPlatformPolicyConfig, DatPolicyConfig};
use super::model::{
    ClonePolicy, LanguageId, LanguagePreference, MAX_POLICY_PREFERENCE_LEN, PolicyField,
    PolicyScope, RegionId, RevisionPolicy,
};
use crate::dat::classification::ContentSelectionPolicy;
use crate::dat::sources::DatSourceRegistry;

// ---------------------------------------------------------------------------
// Effective policy
// ---------------------------------------------------------------------------

/// One source in the resolved consultation order for a platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipatingSource {
    pub id: String,
    pub display_name: String,
    pub priority: u32,
}

/// The fully resolved policy for one platform context.
///
/// Resolution is field-by-field (the design document's §15.2): a per-platform
/// override that sets only `region_preferences` leaves every other field
/// inherited from the global scope. [`EffectiveDatPolicy::scope_of`] records
/// where each resolved value came from so the GUI can show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveDatPolicy {
    /// The canonical platform id this policy is resolved for, or `None` for
    /// the global scope.
    pub platform: Option<String>,
    pub content_selection: ContentSelectionPolicy,
    pub region_preferences: Vec<RegionId>,
    pub language_preferences: Vec<LanguagePreference>,
    pub revision_policy: RevisionPolicy,
    pub clone_policy: ClonePolicy,
    /// The sources that participate in this platform, in consultation order.
    pub source_ordering: Vec<ParticipatingSource>,
    /// For each policy field, which scope supplied the resolved value.
    pub scope_of: BTreeMap<PolicyField, PolicyScope>,
}

/// Resolves the persisted policy for `platform`.
///
/// `platform` must already be canonical: the caller canonicalises once through
/// [`crate::canonical_platform_for_alias`], and this function never
/// re-implements platform canonicalisation. A stored per-platform override
/// whose key is not canonical is not applied (validation reports it).
pub fn resolve(
    config: &DatPolicyConfig,
    platform: Option<&str>,
    participating: Vec<ParticipatingSource>,
) -> EffectiveDatPolicy {
    let canonical_platform =
        platform.filter(|id| crate::canonical_platform_for_alias(id) == Some(id));
    let platform_override: Option<&DatPlatformPolicyConfig> = canonical_platform.and_then(|id| {
        config
            .platforms
            .as_ref()
            .and_then(|overrides| overrides.get(id))
    });

    let region_preferences = match platform_override.and_then(|o| o.region_preferences.as_ref()) {
        Some(list) => parse_regions(list),
        None => parse_regions(config.region_preferences.as_deref().unwrap_or_default()),
    };
    let content_selection = match platform_override.and_then(|o| o.content_selection.as_ref()) {
        Some(value) => ContentSelectionPolicy::parse(value).unwrap_or_default(),
        None => config
            .content_selection
            .as_deref()
            .and_then(ContentSelectionPolicy::parse)
            .unwrap_or_default(),
    };
    let language_preferences = match platform_override.and_then(|o| o.language_preferences.as_ref())
    {
        Some(list) => parse_languages(list),
        None => parse_languages(config.language_preferences.as_deref().unwrap_or_default()),
    };
    let revision_policy = match platform_override.and_then(|o| o.revision_policy.as_ref()) {
        Some(value) => RevisionPolicy::parse(value).unwrap_or_default(),
        None => config
            .revision_policy
            .as_deref()
            .and_then(RevisionPolicy::parse)
            .unwrap_or_default(),
    };
    let clone_policy = match platform_override.and_then(|o| o.clone_policy.as_ref()) {
        Some(value) => ClonePolicy::parse(value).unwrap_or_default(),
        None => config
            .clone_policy
            .as_deref()
            .and_then(ClonePolicy::parse)
            .unwrap_or_default(),
    };

    let scope_of = BTreeMap::from([
        (
            PolicyField::Content,
            scope_for(&platform_override, |o| o.content_selection.is_some()),
        ),
        (
            PolicyField::Region,
            scope_for(&platform_override, |o| o.region_preferences.is_some()),
        ),
        (
            PolicyField::Language,
            scope_for(&platform_override, |o| o.language_preferences.is_some()),
        ),
        (
            PolicyField::Revision,
            scope_for(&platform_override, |o| o.revision_policy.is_some()),
        ),
        (
            PolicyField::Clone,
            scope_for(&platform_override, |o| o.clone_policy.is_some()),
        ),
    ]);

    EffectiveDatPolicy {
        platform: canonical_platform.map(str::to_string),
        content_selection,
        region_preferences,
        language_preferences,
        revision_policy,
        clone_policy,
        source_ordering: participating,
        scope_of,
    }
}

/// Which scope supplied a field: the platform override when it sets that
/// field, otherwise the global scope.
fn scope_for(
    override_: &Option<&DatPlatformPolicyConfig>,
    check: impl Fn(&DatPlatformPolicyConfig) -> bool,
) -> PolicyScope {
    if override_.is_some_and(check) {
        PolicyScope::PlatformOverride
    } else {
        PolicyScope::Global
    }
}

/// The sources that participate in `platform`, in consultation order.
///
/// For a real platform this is the registry's platform-local ordering (an
/// unassigned source participates in every platform); for the global scope it
/// is every enabled source. Excluding non-participating sources *before*
/// ranking is what makes DAT priority platform-local.
pub fn participating_sources(
    registry: &DatSourceRegistry,
    platform: Option<&str>,
) -> Vec<ParticipatingSource> {
    let entries: Vec<&crate::dat::sources::DatSourceEntry> = match platform {
        Some(platform_id) => registry.sorted_enabled_for_platform(platform_id),
        None => registry.sorted_enabled(),
    };
    entries
        .into_iter()
        .map(|entry| ParticipatingSource {
            id: entry.id.clone(),
            display_name: entry.display_name.clone(),
            priority: entry.priority,
        })
        .collect()
}

fn parse_regions(list: &[String]) -> Vec<RegionId> {
    let mut out: Vec<RegionId> = Vec::new();
    for value in list {
        if let Some(region) = RegionId::parse(value)
            && !out.contains(&region)
        {
            out.push(region);
        }
    }
    out
}

fn parse_languages(list: &[String]) -> Vec<LanguagePreference> {
    let mut out: Vec<LanguagePreference> = Vec::new();
    for value in list {
        if let Some(preference) = LanguagePreference::parse(value)
            && !out.contains(&preference)
        {
            out.push(preference);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// One problem found in a persisted policy document.
///
/// Problems never block a save and never drop data: the offending value stays
/// in the file and round-trips. They are surfaced so a hand-edited or newer
/// file is understood rather than silently half-applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyProblem {
    pub field: PolicyField,
    pub message: String,
}

/// Validates a persisted policy document.
///
/// Checks: unknown region ids, unknown language preferences, lists longer
/// than [`MAX_POLICY_PREFERENCE_LEN`], duplicate entries, unknown revision or
/// clone policy names, and per-platform override keys that are not canonical
/// platform ids (or whose own fields are invalid).
pub fn validate_policy_config(config: &DatPolicyConfig) -> Vec<PolicyProblem> {
    let mut problems = Vec::new();

    if let Some(value) = config.content_selection.as_deref()
        && ContentSelectionPolicy::parse(value).is_none()
    {
        problems.push(PolicyProblem {
            field: PolicyField::Content,
            message: format!(
                "unknown content selection '{value}'; kept as written, All entries applies"
            ),
        });
    }

    validate_preference_list(
        config.region_preferences.as_deref(),
        PolicyField::Region,
        "region",
        &mut problems,
        |value| RegionId::parse(value).is_some(),
    );
    validate_preference_list(
        config.language_preferences.as_deref(),
        PolicyField::Language,
        "language preference",
        &mut problems,
        |value| LanguagePreference::parse(value).is_some(),
    );

    if let Some(value) = config.revision_policy.as_deref()
        && RevisionPolicy::parse(value).is_none()
    {
        problems.push(PolicyProblem {
            field: PolicyField::Revision,
            message: format!(
                "unknown revision policy '{value}'; kept as written, safe default applies"
            ),
        });
    }
    if let Some(value) = config.clone_policy.as_deref()
        && ClonePolicy::parse(value).is_none()
    {
        problems.push(PolicyProblem {
            field: PolicyField::Clone,
            message: format!(
                "unknown clone policy '{value}'; kept as written, safe default applies"
            ),
        });
    }

    if let Some(platforms) = config.platforms.as_ref() {
        for (platform_id, override_) in platforms {
            let canonical = crate::canonical_platform_for_alias(platform_id);
            if canonical != Some(platform_id.as_str()) {
                problems.push(PolicyProblem {
                    field: PolicyField::Region,
                    message: format!(
                        "per-platform override key '{platform_id}' is not a canonical platform id; kept as written, not applied"
                    ),
                });
            }
            validate_preference_list(
                override_.region_preferences.as_deref(),
                PolicyField::Region,
                "region",
                &mut problems,
                |value| RegionId::parse(value).is_some(),
            );
            if let Some(value) = override_.content_selection.as_deref()
                && ContentSelectionPolicy::parse(value).is_none()
            {
                problems.push(PolicyProblem {
                    field: PolicyField::Content,
                    message: format!(
                        "unknown content selection '{value}' for platform '{platform_id}'; kept as written, All entries applies"
                    ),
                });
            }
            validate_preference_list(
                override_.language_preferences.as_deref(),
                PolicyField::Language,
                "language preference",
                &mut problems,
                |value| LanguagePreference::parse(value).is_some(),
            );
            if let Some(value) = override_.revision_policy.as_deref()
                && RevisionPolicy::parse(value).is_none()
            {
                problems.push(PolicyProblem {
                    field: PolicyField::Revision,
                    message: format!("unknown revision policy '{value}' for platform '{platform_id}'; kept as written, safe default applies"),
                });
            }
            if let Some(value) = override_.clone_policy.as_deref()
                && ClonePolicy::parse(value).is_none()
            {
                problems.push(PolicyProblem {
                    field: PolicyField::Clone,
                    message: format!("unknown clone policy '{value}' for platform '{platform_id}'; kept as written, safe default applies"),
                });
            }
        }
    }

    problems
}

fn validate_preference_list(
    list: Option<&[String]>,
    field: PolicyField,
    kind: &str,
    problems: &mut Vec<PolicyProblem>,
    known: impl Fn(&str) -> bool,
) {
    let Some(list) = list else { return };
    if list.len() > MAX_POLICY_PREFERENCE_LEN {
        problems.push(PolicyProblem {
            field,
            message: format!(
                "{kind} preference list has {} entries; the limit is {MAX_POLICY_PREFERENCE_LEN}",
                list.len()
            ),
        });
    }
    let mut seen: HashSet<&str> = HashSet::new();
    for value in list {
        if !known(value) {
            problems.push(PolicyProblem {
                field,
                message: format!(
                    "unknown {kind} '{value}' in preferences; kept as written, ignored for matching"
                ),
            });
        }
        if !seen.insert(value.as_str()) {
            problems.push(PolicyProblem {
                field,
                message: format!("duplicate {kind} '{value}' in preferences"),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Candidate ranking
// ---------------------------------------------------------------------------

/// One ranked candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RankedCandidate {
    pub candidate: DatCandidate,
    /// 1-based position in the deterministic display order.
    pub position: usize,
}

/// A candidate excluded from ranking because its source does not participate
/// in the context platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExcludedCandidate {
    pub candidate: DatCandidate,
    pub reason: String,
}

/// The outcome of ranking one set of verified candidates against one policy.
///
/// `entries` is a **display** order: policy-preferred first, ties broken by
/// label so the order never depends on input order. That display order is
/// deliberately not a decision: `decided` is true only when the top entry
/// strictly outranks every other, and `winner_index` then names it. When the
/// policy cannot separate the top candidates the resolution is `ambiguous`,
/// and the deterministic display order simply shows the user what there is to
/// choose between.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateResolution {
    pub entries: Vec<RankedCandidate>,
    pub excluded: Vec<ExcludedCandidate>,
    pub decided: bool,
    pub winner_index: Option<usize>,
    pub ambiguous: bool,
    pub ambiguity_reason: Option<String>,
    /// The reasons behind the ranking, most decisive first, deterministic.
    pub explanations: Vec<String>,
    /// One sentence saying what the policy decided, or why it could not.
    pub summary: String,
}

/// Ranks verified candidates against the effective policy.
pub fn rank_candidates(
    candidates: Vec<DatCandidate>,
    policy: &EffectiveDatPolicy,
) -> CandidateResolution {
    let participating: HashSet<&str> = policy
        .source_ordering
        .iter()
        .map(|source| source.id.as_str())
        .collect();

    let (in_scope, out_of_scope): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|candidate| participating.contains(candidate.source_id.as_str()));

    let excluded: Vec<ExcludedCandidate> = out_of_scope
        .into_iter()
        .map(|candidate| {
            let reason = match &policy.platform {
                Some(platform) => format!(
                    "'{}' does not participate in platform '{platform}'",
                    candidate.source_id
                ),
                None => format!("'{}' is not a participating source", candidate.source_id),
            };
            ExcludedCandidate { candidate, reason }
        })
        .collect();

    let mut ordered = in_scope;
    ordered.sort_by(|a, b| compare(a, b, policy).then_with(|| a.label().cmp(&b.label())));

    let entries: Vec<RankedCandidate> = ordered
        .iter()
        .enumerate()
        .map(|(index, candidate)| RankedCandidate {
            candidate: candidate.clone(),
            position: index + 1,
        })
        .collect();

    let mut explanations: Vec<String> = Vec::new();
    for pair in entries.windows(2) {
        let (ordering, reason) =
            compare_with_reason(&pair[0].candidate, &pair[1].candidate, policy);
        if ordering != std::cmp::Ordering::Equal
            && let Some(reason) = reason
            && !explanations.contains(&reason)
        {
            explanations.push(reason);
        }
    }

    let decided = if entries.is_empty() {
        false
    } else if entries.len() == 1 {
        true
    } else {
        let top = &entries[0].candidate;
        entries[1..]
            .iter()
            .all(|other| compare(top, &other.candidate, policy) == std::cmp::Ordering::Less)
    };

    let ambiguous = !decided && entries.len() > 1;
    let ambiguity_reason = if !ambiguous {
        None
    } else if policy.clone_policy == ClonePolicy::RequireExplicitChoice
        && entries.iter().skip(1).any(|other| {
            entries[0].candidate.is_parent_of(&other.candidate)
                || other.candidate.is_parent_of(&entries[0].candidate)
        })
    {
        Some(
            "a clone and its parent are tied and the policy requires an explicit choice"
                .to_string(),
        )
    } else {
        Some(format!(
            "{} candidates are tied and the policy cannot decide between them",
            entries.len()
        ))
    };

    let winner_index = if decided { Some(0) } else { None };

    let summary = if decided {
        format!("policy prefers '{}'", entries[0].candidate.label())
    } else if entries.is_empty() {
        "no candidates to rank".to_string()
    } else {
        ambiguity_reason
            .clone()
            .unwrap_or_else(|| "ambiguity remains".to_string())
    };

    CandidateResolution {
        entries,
        excluded,
        decided,
        winner_index,
        ambiguous,
        ambiguity_reason,
        explanations,
        summary,
    }
}

/// The policy comparator. `Less` means `a` is preferred over `b`.
///
/// Steps, in order: source priority (lower wins), clone handling under
/// `PreferParent`, region, language, revision, then clone handling under
/// `PreferClone`. The first step that separates the pair decides; an `Equal`
/// result means the pair ties on every step.
fn compare(a: &DatCandidate, b: &DatCandidate, policy: &EffectiveDatPolicy) -> std::cmp::Ordering {
    compare_with_reason(a, b, policy).0
}

fn compare_with_reason(
    a: &DatCandidate,
    b: &DatCandidate,
    policy: &EffectiveDatPolicy,
) -> (std::cmp::Ordering, Option<String>) {
    if a.source_priority != b.source_priority {
        return (
            a.source_priority.cmp(&b.source_priority),
            Some(format!(
                "source priority {} outranked source priority {}",
                a.source_priority, b.source_priority
            )),
        );
    }

    if policy.clone_policy == ClonePolicy::PreferParent
        && let Some(ordering) = parent_vs_clone(a, b)
    {
        return (ordering, Some("parent preferred".to_string()));
    }

    if let Some((ordering, reason)) = compare_region(a, b, policy) {
        return (ordering, Some(reason));
    }
    if let Some((ordering, reason)) = compare_language(a, b, policy) {
        return (ordering, Some(reason));
    }
    if let Some((ordering, reason)) = compare_revision(a, b, policy) {
        return (ordering, Some(reason));
    }

    if policy.clone_policy == ClonePolicy::PreferClone
        && let Some(ordering) = parent_vs_clone(a, b)
    {
        return (ordering, Some("parent preferred on a tie".to_string()));
    }

    (std::cmp::Ordering::Equal, None)
}

/// Orders a clone below its parent, when one is the other's parent.
fn parent_vs_clone(a: &DatCandidate, b: &DatCandidate) -> Option<std::cmp::Ordering> {
    if a.is_parent_of(b) {
        Some(std::cmp::Ordering::Less)
    } else if b.is_parent_of(a) {
        Some(std::cmp::Ordering::Greater)
    } else {
        None
    }
}

fn compare_region(
    a: &DatCandidate,
    b: &DatCandidate,
    policy: &EffectiveDatPolicy,
) -> Option<(std::cmp::Ordering, String)> {
    if policy.region_preferences.is_empty() {
        return None;
    }
    let a_position = best_region_position(&a.regions, &policy.region_preferences);
    let b_position = best_region_position(&b.regions, &policy.region_preferences);
    match (a_position, b_position) {
        (Some(pa), Some(pb)) if pa == pb => None,
        (Some(pa), Some(pb)) => {
            let region = policy.region_preferences[pa.min(pb)];
            Some((
                pa.cmp(&pb),
                format!("preferred region matched ({})", region.label()),
            ))
        }
        (Some(pa), None) => {
            let region = policy.region_preferences[pa];
            Some((
                std::cmp::Ordering::Less,
                format!("preferred region matched ({})", region.label()),
            ))
        }
        (None, Some(pb)) => {
            let region = policy.region_preferences[pb];
            Some((
                std::cmp::Ordering::Greater,
                format!("preferred region matched ({})", region.label()),
            ))
        }
        (None, None) => None,
    }
}

fn best_region_position(regions: &[RegionId], preferences: &[RegionId]) -> Option<usize> {
    preferences
        .iter()
        .position(|preferred| regions.contains(preferred))
}

fn compare_language(
    a: &DatCandidate,
    b: &DatCandidate,
    policy: &EffectiveDatPolicy,
) -> Option<(std::cmp::Ordering, String)> {
    if policy.language_preferences.is_empty() {
        return None;
    }
    let a_position = best_language_position(a, &policy.language_preferences);
    let b_position = best_language_position(b, &policy.language_preferences);
    match (a_position, b_position) {
        (Some(pa), Some(pb)) if pa == pb => None,
        (Some(pa), Some(pb)) => {
            let preference = policy.language_preferences[pa.min(pb)];
            Some((
                pa.cmp(&pb),
                format!("preferred language matched ({})", preference.label()),
            ))
        }
        (Some(pa), None) => {
            let preference = policy.language_preferences[pa];
            Some((
                std::cmp::Ordering::Less,
                format!("preferred language matched ({})", preference.label()),
            ))
        }
        (None, Some(pb)) => {
            let preference = policy.language_preferences[pb];
            Some((
                std::cmp::Ordering::Greater,
                format!("preferred language matched ({})", preference.label()),
            ))
        }
        (None, None) => None,
    }
}

fn best_language_position(
    candidate: &DatCandidate,
    preferences: &[LanguagePreference],
) -> Option<usize> {
    preferences
        .iter()
        .position(|preference| language_preference_matches(*preference, candidate))
}

fn language_preference_matches(preference: LanguagePreference, candidate: &DatCandidate) -> bool {
    match preference {
        LanguagePreference::Language(language) => candidate.languages.contains(&language),
        LanguagePreference::MultiLanguage => candidate.languages.len() > 1,
        LanguagePreference::OriginalLanguage => original_language_of(candidate)
            .is_some_and(|original| candidate.languages.contains(&original)),
    }
}

/// The language a release is conventionally published in, inferred from its
/// primary region tag.
///
/// This is a documented heuristic, not a property the catalogue declares:
/// an `Original language` preference has to decide something, and the most
/// honest deterministic answer available is "the language that release
/// region normally publishes in" (USA/World/Europe → English, Japan →
/// Japanese). A candidate whose region resolved to `Other` - or which has no
/// region tag - makes no claim, so `Original` never matches it.
pub fn original_language_of(candidate: &DatCandidate) -> Option<LanguageId> {
    let primary = candidate.regions.first()?;
    match primary {
        RegionId::Usa | RegionId::World | RegionId::Europe => Some(LanguageId::En),
        RegionId::Japan => Some(LanguageId::Ja),
        RegionId::Other => None,
    }
}

fn compare_revision(
    a: &DatCandidate,
    b: &DatCandidate,
    policy: &EffectiveDatPolicy,
) -> Option<(std::cmp::Ordering, String)> {
    match policy.revision_policy {
        RevisionPolicy::AskWhenAmbiguous => None,
        RevisionPolicy::LatestVerified => {
            if a.revision != b.revision {
                let newer = a.revision.max(b.revision);
                Some((
                    b.revision.cmp(&a.revision),
                    format!("newer verified revision preferred (Rev {newer})"),
                ))
            } else if a.has_revision_marker != b.has_revision_marker {
                // A marked entry outranks an unmarked one even when their
                // numeric revisions are equal: "(Rev 0)" is still an explicit
                // declaration of a chosen revision. `Less` means `a` wins, so
                // `b.marker.cmp(a.marker)` (true > false) prefers the marked
                // side.
                Some((
                    b.has_revision_marker.cmp(&a.has_revision_marker),
                    "newer verified revision preferred".to_string(),
                ))
            } else {
                None
            }
        }
        RevisionPolicy::EarliestVerified => {
            if a.revision != b.revision {
                let earlier = a.revision.min(b.revision);
                Some((
                    a.revision.cmp(&b.revision),
                    format!("earliest verified revision preferred (Rev {earlier})"),
                ))
            } else {
                None
            }
        }
        RevisionPolicy::PreferOriginal => match (a.has_revision_marker, b.has_revision_marker) {
            (false, true) => Some((
                std::cmp::Ordering::Less,
                "original (unrevised) revision preferred".to_string(),
            )),
            (true, false) => Some((
                std::cmp::Ordering::Greater,
                "original (unrevised) revision preferred".to_string(),
            )),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dat::policy::config::{DatPlatformPolicyConfig, DatPolicyConfig};
    use crate::dat::policy::tags::{languages_of_name, regions_of_name, revision_of_name};
    use std::collections::BTreeMap;

    fn candidate_named(source_id: &str, priority: u32, game_name: &str) -> DatCandidate {
        let (revision, has_revision_marker) = revision_of_name(game_name);
        DatCandidate {
            source_id: source_id.into(),
            source_priority: priority,
            game_name: game_name.into(),
            rom_name: game_name.into(),
            regions: regions_of_name(game_name),
            languages: languages_of_name(game_name),
            revision,
            has_revision_marker,
            parent_name: None,
        }
    }

    fn as_clone_of(mut candidate: DatCandidate, parent: &str) -> DatCandidate {
        candidate.parent_name = Some(parent.to_string());
        candidate
    }

    fn context(config: DatPolicyConfig, sources: &[(&str, u32)]) -> EffectiveDatPolicy {
        let participating = sources
            .iter()
            .map(|(id, priority)| ParticipatingSource {
                id: (*id).to_string(),
                display_name: (*id).to_string(),
                priority: *priority,
            })
            .collect();
        resolve(&config, None, participating)
    }

    fn config(
        region: Option<Vec<&str>>,
        language: Option<Vec<&str>>,
        revision: Option<&str>,
        clone: Option<&str>,
    ) -> DatPolicyConfig {
        DatPolicyConfig {
            region_preferences: region.map(|list| list.into_iter().map(str::to_string).collect()),
            language_preferences: language
                .map(|list| list.into_iter().map(str::to_string).collect()),
            revision_policy: revision.map(str::to_string),
            clone_policy: clone.map(str::to_string),
            content_selection: None,
            platforms: None,
            unknown_fields: toml::Table::new(),
        }
    }

    // ---- region ordering --------------------------------------------------

    #[test]
    fn region_preference_orders_candidates() {
        let policy = context(
            config(Some(vec!["europe", "usa"]), None, None, None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (USA)"),
                candidate_named("src", 100, "Game (Europe)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.winner_index, Some(0));
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (Europe)");
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "preferred region matched (Europe)"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn a_candidate_matching_no_preferred_region_ranks_last() {
        let policy = context(
            config(Some(vec!["europe", "usa"]), None, None, None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (Japan)"),
                candidate_named("src", 100, "Game (Europe)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (Europe)");
        assert_eq!(resolution.entries[1].candidate.game_name, "Game (Japan)");
    }

    // ---- language ordering ------------------------------------------------

    #[test]
    fn language_preference_orders_candidates() {
        let policy = context(
            config(None, Some(vec!["ja", "en"]), None, None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (En)"),
                candidate_named("src", 100, "Game (Ja)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (Ja)");
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "preferred language matched (Japanese)"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn multi_language_preference_matches_entries_with_more_than_one_tag() {
        let policy = context(
            config(None, Some(vec!["multi", "en"]), None, None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (En)"),
                candidate_named("src", 100, "Game (En,Fr,De)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (En,Fr,De)");
    }

    #[test]
    fn original_language_preference_uses_the_regions_language() {
        let policy = context(
            config(None, Some(vec!["original"]), None, None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (Japan) (En)"),
                candidate_named("src", 100, "Game (Japan) (Ja)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(
            resolution.entries[0].candidate.game_name,
            "Game (Japan) (Ja)"
        );
    }

    // ---- revision policy --------------------------------------------------

    #[test]
    fn latest_revision_is_preferred() {
        let policy = context(
            config(None, None, Some("latest_verified"), None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (Rev 1)"),
                candidate_named("src", 100, "Game (Rev 2)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (Rev 2)");
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "newer verified revision preferred (Rev 2)"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn a_marked_entry_outranks_an_unmarked_one_under_latest() {
        let policy = context(
            config(None, None, Some("latest_verified"), None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (USA)"),
                candidate_named("src", 100, "Game (USA) (Rev 1)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(
            resolution.entries[0].candidate.game_name,
            "Game (USA) (Rev 1)"
        );
    }

    #[test]
    fn an_explicit_rev_0_outranks_an_unmarked_original_under_latest() {
        // A "(Rev 0)" marker is explicit: it declares revision 0 deliberately.
        // Under LatestVerified the marked entry must still outrank an unmarked
        // one, because the marker says a revision was chosen even when that
        // revision happens to be 0.
        let policy = context(
            config(None, None, Some("latest_verified"), None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (USA)"),
                candidate_named("src", 100, "Game (USA) (Rev 0)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(
            resolution.entries[0].candidate.game_name, "Game (USA) (Rev 0)",
            "the explicitly marked Rev 0 entry must be preferred"
        );
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "newer verified revision preferred"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn explicit_rev_0_wins_regardless_of_input_order_under_latest() {
        let policy = context(
            config(None, None, Some("latest_verified"), None),
            &[("src", 100)],
        );
        let unmarked = candidate_named("src", 100, "Game (USA)");
        let marked = candidate_named("src", 100, "Game (USA) (Rev 0)");

        let forward = rank_candidates(vec![unmarked.clone(), marked.clone()], &policy);
        let reversed = rank_candidates(vec![marked.clone(), unmarked.clone()], &policy);
        assert_eq!(
            forward, reversed,
            "the ranking must not depend on input order"
        );
        assert!(forward.decided);
        assert_eq!(forward.entries[0].candidate.game_name, "Game (USA) (Rev 0)");
        assert_eq!(
            reversed.entries[0].candidate.game_name,
            "Game (USA) (Rev 0)"
        );
    }

    #[test]
    fn an_unmarked_original_still_outranks_explicit_rev_0_under_prefer_original() {
        // PreferOriginal is the opposite contract: the unmarked original wins,
        // and an explicit "(Rev 0)" marker is still a marker, so it loses.
        let policy = context(
            config(None, None, Some("prefer_original"), None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (USA)"),
                candidate_named("src", 100, "Game (USA) (Rev 0)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(
            resolution.entries[0].candidate.game_name, "Game (USA)",
            "the unmarked original must win under PreferOriginal"
        );
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "original (unrevised) revision preferred"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn earliest_revision_is_preferred() {
        let policy = context(
            config(None, None, Some("earliest_verified"), None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (Rev 2)"),
                candidate_named("src", 100, "Game (Rev 1)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (Rev 1)");
    }

    #[test]
    fn prefer_original_ranks_an_unrevised_entry_first() {
        let policy = context(
            config(None, None, Some("prefer_original"), None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (USA)"),
                candidate_named("src", 100, "Game (USA) (Rev 1)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (USA)");
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "original (unrevised) revision preferred"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn ask_when_ambiguous_never_lets_revision_decide() {
        let policy = context(
            config(None, None, Some("ask_when_ambiguous"), None),
            &[("src", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (Rev 1)"),
                candidate_named("src", 100, "Game (Rev 2)"),
            ],
            &policy,
        );
        assert!(!resolution.decided);
        assert!(resolution.ambiguous);
    }

    // ---- parent / clone ---------------------------------------------------

    #[test]
    fn prefer_parent_outranks_a_clone_of_it() {
        let policy = context(
            config(None, None, None, Some("prefer_parent")),
            &[("src", 100)],
        );
        let parent = candidate_named("src", 100, "Game (USA)");
        let clone = as_clone_of(
            candidate_named("src", 100, "Game (USA) (Rev 1)"),
            "Game (USA)",
        );
        let resolution = rank_candidates(vec![clone, parent], &policy);
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (USA)");
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "parent preferred"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn prefer_parent_wins_even_when_the_clone_has_the_better_region() {
        let policy = context(
            config(Some(vec!["japan"]), None, None, Some("prefer_parent")),
            &[("src", 100)],
        );
        let parent = candidate_named("src", 100, "Game (USA)");
        let clone = as_clone_of(candidate_named("src", 100, "Game (Japan)"), "Game (USA)");
        let resolution = rank_candidates(vec![clone, parent], &policy);
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (USA)");
    }

    #[test]
    fn prefer_clone_allows_region_to_promote_a_clone_above_its_parent() {
        let policy = context(
            config(Some(vec!["japan"]), None, None, Some("prefer_clone")),
            &[("src", 100)],
        );
        let parent = candidate_named("src", 100, "Game (USA)");
        let clone = as_clone_of(candidate_named("src", 100, "Game (Japan)"), "Game (USA)");
        let resolution = rank_candidates(vec![clone, parent], &policy);
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (Japan)");
    }

    #[test]
    fn prefer_clone_keeps_the_parent_when_region_ties() {
        let policy = context(
            config(Some(vec!["usa"]), None, None, Some("prefer_clone")),
            &[("src", 100)],
        );
        let parent = candidate_named("src", 100, "Game (USA)");
        let clone = as_clone_of(
            candidate_named("src", 100, "Game (USA) (Rev 1)"),
            "Game (USA)",
        );
        let resolution = rank_candidates(vec![clone, parent], &policy);
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (USA)");
    }

    #[test]
    fn keep_all_variants_ignores_parent_relationships() {
        let policy = context(
            config(None, None, None, Some("keep_all_variants")),
            &[("src", 100)],
        );
        let parent = candidate_named("src", 100, "Game (USA)");
        let clone = as_clone_of(
            candidate_named("src", 100, "Game (USA) (Rev 1)"),
            "Game (USA)",
        );
        let resolution = rank_candidates(vec![clone, parent], &policy);
        assert!(!resolution.decided);
        assert!(resolution.ambiguous);
        assert_eq!(resolution.entries.len(), 2);
    }

    #[test]
    fn require_explicit_choice_marks_a_parent_clone_tie_ambiguous() {
        let policy = context(
            config(None, None, None, Some("require_explicit_choice")),
            &[("src", 100)],
        );
        let parent = candidate_named("src", 100, "Game (USA)");
        let clone = as_clone_of(
            candidate_named("src", 100, "Game (USA) (Rev 1)"),
            "Game (USA)",
        );
        let resolution = rank_candidates(vec![clone, parent], &policy);
        assert!(!resolution.decided);
        assert!(resolution.ambiguous);
        assert!(
            resolution
                .ambiguity_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("explicit choice")),
            "{:?}",
            resolution.ambiguity_reason
        );
    }

    // ---- ambiguity --------------------------------------------------------

    #[test]
    fn equivalent_candidates_are_ambiguous_not_arbitrarily_chosen() {
        let policy = context(config(None, None, None, None), &[("src", 100)]);
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (USA)"),
                candidate_named("src", 100, "Game (Europe)"),
            ],
            &policy,
        );
        assert!(!resolution.decided);
        assert!(resolution.ambiguous);
        assert_eq!(resolution.winner_index, None);
        // Display order is still deterministic.
        assert_eq!(resolution.entries[0].candidate.game_name, "Game (Europe)");
        assert_eq!(resolution.entries[1].candidate.game_name, "Game (USA)");
    }

    // ---- source priority --------------------------------------------------

    #[test]
    fn source_priority_lower_number_wins() {
        let policy = context(
            config(None, None, None, None),
            &[("primary", 20), ("backup", 100)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("backup", 100, "Game"),
                candidate_named("primary", 20, "Game"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.source_id, "primary");
        assert!(
            resolution
                .explanations
                .iter()
                .any(|line| line == "source priority 20 outranked source priority 100"),
            "{:?}",
            resolution.explanations
        );
    }

    #[test]
    fn equal_priorities_tie_break_by_the_other_criteria() {
        // Two sources at the same priority, one with a preferred region: the
        // region decides, not an arbitrary source ordering.
        let policy = context(
            config(Some(vec!["europe"]), None, None, None),
            &[("a", 50), ("b", 50)],
        );
        let resolution = rank_candidates(
            vec![
                candidate_named("b", 50, "Game (USA)"),
                candidate_named("a", 50, "Game (Europe)"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert_eq!(resolution.entries[0].candidate.source_id, "a");
    }

    // ---- per-platform participation and disjoint platforms ----------------

    #[test]
    fn disjoint_platforms_never_compare_priorities() {
        let participating = vec![ParticipatingSource {
            id: "nes-source".to_string(),
            display_name: "NES source".to_string(),
            priority: 100,
        }];
        let policy = resolve(&config(None, None, None, None), Some("NES"), participating);
        let resolution = rank_candidates(
            vec![
                candidate_named("nes-source", 100, "Game"),
                candidate_named("snes-source", 20, "Game"),
            ],
            &policy,
        );
        // The SNES source does not participate for NES: it is excluded, never
        // compared, even though its priority number (20) would beat 100.
        assert!(resolution.decided);
        assert_eq!(resolution.entries.len(), 1);
        assert_eq!(resolution.excluded.len(), 1);
        assert_eq!(resolution.excluded[0].candidate.source_id, "snes-source");
        assert!(
            resolution.excluded[0]
                .reason
                .contains("does not participate"),
            "{:?}",
            resolution.excluded[0].reason
        );
    }

    #[test]
    fn a_source_that_participates_in_the_platform_is_not_excluded() {
        let participating = vec![
            ParticipatingSource {
                id: "nes-source".to_string(),
                display_name: "NES".to_string(),
                priority: 100,
            },
            ParticipatingSource {
                id: "shared-source".to_string(),
                display_name: "Shared".to_string(),
                priority: 50,
            },
        ];
        let policy = resolve(&config(None, None, None, None), Some("NES"), participating);
        let resolution = rank_candidates(
            vec![
                candidate_named("shared-source", 50, "Game"),
                candidate_named("nes-source", 100, "Game"),
            ],
            &policy,
        );
        assert!(resolution.decided);
        assert!(resolution.excluded.is_empty());
        assert_eq!(resolution.entries[0].candidate.source_id, "shared-source");
    }

    // ---- safe defaults and empty preferences ------------------------------

    #[test]
    fn safe_defaults_preserve_todays_report_everything_behaviour() {
        let policy = context(DatPolicyConfig::default(), &[("src", 100)]);
        let resolution = rank_candidates(
            vec![
                candidate_named("src", 100, "Game (USA) (Rev 1)"),
                candidate_named("src", 100, "Game (Europe)"),
            ],
            &policy,
        );
        // No preferences, revision asks when ambiguous, clone keeps all: every
        // candidate is retained and nothing is decided.
        assert_eq!(resolution.entries.len(), 2);
        assert!(!resolution.decided);
        assert!(resolution.ambiguous);
        assert!(resolution.explanations.is_empty());
    }

    #[test]
    fn empty_preference_lists_are_the_same_as_absent() {
        let with_empty_lists = context(
            config(Some(vec![]), Some(vec![]), None, None),
            &[("src", 100)],
        );
        let with_absent = context(DatPolicyConfig::default(), &[("src", 100)]);
        let a = rank_candidates(
            vec![candidate_named("src", 100, "Game (USA)")],
            &with_empty_lists,
        );
        let b = rank_candidates(
            vec![candidate_named("src", 100, "Game (USA)")],
            &with_absent,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn a_single_candidate_is_decided_without_ceremony() {
        let policy = context(DatPolicyConfig::default(), &[("src", 100)]);
        let resolution = rank_candidates(vec![candidate_named("src", 100, "Game")], &policy);
        assert!(resolution.decided);
        assert_eq!(resolution.winner_index, Some(0));
        assert!(!resolution.ambiguous);
    }

    // ---- validation -------------------------------------------------------

    #[test]
    fn unknown_region_and_language_values_are_reported_but_preserved() {
        let config = DatPolicyConfig {
            region_preferences: Some(vec!["moon".to_string(), "europe".to_string()]),
            language_preferences: Some(vec!["xx".to_string(), "en".to_string()]),
            ..Default::default()
        };
        let problems = validate_policy_config(&config);
        assert!(
            problems
                .iter()
                .any(|p| p.field == PolicyField::Region && p.message.contains("moon")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.field == PolicyField::Language && p.message.contains("xx")),
            "{problems:?}"
        );
        // The values still resolve to what they can.
        let policy = resolve(&config, None, vec![]);
        assert_eq!(policy.region_preferences, vec![RegionId::Europe]);
        assert_eq!(
            policy.language_preferences,
            vec![LanguagePreference::Language(LanguageId::En)]
        );
    }

    #[test]
    fn duplicate_and_overlong_preference_lists_are_reported() {
        let overlong: Vec<String> = (0..17).map(|i| format!("europe{i}")).collect();
        let config = DatPolicyConfig {
            region_preferences: Some(overlong),
            ..Default::default()
        };
        let problems = validate_policy_config(&config);
        assert!(
            problems.iter().any(|p| p.message.contains("limit is 16")),
            "{problems:?}"
        );

        let config = DatPolicyConfig {
            language_preferences: Some(vec!["en".to_string(), "en".to_string()]),
            ..Default::default()
        };
        let problems = validate_policy_config(&config);
        assert!(
            problems.iter().any(|p| p.message.contains("duplicate")),
            "{problems:?}"
        );
    }

    #[test]
    fn unknown_revision_and_clone_values_are_reported_and_fall_back() {
        let config = DatPolicyConfig {
            revision_policy: Some("newest_future_policy".to_string()),
            clone_policy: Some("collapse_clones".to_string()),
            ..Default::default()
        };
        let problems = validate_policy_config(&config);
        assert!(problems.iter().any(|p| p.field == PolicyField::Revision));
        assert!(problems.iter().any(|p| p.field == PolicyField::Clone));
        let policy = resolve(&config, None, vec![]);
        assert_eq!(policy.revision_policy, RevisionPolicy::default());
        assert_eq!(policy.clone_policy, ClonePolicy::default());
    }

    #[test]
    fn per_platform_override_keys_must_be_canonical_platform_ids() {
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "nes".to_string(),
            DatPlatformPolicyConfig {
                region_preferences: Some(vec!["europe".to_string()]),
                ..Default::default()
            },
        );
        let config = DatPolicyConfig {
            platforms: Some(platforms),
            ..Default::default()
        };
        let problems = validate_policy_config(&config);
        assert!(
            problems
                .iter()
                .any(|p| p.message.contains("not a canonical platform id")),
            "{problems:?}"
        );
        // The canonical spelling is accepted.
        let mut platforms = BTreeMap::new();
        platforms.insert("NES".to_string(), DatPlatformPolicyConfig::default());
        let config = DatPolicyConfig {
            platforms: Some(platforms),
            ..Default::default()
        };
        assert!(validate_policy_config(&config).is_empty());
    }

    #[test]
    fn a_per_platform_override_overrides_only_the_fields_it_sets() {
        let mut platforms = BTreeMap::new();
        platforms.insert(
            "NES".to_string(),
            DatPlatformPolicyConfig {
                region_preferences: Some(vec!["japan".to_string()]),
                ..Default::default()
            },
        );
        let config = DatPolicyConfig {
            region_preferences: Some(vec!["europe".to_string()]),
            language_preferences: Some(vec!["en".to_string()]),
            platforms: Some(platforms),
            ..Default::default()
        };
        let policy = resolve(&config, Some("NES"), vec![]);
        // Region comes from the platform override, language falls through.
        assert_eq!(policy.region_preferences, vec![RegionId::Japan]);
        assert_eq!(
            policy.language_preferences,
            vec![LanguagePreference::Language(LanguageId::En)]
        );
        assert_eq!(
            policy.scope_of[&PolicyField::Region],
            PolicyScope::PlatformOverride
        );
        assert_eq!(policy.scope_of[&PolicyField::Language], PolicyScope::Global);
        // A different platform is unaffected.
        let other = resolve(&config, Some("SNES"), vec![]);
        assert_eq!(other.region_preferences, vec![RegionId::Europe]);
        assert_eq!(other.scope_of[&PolicyField::Region], PolicyScope::Global);
    }

    #[test]
    fn resolution_is_deterministic_across_input_orders() {
        let policy = context(
            config(
                Some(vec!["europe", "usa"]),
                Some(vec!["en"]),
                Some("latest_verified"),
                None,
            ),
            &[("src", 100)],
        );
        let candidates = vec![
            candidate_named("src", 100, "Game (USA) (Rev 1)"),
            candidate_named("src", 100, "Game (Europe) (Rev 2)"),
            candidate_named("src", 100, "Game (Japan)"),
        ];
        let forward = rank_candidates(candidates.clone(), &policy);
        let reversed_input = {
            let mut reversed = candidates;
            reversed.reverse();
            rank_candidates(reversed, &policy)
        };
        assert_eq!(forward, reversed_input);
        assert_eq!(
            forward.entries[0].candidate.game_name,
            "Game (Europe) (Rev 2)"
        );
    }
}
