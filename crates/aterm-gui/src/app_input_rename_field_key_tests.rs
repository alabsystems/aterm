// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::find_field_key_tests::{character, named, type_str};
use crate::{App, WindowId};
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
};

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
    let physical = PhysicalKey::Code(KeyCode::F5);
    let f5 = |state| {
        KeyEvent::synthetic_for_test(
            physical,
            Key::Named(NamedKey::F5),
            None,
            KeyLocation::Standard,
            state,
            false,
        )
    };
    app.on_key(wid, f5(ElementState::Pressed));
    assert!(
        matches!(
            app.physical_press_owners.get(&physical),
            Some(crate::PhysicalPressOwner::Forwarded {
                window,
                session: 0,
                key: aterm_types::keyboard::Key::Named(aterm_types::keyboard::NamedKey::F5),
                ..
            }) if *window == wid
        ),
        "the unbound key reaches the terminal after settling the rename"
    );
    assert_eq!(app.rename_edit_session(wid), None, "the edit is settled");
    assert_eq!(
        pin(&app, 0).as_deref(),
        Some("kept"),
        "keeping what you typed"
    );
    app.on_key(wid, f5(ElementState::Released));
    assert!(!app.physical_press_owners.contains_key(&physical));
}

#[test]
fn configured_copy_and_select_all_stay_in_the_rename_field_through_repeat_and_release() {
    use crate::keybinding::{Action, Keybindings};
    use aterm_core::selection::{SelectionSide, SelectionType};

    let original_clipboard = crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    let ctrl = ModifiersState::CONTROL;
    let ctrl_shift = ctrl | ModifiersState::SHIFT;
    let mut failures = Vec::new();
    for (binding, action, mods, code, logical, default_binding) in [
        (
            "ctrl+shift+c",
            Action::Copy,
            ctrl_shift,
            KeyCode::KeyC,
            Key::Character(SmolStr::new("C")),
            true,
        ),
        (
            "ctrl+insert",
            Action::Copy,
            ctrl,
            KeyCode::Insert,
            Key::Named(NamedKey::Insert),
            true,
        ),
        (
            "ctrl+shift+a",
            Action::SelectAll,
            ctrl_shift,
            KeyCode::KeyA,
            Key::Character(SmolStr::new("A")),
            true,
        ),
        (
            "f6",
            Action::SelectAll,
            ModifiersState::empty(),
            KeyCode::F6,
            Key::Named(NamedKey::F6),
            false,
        ),
        (
            "ctrl+a",
            Action::SelectAll,
            ctrl,
            KeyCode::KeyA,
            Key::Character(SmolStr::new("a")),
            false,
        ),
        (
            "ctrl+f",
            Action::Copy,
            ctrl,
            KeyCode::KeyF,
            Key::Character(SmolStr::new("f")),
            false,
        ),
    ] {
        let (mut app, wid) = app_editing();
        let terminal = app.pool.get(0).unwrap().term.clone();
        let sink = app.pool.get(0).unwrap().ctx.sink.clone();
        {
            let mut term = crate::term_lock(&terminal);
            term.process(b"parked terminal selection\r\nanother row");
            let selection = term.text_selection_mut();
            selection.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            selection.update_selection(0, 6, SelectionSide::Right);
            selection.complete_selection();
            assert!(selection.has_selection());
        }
        type_str(&mut app, wid, "draft λ");
        app.on_key(wid, named(NamedKey::Home));
        app.on_key(wid, named(NamedKey::ArrowRight));
        app.on_key(wid, named(NamedKey::ArrowRight));
        assert_eq!(field(&app, wid), Some(("draft λ".into(), 2)));

        // Exercise the actual non-macOS defaults, and the same explicitly configured
        // chords on macOS, whose default command bindings live in its native menu.
        let mut table = std::collections::BTreeMap::new();
        if !default_binding || cfg!(target_os = "macos") {
            table.insert(
                binding.to_string(),
                match action {
                    Action::Copy => "copy",
                    Action::SelectAll => "select_all",
                    _ => unreachable!(),
                }
                .to_string(),
            );
        }
        app.keybindings = Keybindings::resolved(Some(&table));
        let physical = PhysicalKey::Code(code);
        let event = |state, repeat| {
            KeyEvent::synthetic_for_test(
                physical,
                logical.clone(),
                None,
                KeyLocation::Standard,
                state,
                repeat,
            )
        };
        assert_eq!(
            app.keybindings.action_for(
                &super::base_logical_key(&event(ElementState::Pressed, false)),
                physical,
                mods,
            ),
            Some(action),
            "fixture must resolve {binding} to its configured command"
        );
        assert!(crate::control::pbcopy("clipboard sentinel λ"));
        let snapshot = |app: &App| {
            let term = crate::term_lock(&terminal);
            (
                app.inline_rename_edit(wid).cloned(),
                pin(app, 0),
                (
                    app.front_terminal(wid).map(|front| front.session),
                    app.windows[&wid].front_content,
                    app.windows[&wid].tab_set.active_id(),
                ),
                (
                    term.visible_content(),
                    term.content_seq(),
                    term.text_selection().clone(),
                    term.selection_to_string(),
                ),
                sink.input_epoch(),
                crate::control::pbpaste(),
            )
        };
        let before = snapshot(&app);
        app.windows.get_mut(&wid).unwrap().mods = mods;
        for (stage, state, repeat) in [
            ("press", ElementState::Pressed, false),
            ("repeat", ElementState::Pressed, true),
            ("release", ElementState::Released, false),
        ] {
            app.on_key(wid, event(state, repeat));
            let after = snapshot(&app);
            if after != before {
                failures.push(format!(
                    "{binding} {stage} changed owned state: rename={}, title={}, owner={}, \
                     terminal={}, pty={}, clipboard={}",
                    after.0 != before.0,
                    after.1 != before.1,
                    after.2 != before.2,
                    after.3 != before.3,
                    after.4 != before.4,
                    after.5 != before.5,
                ));
            }
            let owned = if state == ElementState::Released {
                !app.physical_press_owners.contains_key(&physical)
                    && !app.windows[&wid].consumed_press_keys.contains(&physical)
            } else {
                matches!(
                    app.physical_press_owners.get(&physical),
                    Some(crate::PhysicalPressOwner::Consumed { window }) if *window == wid
                ) && app.windows[&wid].consumed_press_keys.contains(&physical)
            };
            if !owned {
                failures.push(format!(
                    "{binding} {stage} must consume the command without a repeat edit: {:?}",
                    app.physical_press_owners.get(&physical)
                ));
            }
        }
    }
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = original_clipboard);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
    use crate::keybinding::{Action, ChordResolution, KeySequences, Keybindings, resolve_chord};

    for (binding, mods, code, logical) in [
        ("ctrl+g", ModifiersState::CONTROL, KeyCode::KeyG, "g"),
        (
            "ctrl+shift+с",
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            KeyCode::KeyC,
            "с",
        ),
    ] {
        let (mut app, wid) = app_editing();
        app.keybindings = Keybindings::resolved(Some(
            &[("ctrl+shift+c".into(), "copy".into())]
                .into_iter()
                .collect(),
        ));
        app.key_sequences = KeySequences::from_config(Some(
            &[(binding.to_string(), "\\e[99~".to_string())]
                .into_iter()
                .collect(),
        ));
        type_str(&mut app, wid, "kept");

        app.windows.get_mut(&wid).unwrap().mods = mods;
        let physical = PhysicalKey::Code(code);
        let event = |state| {
            KeyEvent::synthetic_for_test(
                physical,
                Key::Character(SmolStr::new(logical)),
                None,
                KeyLocation::Standard,
                state,
                false,
            )
        };
        let chord = event(ElementState::Pressed);
        let base = super::base_logical_key(&chord);
        if code == KeyCode::KeyC {
            assert_eq!(
                app.keybindings.action_for(&base, physical, mods),
                Some(Action::Copy),
                "the physical Copy fallback exists behind the layout's literal sequence"
            );
        }
        assert_eq!(
            resolve_chord(
                &base,
                physical,
                mods,
                &app.keybindings,
                &app.key_sequences,
                |_| false,
            ),
            ChordResolution::Sequence(b"\x1b[99~".to_vec()),
            "the layout's literal sequence is the effective command"
        );
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
        app.on_key(wid, event(ElementState::Released));
        assert!(!app.physical_press_owners.contains_key(&physical));
    }
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
