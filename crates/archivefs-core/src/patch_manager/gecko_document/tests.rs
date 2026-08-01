//! Fixture-based tests using representative real Dolphin GameSettings INI
//! shapes (modeled directly on files found in a real, on-disk Dolphin
//! profile during this milestone's audit).

use super::*;

/// Shaped like a real user GameSettings file: `[Core]`/`[Video_*]`
/// settings, then a `[Gecko]` section with a header comment code, a real
/// multi-line code with author-suffixed name and notes, and no
/// `[Gecko_Enabled]` section (matching every real sample found - codes
/// present but none enabled yet).
const REAL_WORLD_INI: &str = "[Core]\n\
FastDiscSpeed = True\n\
OverclockEnable = True\n\
Overclock = 1.45\n\
GFXBackend = Vulkan\n\
[Video_Settings]\n\
MSAA = 8\n\
[Gecko]\n\
$======== Ocarina of Time ========\n\
00000000 00000000\n\
$Ocarina of Time -> Togle HUD (Part 1) [Admentus]\n\
28134C58 00000001\n\
20C9F0D4 00060000\n\
00002FC6 00000000\n\
*Press D-Pad Left in the Pause Menu to toggle the HUD\n\
*Requires GameCube Controller\n\
$Ocarina of Time -> 30 FPS Switch (Part 1) [Admentus]\n\
C913CEF5 00000000\n\
08002FC2 00000001\n\
";

fn parse() -> DolphinIniDocument {
    parse_dolphin_ini(REAL_WORLD_INI)
}

#[test]
fn parses_real_gecko_codes_with_names_lines_and_notes() {
    let document = parse();
    assert_eq!(document.gecko_codes.len(), 3);

    // The real "$======== Section ========" divider line found in actual
    // Gecko databases is not a real cheat: splitting on '=' (the same
    // name-extraction convention `dolphin_local`'s own inventory parser
    // already uses, kept consistent here) leaves an empty name, so it is
    // reported and excluded from selection rather than being treated as
    // a genuine, enableable code.
    let header_code = &document.gecko_codes[0];
    assert!(header_code.name.is_empty());
    assert!(!header_code.is_selectable());
    assert!(
        header_code
            .warnings
            .iter()
            .any(|warning| warning.kind == GeckoCodeWarningKind::MissingName)
    );

    let hud_code = &document.gecko_codes[1];
    assert_eq!(
        hud_code.name,
        "Ocarina of Time -> Togle HUD (Part 1) [Admentus]"
    );
    assert_eq!(hud_code.lines.len(), 3);
    assert_eq!(
        hud_code.notes,
        vec![
            "Press D-Pad Left in the Pause Menu to toggle the HUD".to_string(),
            "Requires GameCube Controller".to_string(),
        ]
    );
    assert!(hud_code.is_selectable());

    let fps_code = &document.gecko_codes[2];
    assert_eq!(fps_code.lines.len(), 2);
}

#[test]
fn no_enabled_section_means_no_enabled_names_and_nothing_enabled_by_default() {
    let document = parse();
    assert!(document.gecko_enabled_names.is_empty());
    assert!(
        document
            .gecko_codes
            .iter()
            .all(|code| !code.enabled_by_default),
        "matches every real sample found: codes present, none enabled"
    );
}

#[test]
fn unrelated_sections_are_never_touched_by_parsing() {
    let document = parse();
    // Round-tripping through replace_gecko_enabled_section with an empty
    // selection must reproduce Core/Video_Settings byte-for-byte.
    let rewritten = replace_gecko_enabled_section(&document, &[]);
    assert!(rewritten.contains("[Core]\nFastDiscSpeed = True\nOverclockEnable = True\nOverclock = 1.45\nGFXBackend = Vulkan\n"));
    assert!(rewritten.contains("[Video_Settings]\nMSAA = 8\n"));
}

