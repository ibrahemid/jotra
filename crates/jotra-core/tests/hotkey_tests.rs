use std::collections::BTreeSet;

use jotra_core::hotkey::{
    check_global_chord, is_wayland_session, parse_hotkey_chord, GlobalChordError, HotkeyKey,
    HotkeyMod, HotkeyParseError,
};

#[test]
fn parses_default_summon_chord() {
    let chord = parse_hotkey_chord("CmdOrCtrl+Shift+Space").expect("parse failed");
    assert_eq!(chord.key(), HotkeyKey::Space);
    assert!(chord.has_modifier(HotkeyMod::CmdOrCtrl));
    assert!(chord.has_modifier(HotkeyMod::Shift));
    assert!(!chord.has_modifier(HotkeyMod::Ctrl));
    assert!(!chord.has_modifier(HotkeyMod::Cmd));
    assert!(!chord.has_modifier(HotkeyMod::Alt));
}

#[test]
fn parses_literal_cmd_chord_for_mac_users() {
    let chord = parse_hotkey_chord("Cmd+Shift+Space").expect("parse failed");
    assert!(chord.has_modifier(HotkeyMod::Cmd));
    assert!(!chord.has_modifier(HotkeyMod::CmdOrCtrl));
    assert_eq!(chord.key(), HotkeyKey::Space);
}

#[test]
fn parses_literal_ctrl_chord_on_any_platform() {
    let chord = parse_hotkey_chord("Ctrl+Alt+W").expect("parse failed");
    assert!(chord.has_modifier(HotkeyMod::Ctrl));
    assert!(chord.has_modifier(HotkeyMod::Alt));
    assert!(!chord.has_modifier(HotkeyMod::CmdOrCtrl));
    assert_eq!(chord.key(), HotkeyKey::KeyW);
}

#[test]
fn parser_is_case_insensitive() {
    let chord = parse_hotkey_chord("cmdorctrl+SHIFT+space").expect("parse failed");
    assert!(chord.has_modifier(HotkeyMod::CmdOrCtrl));
    assert!(chord.has_modifier(HotkeyMod::Shift));
    assert_eq!(chord.key(), HotkeyKey::Space);
}

#[test]
fn parser_accepts_alternate_modifier_spellings() {
    let chord = parse_hotkey_chord("Control+Option+T").expect("parse failed");
    assert!(chord.has_modifier(HotkeyMod::Ctrl));
    assert!(chord.has_modifier(HotkeyMod::Alt));
    assert_eq!(chord.key(), HotkeyKey::KeyT);
}

#[test]
fn parser_dedupes_repeated_modifiers() {
    let chord = parse_hotkey_chord("Shift+Shift+Space").expect("parse failed");
    assert_eq!(chord.modifiers().len(), 1);
    assert_eq!(chord.modifiers()[0], HotkeyMod::Shift);
}

#[test]
fn parses_function_keys() {
    let chord = parse_hotkey_chord("CmdOrCtrl+F12").expect("parse failed");
    assert_eq!(chord.key(), HotkeyKey::F12);
}

#[test]
fn parses_digit_keys() {
    let chord = parse_hotkey_chord("CmdOrCtrl+1").expect("parse failed");
    assert_eq!(chord.key(), HotkeyKey::Digit1);
}

#[test]
fn parser_rejects_empty_input() {
    assert_eq!(parse_hotkey_chord(""), Err(HotkeyParseError::Empty));
    assert_eq!(parse_hotkey_chord("   "), Err(HotkeyParseError::Empty));
}

#[test]
fn parser_rejects_missing_key() {
    assert_eq!(
        parse_hotkey_chord("CmdOrCtrl+Shift"),
        Err(HotkeyParseError::MissingKey)
    );
}

#[test]
fn parser_rejects_multiple_keys() {
    assert_eq!(
        parse_hotkey_chord("CmdOrCtrl+A+B"),
        Err(HotkeyParseError::MultipleKeys)
    );
}

