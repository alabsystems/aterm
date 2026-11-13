// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_window::keyboard::SmolStr;

fn ch(c: &str) -> WinitKey {
    WinitKey::Character(SmolStr::new(c))
}

#[test]
fn parse_basic_chord() {
    let c = Chord::parse("cmd+shift+t").unwrap();
    assert_eq!(c.mods, MOD_CMD | MOD_SHIFT);
    assert_eq!(c.key, KeyToken::Char('t'));
}

#[test]
fn chord_order_case_and_aliases_parse_equal() {
    for (a, b) in [
        ("cmd+shift+t", "Shift+CMD+T"),
        ("cmd+shift+t", "shift + cmd + t"),
        ("super+a", "cmd+a"),
        ("option+a", "alt+a"),
        ("control+a", "ctrl+a"),
    ] {
        assert_eq!(
            Chord::parse(a).unwrap(),
            Chord::parse(b).unwrap(),
            "{a} {b}"
        );
    }
}

#[test]
fn named_keys_parse() {
    assert_eq!(
        Chord::parse("ctrl+enter").unwrap().key,
        KeyToken::Named(NamedKey::Enter)
    );
    assert_eq!(
        Chord::parse("cmd+up").unwrap().key,
        KeyToken::Named(NamedKey::ArrowUp)
    );
    assert_eq!(
        Chord::parse("alt+f4").unwrap().key,
        KeyToken::Named(NamedKey::F4)
    );
    assert_eq!(
        Chord::parse("esc").unwrap().key,
        KeyToken::Named(NamedKey::Escape)
    );
}

#[test]
fn malformed_chords_rejected() {
    assert!(Chord::parse("").is_err());
    assert!(Chord::parse("cmd+").is_err());
    assert!(Chord::parse("cmd+nope+x").is_err());
    assert!(Chord::parse("cmd").is_err());
    assert!(Chord::parse("notakey").is_err());
    assert!(Chord::parse("a+b").is_err());
    assert!(Chord::parse("ctrl++").is_err());
    assert!(Chord::parse("ctrl+foo+x").is_err());
    assert_eq!(
        Chord::parse("ctrl+plus").map(|c| c.key),
        Ok(KeyToken::Char('+'))
    );
}

#[test]
fn action_names_parse() {
    assert_eq!(Action::parse("new_tab"), Some(Action::NewTab));
    assert_eq!(
        Action::parse("reopen_closed_tab"),
        Some(Action::ReopenClosedTab)
    );
    assert_eq!(
        Action::parse("split_horizontal"),
        Some(Action::SplitHorizontal)
    );
    assert_eq!(Action::parse("switch_tab_3"), Some(Action::SwitchTab(3)));
    assert_eq!(Action::parse("copy"), Some(Action::Copy));
    assert_eq!(
        Action::parse("toggle_matrix_rain"),
        Some(Action::ToggleMatrixRain)
    );
    assert_eq!(
        Action::parse("toggle_sparkle_words"),
        None,
        "the hidden runtime master must not contradict the two Settings toggles"
    );
    assert_eq!(Action::parse("unknown_action"), None);
    assert_eq!(Action::parse("switch_tab_0"), None);
    assert_eq!(Action::parse("switch_tab_99"), None);
}

#[test]
fn event_chord_matches_parsed() {
    let parsed = Chord::parse("cmd+t").unwrap();
    let live = Chord::from_event(&ch("t"), ModifiersState::SUPER).unwrap();
    assert_eq!(parsed, live);
    let parsed_shift = Chord::parse("cmd+shift+t").unwrap();
    let live_shift =
        Chord::from_event(&ch("T"), ModifiersState::SUPER | ModifiersState::SHIFT).unwrap();
    assert_eq!(parsed_shift, live_shift);
}

#[test]
fn bare_modifier_event_is_none() {
    assert!(Chord::from_event(&WinitKey::Named(NamedKey::Shift), ModifiersState::SHIFT).is_none());
}