#[test]
fn replacing_the_enabled_section_preserves_the_gecko_body_exactly() {
    let document = parse();
    let rewritten = replace_gecko_enabled_section(
        &document,
        &["Ocarina of Time -> Togle HUD (Part 1) [Admentus]".to_string()],
    );
    // The [Gecko] section's own code bodies must be byte-identical.
    assert!(rewritten.contains(
        "[Gecko]\n$======== Ocarina of Time ========\n00000000 00000000\n$Ocarina of Time -> Togle HUD (Part 1) [Admentus]\n28134C58 00000001\n20C9F0D4 00060000\n00002FC6 00000000\n*Press D-Pad Left in the Pause Menu to toggle the HUD\n*Requires GameCube Controller\n$Ocarina of Time -> 30 FPS Switch (Part 1) [Admentus]\nC913CEF5 00000000\n08002FC2 00000001\n"
    ));
}

#[test]
fn replacing_the_enabled_section_writes_exactly_the_given_names_in_order() {
    let document = parse();
    let rewritten = replace_gecko_enabled_section(
        &document,
        &[
            "Ocarina of Time -> 30 FPS Switch (Part 1) [Admentus]".to_string(),
            "Ocarina of Time -> Togle HUD (Part 1) [Admentus]".to_string(),
        ],
    );
    let enabled_index = rewritten.find("[Gecko_Enabled]").expect("section appended");
    let enabled_body = &rewritten[enabled_index..];
    assert_eq!(
        enabled_body,
        "[Gecko_Enabled]\n$Ocarina of Time -> 30 FPS Switch (Part 1) [Admentus]\n$Ocarina of Time -> Togle HUD (Part 1) [Admentus]\n"
    );
}

#[test]
fn an_appended_enabled_section_round_trips_through_a_second_parse() {
    let document = parse();
    let rewritten = replace_gecko_enabled_section(
        &document,
        &["Ocarina of Time -> Togle HUD (Part 1) [Admentus]".to_string()],
    );
    let reparsed = parse_dolphin_ini(&rewritten);
    assert_eq!(
        reparsed.gecko_enabled_names,
        vec!["Ocarina of Time -> Togle HUD (Part 1) [Admentus]".to_string()]
    );
    assert_eq!(reparsed.gecko_codes.len(), 3, "the code bodies survive too");
}

#[test]
fn replacing_an_existing_enabled_section_updates_it_in_place_not_appended_twice() {
    let mut with_enabled = REAL_WORLD_INI.to_string();
    with_enabled.push_str("[Gecko_Enabled]\n$Old Code\n");
    let document = parse_dolphin_ini(&with_enabled);
    assert_eq!(document.gecko_enabled_names, vec!["Old Code".to_string()]);

    let rewritten = replace_gecko_enabled_section(&document, &["New Code".to_string()]);
    assert_eq!(rewritten.matches("[Gecko_Enabled]").count(), 1);
    assert!(rewritten.contains("[Gecko_Enabled]\n$New Code\n"));
    assert!(!rewritten.contains("Old Code"));
}

/// Real shape confirmed on disk: a file that references codes from
/// Dolphin's *bundled* database purely by name, with no `[Gecko]`/
/// `[ActionReplay]` body section defining them at all.
#[test]
fn an_enabled_only_file_with_no_body_section_still_parses_its_names() {
    let text = "[ActionReplay_Enabled]\n$Infinite Health\n$Infinite Armor\n";
    let document = parse_dolphin_ini(text);
    assert!(document.gecko_codes.is_empty());
    assert!(
        document.gecko_enabled_names.is_empty(),
        "this file has no Gecko section"
    );
    // Round-tripping must not invent a [Gecko] section or disturb
    // [ActionReplay_Enabled], which this module does not understand.
    let rewritten = replace_gecko_enabled_section(&document, &["New Gecko Code".to_string()]);
    assert!(rewritten.contains("[ActionReplay_Enabled]\n$Infinite Health\n$Infinite Armor\n"));
    assert!(rewritten.contains("[Gecko_Enabled]\n$New Gecko Code\n"));
}

#[test]
fn a_file_with_no_gecko_section_at_all_still_parses_cleanly() {
    let text = "[Core]\nFastDiscSpeed = True\n";
    let document = parse_dolphin_ini(text);
    assert!(document.gecko_codes.is_empty());
    assert!(!document.has_gecko_section());
    assert!(document.warnings.is_empty());
}

