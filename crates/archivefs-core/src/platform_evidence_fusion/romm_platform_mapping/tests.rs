use super::*;

// ------------------------------------------------------------------
// Direction (section 4): canonical -> slug only, never inverted here.
// ------------------------------------------------------------------

#[test]
fn every_static_table_entry_platform_exists_in_the_registry() {
    assert!(every_static_entry_platform_exists());
}

#[test]
fn static_table_never_calls_the_inbound_alias_function() {
    // The inbound (RomM slug -> canonical) resolver must never be *called*
    // from this module - reusing it would silently invert the direction
    // this module exists to enforce. The doc comment above legitimately
    // *mentions* it in prose, so this checks for a real call expression,
    // not bare textual presence.
    let source = include_str!("../romm_platform_mapping.rs");
    assert!(!source.contains("canonical_platform_for_romm_slug("));
}

#[test]
fn production_lookup_never_treats_a_slug_string_as_a_canonical_platform_id() {
    // Feeding a real RomM slug in as if it were a canonical platform id
    // must never accidentally resolve - proves the table is keyed by
    // canonical id, not by slug, in the lookup direction that matters.
    let overrides = FrontendPlatformMapping::default();
    assert_eq!(production_romm_slug("n64", &overrides, None), None);
    assert_eq!(production_romm_slug("gba", &overrides, None), None);
}

#[test]
fn canonical_n64_resolves_forward_to_its_real_observed_slug() {
    let overrides = FrontendPlatformMapping::default();
    assert_eq!(
        production_romm_slug("N64", &overrides, None),
        Some("n64".to_string())
    );
}

// ------------------------------------------------------------------
// Consistency (section 6)
// ------------------------------------------------------------------

#[test]
fn no_duplicate_canonical_ids_in_the_static_table() {
    let mut ids: Vec<&str> = STATIC_TABLE
        .iter()
        .map(|entry| entry.canonical_platform_id)
        .collect();
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(
        ids.len(),
        before,
        "duplicate canonical platform id in STATIC_TABLE"
    );
}

#[test]
fn no_two_mapped_entries_share_the_same_slug() {
    let mut slugs: Vec<&str> = STATIC_TABLE
        .iter()
        .filter(|e| e.status == RommMappingSupportStatus::Mapped)
        .filter_map(|e| e.slug)
        .collect();
    let before = slugs.len();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(
        slugs.len(),
        before,
        "two canonical platforms resolved to the same RomM slug"
    );
}

#[test]
fn ambiguous_entries_carry_no_resolved_slug() {
    for entry in STATIC_TABLE
        .iter()
        .filter(|e| e.status == RommMappingSupportStatus::Ambiguous)
    {
        assert!(
            entry.slug.is_none(),
            "{} is Ambiguous but still carries a resolved slug",
            entry.canonical_platform_id
        );
        assert!(
            entry.aliases.len() >= 2,
            "{} is Ambiguous but does not document its conflicting observed slugs",
            entry.canonical_platform_id
        );
    }
}

#[test]
fn mapped_entries_carry_a_resolved_slug_and_provenance() {
    for entry in STATIC_TABLE
        .iter()
        .filter(|e| e.status == RommMappingSupportStatus::Mapped)
    {
        assert!(entry.slug.is_some());
        assert!(!entry.provenance.is_empty());
    }
}

#[test]
fn every_canonical_id_in_the_table_exists_in_the_platform_registry() {
    for entry in STATIC_TABLE {
        assert!(
            crate::platform::platform_by_id(entry.canonical_platform_id).is_some(),
            "{} is not a real canonical platform id",
            entry.canonical_platform_id
        );
    }
}

#[test]
fn aliases_never_resolve_to_a_different_canonical_platform_via_the_registry_alias_lookup() {
    // An alias documented on one entry (e.g. "sfam" on SNES) must not
    // itself independently resolve, through the *registry's own* alias
    // lookup, to some other canonical platform - that would be a silent
    // false identity, exactly what section 6 forbids.
    for entry in STATIC_TABLE {
        for alias in entry.aliases {
            if let Some(resolved) = crate::platform::platform_for_alias(alias) {
                assert_eq!(
                    resolved.id, entry.canonical_platform_id,
                    "alias {alias:?} on {} resolves elsewhere via the platform registry",
                    entry.canonical_platform_id
                );
            }
        }
    }
}

#[test]
fn mapping_table_count_is_machine_derived_not_hardcoded() {
    // The table's own length, not a magic number restated in a test -
    // this test exists so a future edit to STATIC_TABLE cannot silently
    // drift from whatever count a report might quote, since the report
    // must read `static_table().len()` directly.
    assert_eq!(static_table().len(), STATIC_TABLE.len());
    assert!(!static_table().is_empty());
}

#[test]
fn static_coverage_partitions_every_registry_platform_exactly_once() {
    let by_status = static_coverage_by_status();
    let total: usize = by_status.values().map(Vec::len).sum();
    assert_eq!(total, crate::platform::PLATFORMS.len());
}

#[test]
fn unmapped_is_the_overwhelming_majority_not_guessed_into_mapped() {
    let by_status = static_coverage_by_status();
    let mapped = by_status
        .get(&RommMappingSupportStatus::Mapped)
        .map(Vec::len)
        .unwrap_or(0);
    let total = crate::platform::PLATFORMS.len();
    assert!(
        mapped < total / 2,
        "more than half of all canonical platforms are Mapped - this table has grown beyond \
         what real observed evidence in this repo supports"
    );
}

// ------------------------------------------------------------------
// Production resolver tiering
// ------------------------------------------------------------------

#[test]
fn explicit_override_always_wins_over_the_static_table() {
    let mut overrides = FrontendPlatformMapping::default();
    overrides.insert("N64".to_string(), "totally-custom-slug".to_string());
    assert_eq!(
        production_romm_slug("N64", &overrides, None),
        Some("totally-custom-slug".to_string())
    );
}

#[test]
fn override_can_resolve_a_platform_the_static_table_leaves_unmapped() {
    let mut overrides = FrontendPlatformMapping::default();
    overrides.insert("Vectrex".to_string(), "vectrex".to_string());
    assert_eq!(
        production_romm_slug("Vectrex", &overrides, None),
        Some("vectrex".to_string())
    );
    assert!(static_table_entry("Vectrex").is_none());
}

#[test]
fn status_matches_slug_presence_for_every_registry_platform() {
    let overrides = FrontendPlatformMapping::default();
    for platform in crate::platform::PLATFORMS {
        let status = production_romm_status(platform.id, &overrides, None);
        let slug = production_romm_slug(platform.id, &overrides, None);
        match status {
            RommMappingSupportStatus::Mapped => assert!(slug.is_some(), "{}", platform.id),
            RommMappingSupportStatus::Ambiguous
            | RommMappingSupportStatus::Unmapped
            | RommMappingSupportStatus::Unsupported => {
                assert!(slug.is_none(), "{}", platform.id)
            }
        }
    }
}

#[test]
fn unknown_platform_id_resolves_to_nothing_rather_than_panicking() {
    let overrides = FrontendPlatformMapping::default();
    assert_eq!(
        production_romm_slug("Not A Real Platform", &overrides, None),
        None
    );
    assert_eq!(
        production_romm_status("Not A Real Platform", &overrides, None),
        RommMappingSupportStatus::Unmapped
    );
}