#[test]
fn config_table_resolves_and_misses() {
    let mut table = std::collections::BTreeMap::new();
    table.insert("cmd+shift+n".to_string(), "new_tab".to_string());
    table.insert("ctrl+a".to_string(), "find".to_string());
    let kb = Keybindings::from_config(Some(&table));
    assert!(!kb.is_empty());
    assert_eq!(
        kb.lookup(&ch("n"), ModifiersState::SUPER | ModifiersState::SHIFT),
        Some(Action::NewTab)
    );
    assert_eq!(
        kb.lookup(&ch("a"), ModifiersState::CONTROL),
        Some(Action::Find)
    );
    assert_eq!(kb.lookup(&ch("t"), ModifiersState::SUPER), None);
}

#[test]
fn bad_entries_skipped_rest_kept() {
    let mut table = std::collections::BTreeMap::new();
    table.insert("cmd+t".to_string(), "new_tab".to_string());
    table.insert("garbage++".to_string(), "find".to_string());
    table.insert("cmd+x".to_string(), "no_such_action".to_string());
    let kb = Keybindings::from_config(Some(&table));
    assert_eq!(
        kb.lookup(&ch("t"), ModifiersState::SUPER),
        Some(Action::NewTab)
    );
    assert_eq!(kb.lookup(&ch("x"), ModifiersState::SUPER), None);
    assert_eq!(kb.map.len(), 1);
}

#[test]
fn platform_defaults_seed_terminal_chords() {
    let kb = Keybindings::resolved(None);
    #[cfg(target_os = "macos")]
    assert!(
        kb.is_empty(),
        "macOS keeps an empty default keybinding table"
    );
    #[cfg(not(target_os = "macos"))]
    {
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            kb.lookup(&ch("c"), cs),
            Some(Action::Copy),
            "Ctrl+Shift+C copies"
        );
        assert_eq!(
            kb.lookup(&ch("v"), cs),
            Some(Action::Paste),
            "Ctrl+Shift+V pastes"
        );
        assert_eq!(
            kb.lookup(&ch("t"), cs),
            Some(Action::NewTab),
            "Ctrl+Shift+T new tab"
        );
        assert_eq!(
            kb.lookup(&ch("s"), cs),
            Some(Action::ToggleSettings),
            "Ctrl+Shift+S opens Settings"
        );
        assert_eq!(
            kb.lookup(&ch("a"), cs),
            Some(Action::SelectAll),
            "Ctrl+Shift+A selects all"
        );
        assert_eq!(
            kb.lookup(&WinitKey::Named(NamedKey::F11), ModifiersState::empty()),
            Some(Action::ToggleFullscreen),
            "F11 toggles full-screen"
        );
        assert_eq!(
            kb.lookup(&ch(","), ModifiersState::CONTROL),
            Some(Action::ToggleSettings),
            "Ctrl+, opens Settings",
        );
        assert_eq!(
            kb.lookup(&ch("v"), ModifiersState::CONTROL),
            if cfg!(windows) {
                Some(Action::Paste)
            } else {
                None
            },
            "plain Ctrl+V is a Windows-only paste seed"
        );
        assert_eq!(kb.lookup(&ch("c"), ModifiersState::CONTROL), None);
        let mut table = std::collections::BTreeMap::new();
        table.insert("ctrl+shift+c".to_string(), "find".to_string());
        let kb2 = Keybindings::resolved(Some(&table));
        assert_eq!(
            kb2.lookup(&ch("c"), cs),
            Some(Action::Find),
            "user [keybindings] overlay must win over the platform default"
        );
    }
}

#[test]
fn plus_is_bindable_and_seeded_for_zoom_in() {
    assert_eq!(
        Chord::parse("ctrl+plus").unwrap(),
        Chord {
            mods: MOD_CTRL,
            key: KeyToken::Char('+')
        }
    );
    assert_eq!(Chord::parse("ctrl+minus"), Chord::parse("ctrl+-"));
    assert_eq!(Chord::parse("ctrl+equal"), Chord::parse("ctrl+="));
    assert!(Chord::parse("ctrl++").is_err());

    #[cfg(not(target_os = "macos"))]
    {
        let kb = Keybindings::resolved(None);
        assert_eq!(
            kb.lookup(&ch("+"), ModifiersState::CONTROL),
            Some(Action::FontIncrease),
            "Ctrl+Plus zooms in"
        );
        assert_eq!(
            kb.lookup(&ch("="), ModifiersState::CONTROL | ModifiersState::SHIFT),
            Some(Action::FontIncrease),
            "Ctrl+Shift+= still zooms in"
        );
    }
}

