// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::keys::*;
use super::*;
use crate::event::ElementState;

fn presses(events: &[Replayed]) -> Vec<(Key, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            Replayed::Key(key) if key.state == ElementState::Pressed => {
                Some((key.logical_key.clone(), key.text.as_deref().map(str::to_owned)))
            },
            _ => None,
        })
        .collect()
}

fn character(text: &str) -> Key {
    Key::Character(SmolStr::new(text))
}

#[test]
fn altgr_drops_its_fake_ctrl_and_types_the_character_bare() {
    let typed = Typist::new().press(LCTRL).press(RALT).tap(Q, "@").release(LCTRL).release(RALT);
    let events = replay(&layouts::de_de(), typed.messages());
    assert_eq!(
        presses(&events),
        [(Key::Named(NamedKey::AltGraph), None), (character("@"), Some("@".to_owned())),],
        "{events:#?}"
    );
    let at = events
        .iter()
        .position(|e| matches!(e, Replayed::Key(k) if k.logical_key == character("@")))
        .unwrap();
    let mods_then = events[..at]
        .iter()
        .rev()
        .find_map(|e| match e {
            Replayed::Modifiers(m) => Some(*m),
            _ => None,
        })
        .unwrap();
    assert_eq!(mods_then, ModifiersState::empty(), "{events:#?}");
}

#[test]
fn a_dead_key_composes_with_the_next_key() {
    let typed = Typist::new()
        .press(DE_CIRCUMFLEX)
        .dead('^')
        .release(DE_CIRCUMFLEX)
        .tap(E, "ê")
        .press(DE_CIRCUMFLEX)
        .dead('^')
        .release(DE_CIRCUMFLEX)
        .tap(SPACE, "^");
    let events = replay(&layouts::de_de(), typed.messages());
    assert_eq!(presses(&events), [
        (Key::Dead(Some('^')), None),
        (character("ê"), Some("ê".to_owned())),
        (Key::Dead(Some('^')), None),
        (Key::Named(NamedKey::Space), Some("^".to_owned())),
    ]);
    let typed = Typist::new().press(QUOTE).dead('\'').release(QUOTE).tap(S, "'s");
    let events = replay(&layouts::en_us_intl(), typed.messages());
    assert_eq!(presses(&events), [
        (Key::Dead(Some('\'')), None),
        (character("s"), Some("'s".to_owned()))
    ]);
}

#[test]
fn an_alt_code_commits_its_character() {
    let mut typed = Typist::new().toggled(0x90).press(LALT);
    for digit in [NUMPAD0, NUMPAD2, NUMPAD3, NUMPAD3] {
        typed = typed.press(digit).release(digit);
    }
    let typed = typed.release(LALT).posted("é");
    let events = replay(&layouts::en_us(), typed.messages());
    let digits: Vec<_> = presses(&events).into_iter().skip(1).collect();
    assert_eq!(
        digits,
        [0x60, 0x62, 0x63, 0x63]
            .map(|vk| (Key::Unidentified(NativeKey::Windows(vk)), None))
            .to_vec()
    );
    assert_eq!(events.last(), Some(&Replayed::Ime(Ime::Commit("é".to_owned()))));
}

#[test]
fn an_input_methods_result_is_committed_once() {
    let converted = "日本語";
    let typed = Typist::new()
        .ime(WM_IME_STARTCOMPOSITION, 0, Composition::default())
        .press(PROCESS_N)
        .ime(WM_IME_COMPOSITION, GCS_COMPSTR | GCS_COMPATTR, Composition {
            composing: Some("に".into()),
            attrs: vec![0],
            cursor: 1,
            ..Composition::default()
        })
        .release(N)
        .press(PROCESS_SPACE)
        .ime(WM_IME_COMPOSITION, GCS_COMPSTR | GCS_COMPATTR, Composition {
            composing: Some(converted.into()),
            attrs: vec![1, 1, 1],
            cursor: 3,
            ..Composition::default()
        })
        .release(SPACE)
        .press(PROCESS_ENTER)
        .ime(WM_IME_COMPOSITION, GCS_RESULTSTR, Composition {
            result: Some(converted.into()),
            ..Composition::default()
        })
        .ime(WM_IME_ENDCOMPOSITION, 0, Composition {
            result: Some(converted.into()),
            ..Composition::default()
        })
        .release(ENTER);
    let events = replay(&layouts::ja_jp(), typed.messages());
    let ime: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Replayed::Ime(ime) => Some(ime.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ime, [
        Ime::Enabled,
        Ime::Preedit("に".into(), Some((3, 3))),
        Ime::Preedit(converted.into(), Some((0, converted.len()))),
        Ime::Preedit(String::new(), None),
        Ime::Commit(converted.into()),
        Ime::Disabled,
    ]);
    assert!(
        presses(&events)
            .iter()
            .all(|(key, text)| *key == Key::Named(NamedKey::Process) && text.is_none()),
        "{events:#?}"
    );
}

#[test]
fn the_hangul_ime_commits_once() {
    let typed = Typist::new()
        .ime(WM_IME_STARTCOMPOSITION, 0, Composition::default())
        .ime(WM_IME_COMPOSITION, GCS_COMPSTR, Composition {
            composing: Some("한".into()),
            cursor: 1,
            ..Composition::default()
        })
        .ime(WM_IME_ENDCOMPOSITION, 0, Composition {
            result: Some("한".into()),
            ..Composition::default()
        })
        .ime(WM_IME_COMPOSITION, GCS_RESULTSTR, Composition {
            result: Some("한".into()),
            ..Composition::default()
        });
    let events = replay(&layouts::ko_kr(), typed.messages());
    let commits = events.iter().filter(|e| matches!(e, Replayed::Ime(Ime::Commit(_)))).count();
    assert_eq!(commits, 1, "{events:#?}");
}

#[test]
fn a_packet_key_carries_its_text() {
    let typed = Typist::new().tap(PACKET, "😀");
    let events = replay(&layouts::en_us(), typed.messages());
    assert_eq!(presses(&events), [(
        Key::Unidentified(NativeKey::Windows(0xE7)),
        Some("😀".to_owned())
    )]);
}