#[test]
fn parser_rejects_unknown_token() {
    match parse_hotkey_chord("CmdOrCtrl+Bogus+Space") {
        Err(HotkeyParseError::UnknownToken(t)) => assert_eq!(t, "Bogus"),
        other => panic!("expected UnknownToken, got {other:?}"),
    }
}

#[test]
fn parser_rejects_empty_token_between_pluses() {
    match parse_hotkey_chord("CmdOrCtrl++Space") {
        Err(HotkeyParseError::UnknownToken(_)) => {}
        other => panic!("expected UnknownToken, got {other:?}"),
    }
}

#[test]
fn equal_chords_normalize_modifier_set() {
    let a = parse_hotkey_chord("CmdOrCtrl+Shift+Space").expect("parse failed");
    let b = parse_hotkey_chord("Shift+CmdOrCtrl+Space").expect("parse failed");
    assert_eq!(a.modifiers().len(), b.modifiers().len());
    assert!(a.has_modifier(HotkeyMod::CmdOrCtrl) && a.has_modifier(HotkeyMod::Shift));
    assert!(b.has_modifier(HotkeyMod::CmdOrCtrl) && b.has_modifier(HotkeyMod::Shift));
    assert_eq!(a.key(), b.key());
}

#[test]
fn parses_function_keys_past_f12() {
    assert_eq!(
        parse_hotkey_chord("F13").expect("parse failed").key(),
        HotkeyKey::F13
    );
    assert_eq!(
        parse_hotkey_chord("Shift+F24").expect("parse failed").key(),
        HotkeyKey::F24
    );
    assert!(matches!(
        parse_hotkey_chord("F25"),
        Err(HotkeyParseError::UnknownToken(_))
    ));
}

#[test]
fn a_bare_key_cannot_be_held_globally() {
    for chord in ["A", "Space", "F2", "Shift+A", "Shift+Space"] {
        let parsed = parse_hotkey_chord(chord).expect("parse failed");
        assert_eq!(
            check_global_chord(&parsed),
            Err(GlobalChordError::NeedsModifier),
            "{chord} would take a key from every app"
        );
    }
}

#[test]
fn a_chord_with_cmd_ctrl_or_alt_can_be_held_globally() {
    for chord in [
        "CmdOrCtrl+Shift+Space",
        "Cmd+K",
        "Ctrl+Space",
        "Alt+Space",
        "Shift+Alt+W",
    ] {
        let parsed = parse_hotkey_chord(chord).expect("parse failed");
        assert_eq!(check_global_chord(&parsed), Ok(()), "{chord}");
    }
}

#[test]
fn f13_to_f24_can_be_held_globally_on_their_own() {
    for chord in ["F13", "F20", "F24", "Shift+F13"] {
        let parsed = parse_hotkey_chord(chord).expect("parse failed");
        assert_eq!(check_global_chord(&parsed), Ok(()), "{chord}");
    }
}

#[test]
fn the_refusal_states_the_rule() {
    let message = GlobalChordError::NeedsModifier.to_string();
    assert!(message.contains("Cmd, Ctrl or Alt"), "{message}");
    assert!(message.contains("F13 to F24"), "{message}");
}

#[test]
fn a_wayland_session_is_recognised_from_either_variable() {
    assert!(is_wayland_session(Some("wayland"), None));
    assert!(is_wayland_session(Some("Wayland"), None));
    assert!(is_wayland_session(None, Some("wayland-0")));
    assert!(is_wayland_session(Some("x11"), Some("wayland-0")));
    assert!(!is_wayland_session(Some("x11"), None));
    assert!(!is_wayland_session(None, None));
    assert!(!is_wayland_session(Some("tty"), Some("  ")));
}

// src/lib/chord.ts reads chords through this table, so it has to hold every spelling the parser takes.
const HOTKEY_TOKENS: &str = include_str!("../../../src/lib/hotkey-tokens.json");
const PARSER_SOURCE: &str = include_str!("../src/hotkey/mod.rs");