/// WINDOWS LEAVES PSREADLINE ITS KEYS: the defaults bind no Shift selection key off
/// macOS, and on Windows no Ctrl+Shift+arrow or Enter.
#[cfg(not(target_os = "macos"))]
#[test]
fn windows_defaults_leave_psreadline_its_selection_and_edit_chords() {
    let kb = Keybindings::resolved(None);
    let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let named = WinitKey::Named;
    for key in [NamedKey::ArrowLeft, NamedKey::ArrowRight, NamedKey::Enter] {
        let bound = kb.lookup(&named(key), cs);
        if cfg!(windows) {
            assert_eq!(bound, None, "Ctrl+Shift+{key:?} belongs to PSReadLine");
        } else {
            assert!(
                bound.is_some(),
                "Ctrl+Shift+{key:?} keeps its seed off Windows"
            );
        }
    }
    for key in [
        NamedKey::ArrowLeft,
        NamedKey::ArrowRight,
        NamedKey::Home,
        NamedKey::End,
    ] {
        assert_eq!(
            kb.lookup(&named(key), ModifiersState::SHIFT),
            None,
            "Shift+{key:?} selects in PSReadLine"
        );
    }
    assert_eq!(
        kb.lookup(&ch("z"), cs),
        cfg!(windows).then_some(Action::TogglePaneZoom),
        "WezTerm's zoom chord, on Windows"
    );
    assert_eq!(
        kb.lookup(&named(NamedKey::PageDown), ModifiersState::CONTROL),
        Some(Action::NextTab)
    );
    assert_eq!(
        kb.lookup(&named(NamedKey::PageUp), ModifiersState::CONTROL),
        Some(Action::PrevTab)
    );
}

/// Off macOS, pane focus (Alt+arrows) and Ctrl+Tab / Ctrl+Shift+Tab are seeded.
#[cfg(not(target_os = "macos"))]
#[test]
fn platform_defaults_seed_pane_focus_and_tab_nav() {
    let kb = Keybindings::resolved(None);
    let named = WinitKey::Named;
    let alt = ModifiersState::ALT;
    assert_eq!(
        kb.lookup(&named(NamedKey::ArrowLeft), alt),
        Some(Action::FocusPaneLeft)
    );
    assert_eq!(
        kb.lookup(&named(NamedKey::ArrowRight), alt),
        Some(Action::FocusPaneRight)
    );
    assert_eq!(
        kb.lookup(&named(NamedKey::ArrowUp), alt),
        Some(Action::FocusPaneUp)
    );
    assert_eq!(
        kb.lookup(&named(NamedKey::ArrowDown), alt),
        Some(Action::FocusPaneDown)
    );
    assert_eq!(
        kb.lookup(&named(NamedKey::Tab), ModifiersState::CONTROL),
        Some(Action::NextTab),
        "Ctrl+Tab"
    );
    assert_eq!(
        kb.lookup(
            &named(NamedKey::Tab),
            ModifiersState::CONTROL | ModifiersState::SHIFT
        ),
        Some(Action::PrevTab),
        "Ctrl+Shift+Tab"
    );
    let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
    for n in 1..=9u8 {
        assert_eq!(
            kb.lookup(&ch(&n.to_string()), ctrl_alt),
            None,
            "Ctrl+Alt+{n} must stay free — it is AltGr+{n} on de-DE"
        );
        assert_eq!(kb.lookup(&ch(&n.to_string()), ModifiersState::ALT), None);
    }
    let mut table = std::collections::BTreeMap::new();
    table.insert("ctrl+alt+2".to_string(), "switch_tab_2".to_string());
    assert_eq!(
        Keybindings::resolved(Some(&table)).lookup(&ch("2"), ctrl_alt),
        Some(Action::SwitchTab(2)),
        "opt-in still works"
    );
}

#[test]
fn fullscreen_is_a_bindable_action() {
    assert_eq!(Action::parse("fullscreen"), Some(Action::ToggleFullscreen));
    assert_eq!(
        Action::parse("toggle_fullscreen"),
        Some(Action::ToggleFullscreen)
    );
    let mut table = std::collections::BTreeMap::new();
    table.insert("ctrl+shift+f11".to_string(), "fullscreen".to_string());
    let (kb, warns) = Keybindings::from_config_warn(Some(&table));
    assert!(warns.is_empty(), "no dropped-rule warning: {warns:?}");
    assert_eq!(
        kb.lookup(
            &WinitKey::Named(NamedKey::F11),
            ModifiersState::CONTROL | ModifiersState::SHIFT
        ),
        Some(Action::ToggleFullscreen),
    );
}