#[test]
fn a_code_with_no_body_lines_is_reported_and_not_selectable() {
    let text = "[Gecko]\n$Broken Code\n$Next Code\nAABBCCDD 11223344\n";
    let document = parse_dolphin_ini(text);
    assert_eq!(document.gecko_codes.len(), 2);
    let broken = &document.gecko_codes[0];
    assert!(!broken.is_selectable());
    assert!(
        broken
            .warnings
            .iter()
            .any(|warning| warning.kind == GeckoCodeWarningKind::EmptyCode)
    );
    assert!(document.gecko_codes[1].is_selectable());
    assert_eq!(document.selectable_gecko_count(), 1);
}

#[test]
fn a_malformed_code_line_is_reported_not_silently_dropped_or_repaired() {
    let text = "[Gecko]\n$Some Code\nAABBCCDD 11223344\nthis is not hex\nAABBCCDD 55667788\n";
    let document = parse_dolphin_ini(text);
    let code = &document.gecko_codes[0];
    assert_eq!(
        code.lines,
        vec![
            "AABBCCDD 11223344".to_string(),
            "AABBCCDD 55667788".to_string()
        ],
        "the malformed line is excluded from the retained lines"
    );
    assert!(
        code.warnings
            .iter()
            .any(|warning| warning.kind == GeckoCodeWarningKind::MalformedLine)
    );
    assert!(
        !code.is_selectable(),
        "a code with a malformed line is blocked"
    );
}

#[test]
fn a_code_with_no_name_is_reported() {
    let text = "[Gecko]\n$\nAABBCCDD 11223344\n";
    let document = parse_dolphin_ini(text);
    let code = &document.gecko_codes[0];
    assert!(code.name.is_empty());
    assert!(
        code.warnings
            .iter()
            .any(|warning| warning.kind == GeckoCodeWarningKind::MissingName)
    );
    assert!(!code.is_selectable());
}

#[test]
fn a_malformed_section_header_is_reported_and_does_not_panic() {
    let text = "[Core\nFastDiscSpeed = True\n[Gecko]\n$Code\nAABBCCDD 11223344\n";
    let document = parse_dolphin_ini(text);
    assert!(
        document
            .warnings
            .iter()
            .any(|warning| warning.kind == DolphinIniWarningKind::MalformedSectionHeader)
    );
    // Recovery continues: the well-formed [Gecko] section after it still
    // parses normally.
    assert_eq!(document.gecko_codes.len(), 1);
}

#[test]
fn codes_stay_in_catalogue_order() {
    let text = "[Gecko]\n$Third\nAABBCCDD 11223344\n$First\nAABBCCDD 22334455\n$Second\nAABBCCDD 33445566\n";
    let document = parse_dolphin_ini(text);
    assert_eq!(
        document
            .gecko_codes
            .iter()
            .map(|code| code.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Third", "First", "Second"],
        "file order is catalogue order - Gecko codes have no separate index"
    );
}

#[test]
fn rendering_an_empty_selection_writes_an_empty_enabled_section() {
    let document = parse();
    let rewritten = replace_gecko_enabled_section(&document, &[]);
    assert!(rewritten.contains("[Gecko_Enabled]\n"));
    assert!(rewritten.trim_end().ends_with("[Gecko_Enabled]"));
}

#[test]
fn rendering_is_deterministic() {
    let document = parse();
    let names = vec!["Ocarina of Time -> Togle HUD (Part 1) [Admentus]".to_string()];
    assert_eq!(
        replace_gecko_enabled_section(&document, &names),
        replace_gecko_enabled_section(&document, &names)
    );
}

#[test]
fn arbitrary_and_binary_like_input_never_panics() {
    let inputs = [
        "",
        "[",
        "]",
        "[Gecko]",
        "[Gecko]\n$",
        "[Gecko_Enabled",
        "\0\0\0",
        "[Gecko]\n$Code\n\r\n\r\nAABBCCDD 11223344\r\n",
    ];
    for input in inputs {
        let document = parse_dolphin_ini(input);
        let _ = replace_gecko_enabled_section(&document, &["X".to_string()]);
    }
}