fn hotkey_tokens() -> serde_json::Value {
    serde_json::from_str(HOTKEY_TOKENS).expect("the token table is json")
}

fn hotkey_chord_cases() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/hotkey-chords.json"
    );
    let text = std::fs::read_to_string(path).expect("fixture is readable");
    serde_json::from_str(&text).expect("fixture is json")
}

/// The string literals in `function`'s match arms ahead of its fallback arm.
fn match_arm_literals(function: &str) -> BTreeSet<String> {
    let start = PARSER_SOURCE
        .find(&format!("fn {function}("))
        .expect("the parser function is there");
    let body = &PARSER_SOURCE[start..];
    let arms = &body[body.find("match").expect("a match")..body.find("_ =>").expect("a fallback")];
    let mut literals = BTreeSet::new();
    let mut chars = arms.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut literal = String::new();
        loop {
            match chars.next().expect("the literal closes") {
                '"' => break,
                '\\' => literal.push(chars.next().expect("an escaped character")),
                other => literal.push(other),
            }
        }
        literals.insert(literal);
    }
    literals
}

/// Each spelling in a section of the token table, with the name it reads as.
fn table_spellings(tokens: &serde_json::Value, section: &str) -> Vec<(String, String)> {
    tokens[section]
        .as_object()
        .expect("the section is an object")
        .iter()
        .flat_map(|(name, spellings)| {
            spellings
                .as_array()
                .expect("spellings are a list")
                .iter()
                .map(move |spelling| {
                    let spelling = spelling.as_str().expect("a spelling is a string");
                    (spelling.to_string(), name.clone())
                })
        })
        .collect()
}

fn variant_name(value: impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .expect("a variant serializes")
        .as_str()
        .expect("a unit variant serializes as its name")
        .to_string()
}

#[test]
fn the_token_table_lists_every_spelling_the_parser_reads() {
    let tokens = hotkey_tokens();
    let modifiers: BTreeSet<String> = table_spellings(&tokens, "modifiers")
        .into_iter()
        .map(|(spelling, _)| spelling)
        .collect();
    assert_eq!(modifiers, match_arm_literals("parse_modifier_token"));
    let keys: BTreeSet<String> = table_spellings(&tokens, "keys")
        .into_iter()
        .map(|(spelling, _)| spelling)
        .collect();
    assert_eq!(keys, match_arm_literals("parse_key_token"));
}

#[test]
fn every_spelling_in_the_token_table_parses_as_its_name() {
    let tokens = hotkey_tokens();
    for (alias, name) in table_spellings(&tokens, "modifiers") {
        for spelling in [alias.clone(), alias.to_ascii_uppercase()] {
            let chord = parse_hotkey_chord(&format!("{spelling}+A")).expect("a modifier parses");
            let names: Vec<String> = chord.modifiers().iter().map(|m| variant_name(*m)).collect();
            assert_eq!(names, vec![name.clone()], "{spelling}");
        }
    }
    for (alias, name) in table_spellings(&tokens, "keys") {
        for spelling in [alias.clone(), alias.to_ascii_lowercase()] {
            let chord = parse_hotkey_chord(&format!("CmdOrCtrl+{spelling}")).expect("a key parses");
            assert_eq!(variant_name(chord.key()), name, "{spelling}");
        }
    }
}

#[test]
fn the_fixture_keys_and_refusals_are_the_parsers() {
    let cases = hotkey_chord_cases();
    for pair in cases["spelledKeys"]
        .as_array()
        .expect("spelledKeys is a list")
    {
        let token = pair[0].as_str().expect("a token");
        let name = pair[1].as_str().expect("a key name");
        let chord = parse_hotkey_chord(&format!("CmdOrCtrl+{token}")).expect("the key parses");
        assert_eq!(variant_name(chord.key()), name, "{token}");
    }
    for chord in cases["unparsed"].as_array().expect("unparsed is a list") {
        let chord = chord.as_str().expect("a chord");
        assert!(parse_hotkey_chord(chord).is_err(), "{chord}");
    }
}