#[test]
fn find_again_is_bindable_but_unseeded() {
    assert_eq!(Action::parse("find_next"), Some(Action::FindNext));
    assert_eq!(Action::parse("find_prev"), Some(Action::FindPrev));
    assert_eq!(Action::parse("find_previous"), Some(Action::FindPrev));
    let kb = Keybindings::resolved(None);
    for f in [NamedKey::F3, NamedKey::F1, NamedKey::F12] {
        assert_eq!(
            kb.lookup(&WinitKey::Named(f), ModifiersState::empty()),
            None,
            "{f:?} stays the program's on the other side of the PTY",
        );
    }
    let mut table = std::collections::BTreeMap::new();
    table.insert("f3".to_string(), "find_next".to_string());
    table.insert("shift+f3".to_string(), "find_prev".to_string());
    let kb = Keybindings::resolved(Some(&table));
    assert_eq!(
        kb.lookup(&WinitKey::Named(NamedKey::F3), ModifiersState::empty()),
        Some(Action::FindNext),
        "the Windows reflex is one config line away",
    );
    assert_eq!(
        kb.lookup(&WinitKey::Named(NamedKey::F3), ModifiersState::SHIFT),
        Some(Action::FindPrev),
    );
}

#[test]
fn unbind_value_masks_platform_seed() {
    let mut table = std::collections::BTreeMap::new();
    table.insert("ctrl+tab".to_string(), "none".to_string());
    table.insert("f11".to_string(), "unbind".to_string());
    table.insert("ctrl+alt+f9".to_string(), "none".to_string());
    let (kb, warns) = Keybindings::resolved_warn(Some(&table));
    assert!(
        warns.is_empty(),
        "unbind is not an unknown action: {warns:?}"
    );
    assert_eq!(
        kb.lookup(&WinitKey::Named(NamedKey::Tab), ModifiersState::CONTROL),
        None,
        "ctrl+tab seed is masked"
    );
    assert_eq!(
        kb.lookup(&WinitKey::Named(NamedKey::F11), ModifiersState::empty()),
        None,
        "f11 seed is masked (both spellings work)"
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        kb.lookup(&ch("c"), ModifiersState::CONTROL | ModifiersState::SHIFT),
        Some(Action::Copy),
        "the other seeds survive an unbind"
    );
    let (kb_cfg, warns_cfg) = Keybindings::from_config_warn(Some(&table));
    assert!(warns_cfg.is_empty());
    assert!(kb_cfg.is_empty());
    assert!(!chord_in_keybindings("ctrl+tab", &table));
}

/// Hints follow presentation order, honor a user rebind or unbind, are `None`
/// for an unbound action, and round-trip through `Chord::parse`.
#[cfg(not(target_os = "macos"))]
#[test]
fn display_chord_for_resolves_effective_hints() {
    let kb = Keybindings::resolved(None);
    assert_eq!(
        kb.display_chord_for(Action::Copy).as_deref(),
        Some("Ctrl+Shift+C")
    );
    assert_eq!(
        kb.display_chord_for(Action::ToggleFullscreen).as_deref(),
        Some("F11")
    );
    assert_eq!(
        kb.display_chord_for(Action::NextTab).as_deref(),
        Some(if cfg!(windows) {
            "Ctrl+Tab"
        } else {
            "Ctrl+Shift+Right"
        }),
        "presentation order picks the FIRST seeded spelling, not an alias"
    );
    assert_eq!(
        kb.display_chord_for(Action::ToggleMatrixRain),
        None,
        "an unbound action shows no hint"
    );
    let mut t = std::collections::BTreeMap::new();
    t.insert("ctrl+shift+c".to_string(), "none".to_string());
    t.insert("ctrl+insert".to_string(), "none".to_string());
    t.insert("ctrl+shift+y".to_string(), "copy".to_string());
    let kb2 = Keybindings::resolved(Some(&t));
    assert_eq!(
        kb2.display_chord_for(Action::Copy).as_deref(),
        Some("Ctrl+Shift+Y")
    );
    let mut t2 = std::collections::BTreeMap::new();
    t2.insert("ctrl+shift+c".to_string(), "none".to_string());
    let kb3 = Keybindings::resolved(Some(&t2));
    assert_eq!(
        kb3.display_chord_for(Action::Copy).as_deref(),
        Some("Ctrl+Insert")
    );
    for (chord_str, _) in Keybindings::PLATFORM_DEFAULT_PAIRS {
        let chord = Chord::parse(chord_str).unwrap();
        assert_eq!(
            Chord::parse(&chord.display()),
            Ok(chord.clone()),
            "display of {chord_str:?} must round-trip"
        );
    }
}

