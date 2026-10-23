// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::find_field_key_tests::{character, named, type_str};
use crate::{App, WindowId};
use aterm_window::keyboard::{ModifiersState, NamedKey};

fn app_editing() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    assert!(app.begin_active_session_rename(wid));
    (app, wid)
}

fn field(app: &App, wid: WindowId) -> Option<(String, usize)> {
    app.inline_rename_edit(wid)
        .map(|edit| (edit.text.clone(), edit.cursor))
}

fn pin(app: &App, session: u64) -> Option<String> {
    app.pool
        .get(session)?
        .ctx
        .meta
        .lock()
        .unwrap()
        .user_title
        .clone()
}

#[test]
fn typing_edits_the_field_and_return_commits_the_pin() {
    let (mut app, wid) = app_editing();
    type_str(&mut app, wid, "build agent");
    assert_eq!(field(&app, wid), Some(("build agent".to_string(), 11)));

    app.on_key(wid, named(NamedKey::Enter));
    assert_eq!(pin(&app, 0).as_deref(), Some("build agent"));
    assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
}

#[test]
fn escape_cancels_and_types_nothing() {
    let (mut app, wid) = app_editing();
    type_str(&mut app, wid, "discard");
    app.on_key(wid, named(NamedKey::Escape));
    assert_eq!(app.rename_edit_session(wid), None, "⎋ closed the editor");
    assert_eq!(pin(&app, 0), None, "and wrote nothing");

    assert!(app.begin_active_session_rename(wid));
    assert_eq!(field(&app, wid), Some((String::new(), 0)));
}

#[test]
fn tab_commits_rather_than_typing_a_tab() {
    let (mut app, wid) = app_editing();
    type_str(&mut app, wid, "ci");
    app.on_key(wid, named(NamedKey::Tab));
    assert_eq!(pin(&app, 0).as_deref(), Some("ci"));
}

#[test]
fn the_field_speaks_the_shared_single_line_keymap() {
    let (mut app, wid) = app_editing();
    type_str(&mut app, wid, "release");
    app.on_key(wid, named(NamedKey::Backspace));
    assert_eq!(field(&app, wid), Some(("releas".to_string(), 6)));

    let ctrl = |app: &mut App, ch: &str| {
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL;
        app.on_key(wid, character(ch));
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
    };
    ctrl(&mut app, "a");
    assert_eq!(field(&app, wid), Some(("releas".to_string(), 0)), "^A");
    app.on_key(wid, named(NamedKey::ArrowRight));
    ctrl(&mut app, "k");
    assert_eq!(field(&app, wid), Some(("r".to_string(), 1)), "^K");
    ctrl(&mut app, "e");
    assert_eq!(field(&app, wid), Some(("r".to_string(), 1)), "^E");
}

#[test]
fn a_bare_shift_does_not_end_the_edit() {
    let (mut app, wid) = app_editing();
    type_str(&mut app, wid, "a");
    app.on_key(wid, named(NamedKey::Shift));
    assert_eq!(
        field(&app, wid),
        Some(("a".to_string(), 1)),
        "the field is untouched and still open"
    );
}

#[test]
fn an_unowned_key_settles_the_edit_and_does_not_swallow_itself() {
    let (mut app, wid) = app_editing();
    type_str(&mut app, wid, "kept");
    let mods = ModifiersState::empty();
    let f5 = named(NamedKey::F5);
    assert!(
        !app.on_key_rename_mode(wid, mods, &f5),
        "the gate declines the key so its real handler still runs"
    );
    assert_eq!(app.rename_edit_session(wid), None, "the edit is settled");
    assert_eq!(
        pin(&app, 0).as_deref(),
        Some("kept"),
        "keeping what you typed"
    );
}

#[test]
fn a_bound_paste_chord_pastes_into_the_field_instead_of_the_pty() {
    let (mut app, wid) = app_editing();
    let mut table = std::collections::BTreeMap::new();
    table.insert("ctrl+v".to_string(), "paste".to_string());
    table.insert("ctrl+t".to_string(), "new_tab".to_string());
    app.keybindings = crate::keybinding::Keybindings::resolved(Some(&table));
    type_str(&mut app, wid, "half");

    assert!(
        app.on_key_rename_mode(wid, ModifiersState::CONTROL, &character("v")),
        "the field claims the bound paste chord, so on_key returns before its \
         keybinding block can deliver Paste to the PTY"
    );
    assert!(
        app.rename_edit_session(wid).is_some(),
        "a paste is an edit, not an exit — the field is still open"
    );
    assert_eq!(pin(&app, 0), None, "and nothing was committed");

    assert!(
        !app.on_key_rename_mode(wid, ModifiersState::CONTROL, &character("t")),
        "ctrl+t is New Tab, not a field edit"
    );
    assert!(app.rename_edit_session(wid).is_none(), "the edit settled");
    assert!(pin(&app, 0).is_some(), "keeping what was typed");
}

#[test]
fn a_key_sequence_chord_settles_the_edit_before_its_bytes_are_sent() {
    let (mut app, wid) = app_editing();
    app.key_sequences = crate::keybinding::KeySequences::from_config(Some(
        &[("ctrl+g".to_string(), "\\e[99~".to_string())]
            .into_iter()
            .collect(),
    ));
    type_str(&mut app, wid, "kept");

    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL;
    let chord = character("g");
    let physical = chord.physical_key;
    app.on_key(wid, chord);

    assert_eq!(app.rename_edit_session(wid), None, "the edit settled");
    assert_eq!(
        pin(&app, 0).as_deref(),
        Some("kept"),
        "keeping what was typed"
    );
    assert!(
        matches!(
            app.physical_press_owners.get(&physical),
            Some(crate::PhysicalPressOwner::Literal {
                event: crate::input::InputEvent::KeySequence(bytes),
                ..
            }) if bytes.as_slice() == b"\x1b[99~"
        ),
        "the chord's bytes went to the shell, after the field closed: {:?}",
        app.physical_press_owners.get(&physical)
    );
}

#[test]
fn a_held_key_repeats_against_the_renamed_session_only() {
    let (mut app, wid) = app_editing();
    type_str(&mut app, wid, "abc");
    let mods = ModifiersState::empty();
    let backspace = named(NamedKey::Backspace);
    let (session, edit) = app
        .rename_repeat_action(wid, mods, &backspace)
        .expect("⌫ is a repeatable field edit");
    assert_eq!(session, 0);

    assert!(app.apply_local_repeat_action(
        wid,
        crate::LocalRepeatAction::Rename {
            session,
            edit: edit.clone()
        }
    ));
    assert_eq!(field(&app, wid), Some(("ab".to_string(), 2)));

    assert!(
        !app.apply_local_repeat_action(wid, crate::LocalRepeatAction::Rename { session: 99, edit })
    );
    assert_eq!(field(&app, wid), Some(("ab".to_string(), 2)));
}

#[test]
fn an_ime_commit_lands_in_the_field_not_the_shell() {
    let (mut app, wid) = app_editing();
    app.on_ime_commit(wid, "é".to_string());
    assert_eq!(field(&app, wid), Some(("é".to_string(), 2)));
}