#[test]
fn display_round_trips_non_ascii_chord_keys() {
    for spelling in ["ctrl+ß", "ctrl+é", "ctrl+shift+a", "alt+µ"] {
        let chord = Chord::parse(spelling).unwrap();
        assert_eq!(
            Chord::parse(&chord.display()),
            Ok(chord.clone()),
            "display of {spelling:?} must round-trip"
        );
    }
    assert_eq!(Chord::parse("ctrl+ß").unwrap().display(), "Ctrl+ß");
    assert_eq!(Chord::parse("ctrl+a").unwrap().display(), "Ctrl+A");
}

#[test]
fn key_sequence_parses_escapes() {
    assert_eq!(parse_byte_sequence("\\n").unwrap(), vec![0x0a]);
    assert_eq!(
        parse_byte_sequence("\\e[13;2u").unwrap(),
        b"\x1b[13;2u".to_vec()
    );
    assert_eq!(parse_byte_sequence("\\x1b\\x0d").unwrap(), vec![0x1b, 0x0d]);
    assert_eq!(
        parse_byte_sequence("\\u{1b}OP").unwrap(),
        b"\x1bOP".to_vec()
    );
    assert_eq!(parse_byte_sequence("\n").unwrap(), vec![0x0a]);
    assert_eq!(parse_byte_sequence("ab").unwrap(), b"ab".to_vec());
}

#[test]
fn key_sequence_rejects_bad_escapes() {
    assert!(parse_byte_sequence("\\q").is_err());
    assert!(parse_byte_sequence("\\x1").is_err());
    assert!(parse_byte_sequence("\\xzz").is_err());
    assert!(parse_byte_sequence("\\u1b").is_err());
    assert!(parse_byte_sequence("\\").is_err());
}

#[test]
fn key_sequences_from_config_and_lookup() {
    let mut table = std::collections::BTreeMap::new();
    table.insert("shift+enter".to_string(), "\\n".to_string());
    table.insert("f5".to_string(), "\\e[15~".to_string());
    table.insert("garbage++".to_string(), "x".to_string());
    let ks = KeySequences::from_config(Some(&table));
    assert!(!ks.is_empty());
    assert_eq!(
        ks.lookup(&WinitKey::Named(NamedKey::Enter), ModifiersState::SHIFT),
        Some(&[0x0a][..]),
        "shift+enter -> LF"
    );
    assert_eq!(
        ks.lookup(&WinitKey::Named(NamedKey::F5), ModifiersState::empty()),
        Some(&b"\x1b[15~"[..]),
        "f5 -> ESC[15~"
    );
    assert_eq!(
        ks.lookup(&WinitKey::Named(NamedKey::Enter), ModifiersState::empty()),
        None
    );
    assert_eq!(ks.map.len(), 2, "the bad chord was skipped");
}

#[test]
fn key_sequence_oversized_value_rejected() {
    let at_cap = "a".repeat(MAX_KEY_SEQUENCE_BYTES);
    assert_eq!(
        parse_byte_sequence(&at_cap).unwrap().len(),
        MAX_KEY_SEQUENCE_BYTES
    );
    let too_big = "a".repeat(MAX_KEY_SEQUENCE_BYTES + 1);
    assert!(parse_byte_sequence(&too_big).is_err());
}

#[test]
fn key_sequence_unicode_brace_edge_cases() {
    assert!(parse_byte_sequence("\\u{}").is_err());
    assert!(parse_byte_sequence("\\u{+41}").is_err());
    assert!(parse_byte_sequence("\\u{D800}").is_err());
    assert!(parse_byte_sequence("\\u{110000}").is_err());
    assert_eq!(
        parse_byte_sequence("\\u{1F600}").unwrap(),
        "\u{1F600}".as_bytes().to_vec()
    );
}

#[test]
fn resolve_chord_precedence() {
    use std::collections::BTreeMap;
    let mut kbt = BTreeMap::new();
    kbt.insert("cmd+t".to_string(), "new_tab".to_string());
    let mut kst = BTreeMap::new();
    kst.insert("cmd+t".to_string(), "X".to_string());
    let kb = Keybindings::from_config(Some(&kbt));
    let ks = KeySequences::from_config(Some(&kst));
    let not_asked =
        |_: crate::pane::FocusDir| -> bool { panic!("asked only for a focus_pane_* binding") };
    assert_eq!(
        resolve_chord(
            &ch("t"),
            NO_PHYSICAL_KEY,
            ModifiersState::SUPER,
            &kb,
            &ks,
            not_asked
        ),
        ChordResolution::Action(Action::NewTab)
    );
    let kb_empty = Keybindings::from_config(None);
    assert_eq!(
        resolve_chord(
            &ch("t"),
            NO_PHYSICAL_KEY,
            ModifiersState::SUPER,
            &kb_empty,
            &ks,
            not_asked
        ),
        ChordResolution::Sequence(b"X".to_vec())
    );
    let ks_empty = KeySequences::from_config(None);
    assert_eq!(
        resolve_chord(
            &ch("z"),
            NO_PHYSICAL_KEY,
            ModifiersState::SUPER,
            &kb_empty,
            &ks_empty,
            not_asked
        ),
        ChordResolution::FallThrough
    );
}

#[test]
fn a_layout_letter_matches_its_us_keys_binding_only_in_a_chord() {
    use aterm_window::keyboard::KeyCode;
    use std::collections::BTreeMap;
    let key = PhysicalKey::Code;
    let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let table: BTreeMap<String, String> = [
        ("ctrl+shift+c", "copy"),
        ("ctrl+shift+t", "new_tab"),
        ("ctrl+shift+w", "close_tab"),
        ("c", "copy"),
        ("shift+c", "copy"),
        ("ctrl+2", "font_reset"),
    ]
    .into_iter()
    .map(|(c, a)| (c.to_string(), a.to_string()))
    .collect();
    let kb = Keybindings::from_config(Some(&table));
    let ks = KeySequences::from_config(None);
    let not_asked =
        |_: crate::pane::FocusDir| -> bool { panic!("asked only for a focus_pane_* binding") };
    assert_eq!(
        kb.action_for(&ch("с"), key(KeyCode::KeyC), ctrl_shift),
        Some(Action::Copy)
    );
    assert_eq!(
        kb.action_for(&ch("е"), key(KeyCode::KeyT), ctrl_shift),
        Some(Action::NewTab)
    );
    assert_eq!(
        resolve_chord(
            &ch("ц"),
            key(KeyCode::KeyW),
            ctrl_shift,
            &kb,
            &ks,
            not_asked
        ),
        ChordResolution::Action(Action::CloseTab)
    );
    assert_eq!(
        kb.action_for(&ch("с"), key(KeyCode::KeyC), ModifiersState::empty()),
        None
    );
    assert_eq!(
        kb.action_for(&ch("с"), key(KeyCode::KeyC), ModifiersState::SHIFT),
        None
    );
    assert_eq!(
        kb.action_for(&ch("é"), key(KeyCode::Digit2), ModifiersState::CONTROL),
        None
    );
    let mut own = table.clone();
    own.insert("ctrl+shift+с".to_string(), "find".to_string());
    let kb_own = Keybindings::from_config(Some(&own));
    assert_eq!(
        kb_own.action_for(&ch("с"), key(KeyCode::KeyC), ctrl_shift),
        Some(Action::Find)
    );
    let ks_own = KeySequences::from_config(Some(
        &[("ctrl+shift+с".to_string(), "X".to_string())]
            .into_iter()
            .collect(),
    ));
    assert_eq!(
        resolve_chord(
            &ch("с"),
            key(KeyCode::KeyC),
            ctrl_shift,
            &kb,
            &ks_own,
            not_asked
        ),
        ChordResolution::Sequence(b"X".to_vec())
    );
}

/// Shift+Insert pastes PRIMARY on Linux (xterm, VTE, kitty, foot) and
/// Ctrl+Shift+Insert the clipboard (VTE); elsewhere, with no PRIMARY,
/// Shift+Insert is the clipboard paste (Windows Terminal).
#[cfg(not(target_os = "macos"))]
#[test]
fn shift_insert_pastes_the_platforms_selection() {
    let kb = Keybindings::platform_defaults();
    let insert = WinitKey::Named(NamedKey::Insert);
    let shift = ModifiersState::SHIFT;
    let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
    if cfg!(target_os = "linux") {
        assert_eq!(kb.lookup(&insert, shift), Some(Action::PasteSelection));
        assert_eq!(kb.lookup(&insert, ctrl_shift), Some(Action::Paste));
    } else {
        assert_eq!(kb.lookup(&insert, shift), Some(Action::Paste));
    }
    assert_eq!(Action::parse("paste_primary"), Some(Action::PasteSelection));
    assert!(Action::Paste.is_paste() && Action::PasteSelection.is_paste());
    assert!(!Action::Copy.is_paste());
}

#[test]
fn a_pane_focus_binding_claims_its_chord_only_when_focus_can_move() {
    use crate::pane::FocusDir;
    use std::collections::BTreeMap;
    let alt = ModifiersState::ALT;
    let left = WinitKey::Named(NamedKey::ArrowLeft);
    let up = WinitKey::Named(NamedKey::ArrowUp);
    let mut kbt = BTreeMap::new();
    kbt.insert("alt+left".to_string(), "focus_pane_left".to_string());
    kbt.insert("alt+up".to_string(), "focus_pane_up".to_string());
    let kb = Keybindings::from_config(Some(&kbt));
    let ks_empty = KeySequences::from_config(None);
    let mut kst = BTreeMap::new();
    kst.insert("alt+left".to_string(), "\\x1bb".to_string());
    let ks = KeySequences::from_config(Some(&kst));

    assert_eq!(
        resolve_chord(&left, NO_PHYSICAL_KEY, alt, &kb, &ks, |dir| dir
            == FocusDir::Left),
        ChordResolution::Action(Action::FocusPaneLeft)
    );
    assert_eq!(
        resolve_chord(&up, NO_PHYSICAL_KEY, alt, &kb, &ks_empty, |dir| dir
            == FocusDir::Up),
        ChordResolution::Action(Action::FocusPaneUp)
    );
    assert_eq!(
        resolve_chord(&left, NO_PHYSICAL_KEY, alt, &kb, &ks_empty, |_| false),
        ChordResolution::FallThrough
    );
    assert_eq!(
        resolve_chord(&up, NO_PHYSICAL_KEY, alt, &kb, &ks_empty, |dir| dir
            != FocusDir::Up),
        ChordResolution::FallThrough,
        "the question names the action's own direction"
    );
    assert_eq!(
        resolve_chord(&left, NO_PHYSICAL_KEY, alt, &kb, &ks, |_| false),
        ChordResolution::Sequence(b"\x1bb".to_vec())
    );
}

#[test]
fn a_pane_focus_binding_off_the_arrows_always_claims_its_chord() {
    use std::collections::BTreeMap;
    let ctrl = ModifiersState::CONTROL;
    let mut kbt = BTreeMap::new();
    for (chord, action) in [
        ("ctrl+h", "focus_pane_left"),
        ("ctrl+j", "focus_pane_down"),
        ("ctrl+k", "focus_pane_up"),
        ("ctrl+l", "focus_pane_right"),
        ("ctrl+shift+left", "focus_pane_left"),
    ] {
        kbt.insert(chord.to_string(), action.to_string());
    }
    let kb = Keybindings::from_config(Some(&kbt));
    let ks = KeySequences::from_config(None);
    let not_asked = |_: crate::pane::FocusDir| -> bool { panic!("asked only for an arrow chord") };
    for (letter, action) in [
        ("h", Action::FocusPaneLeft),
        ("j", Action::FocusPaneDown),
        ("k", Action::FocusPaneUp),
        ("l", Action::FocusPaneRight),
    ] {
        assert_eq!(
            resolve_chord(&ch(letter), NO_PHYSICAL_KEY, ctrl, &kb, &ks, not_asked),
            ChordResolution::Action(action),
            "ctrl+{letter}"
        );
    }
    assert_eq!(
        resolve_chord(
            &WinitKey::Named(NamedKey::ArrowLeft),
            NO_PHYSICAL_KEY,
            ctrl | ModifiersState::SHIFT,
            &kb,
            &ks,
            |_| false
        ),
        ChordResolution::FallThrough,
        "an arrow with any modifiers passes through"
    );
}

#[test]
fn pane_focus_dir_names_the_four_focus_actions() {
    use crate::pane::FocusDir;
    assert_eq!(Action::FocusPaneLeft.pane_focus_dir(), Some(FocusDir::Left));
    assert_eq!(
        Action::FocusPaneRight.pane_focus_dir(),
        Some(FocusDir::Right)
    );
    assert_eq!(Action::FocusPaneUp.pane_focus_dir(), Some(FocusDir::Up));
    assert_eq!(Action::FocusPaneDown.pane_focus_dir(), Some(FocusDir::Down));
    for name in ACTION_NAMES.iter().filter(|n| !n.contains("..")) {
        let action = Action::parse(name).expect("every listed name parses");
        assert_eq!(
            action.pane_focus_dir().is_some(),
            name.starts_with("focus_pane_"),
            "{name}"
        );
    }
}

#[test]
fn builtin_shadow_label_maps_known_and_normalizes() {
    let known = |chord: &str, menu_bar: bool| builtin_shadow_label_when(chord, true, menu_bar);
    for chord in [
        "cmd+c",
        "cmd+shift+]",
        "shift+cmd+]",
        "cmd+1",
        "cmd+shift+t",
        "cmd+q",
    ] {
        assert!(known(chord, false).is_some(), "{chord}");
    }
    assert_eq!(known("cmd+shift+]", false), known("shift+cmd+]", false));
    assert_eq!(known("cmd+k", false), None);
    assert_eq!(known("garbage++", false), None);
    assert!(known("cmd+shift+p", true).is_some());
    assert_eq!(known("cmd+shift+p", false), None);
    assert_eq!(
        builtin_shadow_label("cmd+shift+p").is_some(),
        cfg!(target_os = "macos")
    );
}

#[test]
fn builtin_shadow_label_is_silent_when_the_suite_is_gated_off() {
    assert_eq!(builtin_shadow_label_when("cmd+c", false, true), None);
    assert_eq!(builtin_shadow_label_when("super+t", false, true), None);
    assert_eq!(
        builtin_shadow_label("cmd+c").is_some(),
        crate::app_input::HARDCODED_SUPER_CHORDS
    );
}

#[test]
fn chord_in_keybindings_detects_cross_table() {
    use std::collections::BTreeMap;
    let mut kb = BTreeMap::new();
    kb.insert("cmd+shift+]".to_string(), "next_tab".to_string());
    assert!(chord_in_keybindings("shift+cmd+]", &kb));
    assert!(!chord_in_keybindings("cmd+x", &kb));
}

#[test]
fn key_sequences_from_config_warn_returns_dropped_rules() {
    use std::collections::BTreeMap;
    let mut t = BTreeMap::new();
    t.insert("ctrl+a".to_string(), "\\n".to_string());
    t.insert("garbage++".to_string(), "x".to_string());
    t.insert("ctrl+b".to_string(), "\\q".to_string());
    t.insert("ctrl+c".to_string(), String::new());
    t.insert("ctrl+d".to_string(), "a".repeat(MAX_KEY_SEQUENCE_BYTES + 1));
    let (ks, warns) = KeySequences::from_config_warn(Some(&t));
    assert_eq!(ks.map.len(), 1, "only the good rule survives");
    assert_eq!(warns.len(), 4, "one warning per dropped rule: {warns:?}");
}

#[test]
fn keybindings_from_config_warn_reports_unknown_action_and_bad_chord() {
    use std::collections::BTreeMap;
    let mut t = BTreeMap::new();
    t.insert("cmd+t".to_string(), "new_tab".to_string());
    t.insert("garbage++".to_string(), "copy".to_string());
    t.insert("cmd+k".to_string(), "no_such".to_string());
    let (_kb, warns) = Keybindings::from_config_warn(Some(&t));
    assert_eq!(warns.len(), 2, "{warns:?}");
    let (_kb2, warns2) = Keybindings::resolved_warn(Some(&t));
    assert_eq!(warns2.len(), 2);
}
