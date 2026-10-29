// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
};

/// A pressed key event shaped like the platform's.
pub(super) fn press(logical: Key, text: Option<&str>) -> KeyEvent {
    KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyA),
        logical,
        text.map(SmolStr::new),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    )
}

pub(super) fn named(key: NamedKey) -> KeyEvent {
    press(Key::Named(key), key.to_text())
}

pub(super) fn character(ch: &str) -> KeyEvent {
    press(Key::Character(SmolStr::new(ch)), Some(ch))
}

fn set_mods(app: &mut App, wid: WindowId, mods: ModifiersState) {
    app.windows.get_mut(&wid).unwrap().mods = mods;
}

fn query(app: &App, wid: WindowId) -> Option<(String, usize)> {
    app.windows[&wid]
        .search
        .as_ref()
        .map(|search| (search.query.clone(), search.cursor))
}

pub(super) fn type_str(app: &mut App, wid: WindowId, text: &str) {
    for ch in text.chars() {
        app.on_key(wid, character(&ch.to_string()));
    }
}

#[test]
fn escape_closes_the_bar_and_types_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    type_str(&mut app, wid, "abc");
    assert_eq!(query(&app, wid), Some(("abc".to_string(), 3)));

    app.on_key(wid, named(NamedKey::Escape));
    assert!(app.windows[&wid].search.is_none(), "⎋ closed the find bar");
    app.search_enter();
    assert_eq!(query(&app, wid), Some((String::new(), 0)));
}

#[test]
fn enter_accepts_and_tab_is_not_typed() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"needle here\r\n");
    app.search_enter();
    type_str(&mut app, wid, "needle");

    app.on_key(wid, named(NamedKey::Tab));
    assert_eq!(
        query(&app, wid),
        Some(("needle".to_string(), 6)),
        "⇥ leaves no tab in the query"
    );

    app.on_key(wid, named(NamedKey::Enter));
    assert!(app.windows[&wid].search.is_none(), "⏎ accepted and closed");
    assert_eq!(app.search_last_query, "needle", "accept remembered it");
}

#[test]
fn standard_text_field_chords_edit_the_query() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    type_str(&mut app, wid, "beta");

    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("a"));
    assert_eq!(query(&app, wid), Some(("beta".to_string(), 0)), "^A");
    set_mods(&mut app, wid, ModifiersState::empty());
    type_str(&mut app, wid, "al");
    assert_eq!(query(&app, wid), Some(("albeta".to_string(), 2)));

    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("e"));
    assert_eq!(query(&app, wid), Some(("albeta".to_string(), 6)), "^E");

    app.on_key(wid, character("b"));
    assert_eq!(query(&app, wid).unwrap().1, 5, "^B");
    app.on_key(wid, character("f"));
    assert_eq!(query(&app, wid).unwrap().1, 6, "^F");

    set_mods(&mut app, wid, ModifiersState::empty());
    app.on_key(wid, named(NamedKey::ArrowLeft));
    app.on_key(wid, named(NamedKey::ArrowLeft));
    assert_eq!(query(&app, wid), Some(("albeta".to_string(), 4)), "←←");
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("k"));
    assert_eq!(query(&app, wid), Some(("albe".to_string(), 4)), "^K");
    app.on_key(wid, character("u"));
    assert_eq!(query(&app, wid), Some((String::new(), 0)), "^U");

    set_mods(&mut app, wid, ModifiersState::empty());
    type_str(&mut app, wid, "one two");
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("w"));
    assert_eq!(query(&app, wid), Some(("one ".to_string(), 4)), "^W");

    set_mods(&mut app, wid, ModifiersState::empty());
    type_str(&mut app, wid, "two three");
    set_mods(&mut app, wid, ModifiersState::ALT);
    app.on_key(wid, named(NamedKey::ArrowLeft));
    assert_eq!(
        query(&app, wid),
        Some(("one two three".to_string(), 8)),
        "⌥←"
    );
    app.on_key(wid, named(NamedKey::Backspace));
    assert_eq!(query(&app, wid), Some(("one three".to_string(), 4)), "⌥⌫");
    set_mods(&mut app, wid, ModifiersState::SUPER);
    app.on_key(wid, named(NamedKey::Backspace));
    assert_eq!(query(&app, wid), Some(("three".to_string(), 0)), "⌘⌫");
    app.on_key(wid, named(NamedKey::ArrowRight));
    assert_eq!(query(&app, wid).unwrap().1, 5, "⌘→");
    app.on_key(wid, named(NamedKey::ArrowLeft));
    assert_eq!(query(&app, wid).unwrap().1, 0, "⌘←");

    set_mods(&mut app, wid, ModifiersState::empty());
    app.on_key(wid, named(NamedKey::End));
    assert_eq!(query(&app, wid).unwrap().1, 5, "End");
    app.on_key(wid, named(NamedKey::Home));
    assert_eq!(query(&app, wid).unwrap().1, 0, "Home");
    app.on_key(wid, named(NamedKey::Delete));
    assert_eq!(query(&app, wid), Some(("hree".to_string(), 0)), "⌦");
}

#[test]
fn ime_commit_types_into_the_field_not_the_shell() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let before = term_lock(&term).content_seq();
    app.search_enter();
    type_str(&mut app, wid, "caf");
    app.on_ime_commit(wid, "\u{e9}".to_string());
    assert_eq!(
        query(&app, wid),
        Some(("caf\u{e9}".to_string(), 5)),
        "the composed character landed in the query at the caret"
    );
    assert_eq!(
        term_lock(&term).content_seq(),
        before,
        "and never reached the shell"
    );
    app.search_cancel();
    app.on_ime_commit(wid, "\u{e9}".to_string());
    assert!(app.windows[&wid].search.is_none());
}

#[test]
fn command_chords_abort_and_toggle_through_the_key_path() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut lines = Vec::new();
    for i in 0..80 {
        lines.extend_from_slice(format!("line {i} needle\r\n").as_bytes());
    }
    term_lock(&term).process(&lines);
    term_lock(&term).scroll_display(5);
    let origin = term_lock(&term).grid().display_offset();

    app.search_enter();
    type_str(&mut app, wid, "needle");
    assert!(
        term_lock(&term).grid().display_offset() != origin,
        "find navigated away from the origin viewport"
    );
    set_mods(&mut app, wid, ModifiersState::SUPER | ModifiersState::ALT);
    app.on_key(wid, press(Key::Character(SmolStr::new("c")), Some("ç")));
    assert!(
        app.windows[&wid].search.as_ref().unwrap().case_sensitive,
        "⌥⌘C"
    );
    app.on_key(wid, press(Key::Character(SmolStr::new("r")), Some("®")));
    assert!(app.windows[&wid].search.as_ref().unwrap().is_regex, "⌥⌘R");

    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("g"));
    assert!(app.windows[&wid].search.is_none(), "^G closed the find bar");
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        origin,
        "^G restored the pre-find viewport"
    );
}

#[test]
fn the_field_claims_text_input_not_every_chord() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    let mut lines = Vec::new();
    for i in 0..60 {
        lines.extend_from_slice(format!("line {i} needle\r\n").as_bytes());
    }
    term_lock(&term).process(&lines);
    app.search_enter();
    type_str(&mut app, wid, "needle");
    let found = term_lock(&term).grid().display_offset();

    set_mods(&mut app, wid, ModifiersState::SHIFT);
    app.on_key(wid, named(NamedKey::Home));
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        found,
        "a scrollback chord cannot desync the viewport from the find"
    );
    assert!(app.windows[&wid].search.is_some(), "…and find stays open");

    set_mods(&mut app, wid, ModifiersState::SUPER);
    assert!(
        !app.on_key_search_mode(wid, ModifiersState::SUPER, &character("c")),
        "⌘C falls through to the terminal's copy"
    );
    assert!(
        term_lock(&term).selection_to_string().is_some(),
        "the find left a match selected for that copy"
    );
}

#[test]
fn bound_copy_copies_the_find_match_without_editing_or_repeating_the_query() {
    use crate::keybinding::{Action, ChordResolution, KeySequences, Keybindings, resolve_chord};

    let original_clipboard = crate::control::PBPASTE_STUB.with(|board| board.borrow().clone());
    let ctrl = ModifiersState::CONTROL;
    let ctrl_shift = ctrl | ModifiersState::SHIFT;
    let mut failures = Vec::new();
    for (binding, mods, code, logical, default_binding, layout_sequence) in [
        (
            "ctrl+shift+c",
            ctrl_shift,
            KeyCode::KeyC,
            Key::Character(SmolStr::new("C")),
            true,
            false,
        ),
        (
            "ctrl+insert",
            ctrl,
            KeyCode::Insert,
            Key::Named(NamedKey::Insert),
            true,
            false,
        ),
        (
            "ctrl+f",
            ctrl,
            KeyCode::KeyF,
            Key::Character(SmolStr::new("f")),
            false,
            false,
        ),
        (
            "ctrl+shift+c",
            ctrl_shift,
            KeyCode::KeyC,
            Key::Character(SmolStr::new("с")),
            true,
            true,
        ),
    ] {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).unwrap().term.clone();
        let sink = app.pool.get(0).unwrap().ctx.sink.clone();
        term_lock(&term).process("prefix λneedle suffix\r\n".as_bytes());
        app.search_enter();
        type_str(&mut app, wid, "λneedle");
        app.search_edit_in(wid, crate::app_search::SearchEdit::MoveTo(2));
        assert_eq!(query(&app, wid), Some(("λneedle".into(), 2)));
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("λneedle"),
            "the real search selected a Unicode match"
        );
        assert!(app.copy_selection_in(wid));
        assert_eq!(
            crate::control::pbpaste().as_deref(),
            Some("λneedle"),
            "the shared menu Copy path can copy the active match"
        );

        let mut table = std::collections::BTreeMap::new();
        if !default_binding || cfg!(target_os = "macos") {
            table.insert(binding.to_string(), "copy".to_string());
        }
        app.keybindings = Keybindings::resolved(Some(&table));
        if layout_sequence {
            app.key_sequences = KeySequences::from_config(Some(
                &[("ctrl+shift+с".to_string(), "\\e[99~".to_string())]
                    .into_iter()
                    .collect(),
            ));
        }
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
        let base = super::base_logical_key(&event(ElementState::Pressed, false));
        assert_eq!(
            app.keybindings.action_for(&base, physical, mods),
            Some(Action::Copy),
            "fixture must provide the configured Copy binding"
        );
        assert_eq!(
            resolve_chord(
                &base,
                physical,
                mods,
                &app.keybindings,
                &app.key_sequences,
                |_| false,
            ),
            if layout_sequence {
                ChordResolution::Sequence(b"\x1b[99~".to_vec())
            } else {
                ChordResolution::Action(Action::Copy)
            },
            "the layout's literal sequence must precede a physical Copy fallback"
        );
        assert!(crate::control::pbcopy("clipboard sentinel λ"));
        let snapshot = |app: &App| {
            let search = app.windows[&wid].search.as_ref().map(|search| {
                (
                    search.query.clone(),
                    search.cursor,
                    search.matches.clone(),
                    search.current,
                    search.direction,
                    search.case_sensitive,
                    search.is_regex,
                    search.regex_error,
                )
            });
            let terminal = term_lock(&term);
            (
                search,
                (
                    terminal.visible_content(),
                    terminal.content_seq(),
                    terminal.grid().display_offset(),
                    terminal.text_selection().clone(),
                ),
                sink.input_epoch(),
                (
                    app.frontmost_window,
                    app.windows[&wid].front_content,
                    app.front_terminal(wid).map(|front| front.session),
                ),
            )
        };
        let before = snapshot(&app);
        let expected_clipboard = if layout_sequence {
            "clipboard sentinel λ"
        } else {
            "λneedle"
        };
        set_mods(&mut app, wid, mods);
        for (stage, state, repeat) in [
            ("press", ElementState::Pressed, false),
            ("repeat", ElementState::Pressed, true),
            ("release", ElementState::Released, false),
        ] {
            app.on_key(wid, event(state, repeat));
            let after = snapshot(&app);
            if after != before {
                failures.push(format!(
                    "{binding} layout_sequence={layout_sequence} {stage} changed owned state: \
                     query={}, terminal={}, pty={}, owner={}",
                    after.0 != before.0,
                    after.1 != before.1,
                    after.2 != before.2,
                    after.3 != before.3,
                ));
            }
            let copied = crate::control::pbpaste();
            if copied.as_deref() != Some(expected_clipboard) {
                failures.push(format!(
                    "{binding} layout_sequence={layout_sequence} {stage}: \
                     clipboard={copied:?}, expected={expected_clipboard:?}"
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
                    "{binding} layout_sequence={layout_sequence} {stage} must not repeat \
                     a field edit or terminal input: {:?}",
                    app.physical_press_owners.get(&physical)
                ));
            }
        }
    }
    crate::control::PBPASTE_STUB.with(|board| *board.borrow_mut() = original_clipboard);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn other_input_doors_defer_to_an_open_field() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(
        app.find_field_window().is_none(),
        "no find ⇒ the menu keeps its ordinary targets"
    );
    app.search_enter();
    assert_eq!(
        app.find_field_window(),
        Some(wid),
        "an open find claims ⌘V / ⌘A from the menu bar"
    );

    use crate::keybinding::Action;
    for action in [
        Action::Paste,
        Action::ScrollToTop,
        Action::ScrollPageUp,
        Action::JumpPrevPrompt,
        Action::FocusPaneLeft,
        Action::FocusPaneRight,
    ] {
        assert!(super::find_suspends_action(action), "{action:?}");
    }
    for action in [
        Action::Copy,
        Action::Find,
        Action::NewTab,
        Action::CloseTab,
        Action::FontIncrease,
        Action::SplitVertical,
        Action::ToggleSettings,
        Action::FocusPaneUp,
        Action::FocusPaneDown,
    ] {
        assert!(!super::find_suspends_action(action), "{action:?}");
    }
}

#[test]
fn field_editing_re_runs_the_search_but_never_reaches_the_shell() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"alpha beta gamma\r\n");
    let before = term_lock(&term).content_seq();

    app.search_enter();
    type_str(&mut app, wid, "beta");
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().matches.len(),
        1,
        "typing re-ran the real search"
    );
    app.on_key(wid, named(NamedKey::Backspace));
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().query, "bet");
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().matches.len(), 1);
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("a"));
    set_mods(&mut app, wid, ModifiersState::empty());
    assert_eq!(
        term_lock(&term).content_seq(),
        before,
        "no find-field keystroke reached the shell"
    );
}

#[test]
fn a_bound_paste_chord_pastes_into_the_query_never_the_pty() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.keybindings = crate::keybinding::Keybindings::resolved(Some(
        &[("ctrl+y".to_string(), "paste".to_string())]
            .into_iter()
            .collect(),
    ));
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"needle here\r\n");
    let before = term_lock(&term).content_seq();
    crate::control::PBPASTE_STUB.with(|s| *s.borrow_mut() = Some("needle".to_string()));

    app.search_enter();
    app.on_key(wid, character("n"));
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("y"));
    assert_eq!(
        query(&app, wid),
        Some(("nneedle".to_string(), 7)),
        "the bound chord pasted the (stubbed) clipboard at the caret"
    );
    assert!(app.windows[&wid].search.is_some(), "find stays open");
    assert_eq!(
        term_lock(&term).content_seq(),
        before,
        "nothing reached the shell"
    );
    crate::control::PBPASTE_STUB.with(|s| *s.borrow_mut() = None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn every_seeded_paste_spelling_pastes_into_the_query() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.keybindings = crate::keybinding::Keybindings::platform_defaults();
    crate::control::PBPASTE_STUB.with(|s| *s.borrow_mut() = Some("x".to_string()));

    app.search_enter();
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("v"));
    let mut q = String::new();
    if cfg!(windows) {
        q.push('x');
        assert_eq!(
            query(&app, wid),
            Some((q.clone(), 1)),
            "ctrl+v (Windows seed)"
        );
    } else {
        assert_eq!(
            query(&app, wid),
            Some((String::new(), 0)),
            "plain ctrl+v is NOT seeded on Linux — the field ignores it"
        );
    }
    set_mods(
        &mut app,
        wid,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
    );
    app.on_key(wid, character("v"));
    q.push('x');
    assert_eq!(query(&app, wid), Some((q.clone(), q.len())), "ctrl+shift+v");
    set_mods(&mut app, wid, ModifiersState::SHIFT);
    app.on_key(wid, named(NamedKey::Insert));
    q.push('x');
    assert_eq!(query(&app, wid), Some((q.clone(), q.len())), "shift+insert");
    assert!(app.windows[&wid].search.is_some(), "find stays open");
    crate::control::PBPASTE_STUB.with(|s| *s.borrow_mut() = None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_named_keys_are_word_motions_in_the_field_off_macos() {
    use crate::app_search::SearchEdit;
    let classify = |mods, ev: &aterm_window::event::KeyEvent| super::field_edit_action(mods, ev);
    let ctrl = ModifiersState::CONTROL;
    assert_eq!(
        classify(ctrl, &named(NamedKey::ArrowLeft)),
        Some(SearchEdit::MoveWordLeft),
        "Ctrl+← walks a word"
    );
    assert_eq!(
        classify(ctrl, &named(NamedKey::ArrowRight)),
        Some(SearchEdit::MoveWordRight),
        "Ctrl+→ walks a word"
    );
    assert_eq!(
        classify(ctrl, &named(NamedKey::Backspace)),
        Some(SearchEdit::DeleteWordBack),
        "Ctrl+⌫ eats the previous word"
    );
    assert_eq!(
        classify(ctrl, &named(NamedKey::Delete)),
        Some(SearchEdit::DeleteWordForward),
        "Ctrl+⌦ eats the next word"
    );
    assert_eq!(
        classify(ModifiersState::ALT, &named(NamedKey::ArrowLeft)),
        Some(SearchEdit::MoveWordLeft),
        "⌥← still walks a word"
    );
    assert_eq!(
        classify(ctrl, &character("b")),
        Some(SearchEdit::MoveCharLeft),
        "^B stays by-character"
    );
    assert_eq!(
        classify(ctrl | ModifiersState::ALT, &named(NamedKey::ArrowLeft)),
        Some(SearchEdit::MoveCharLeft),
        "Ctrl+Alt+← is not a word motion"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn ctrl_named_keys_stay_character_motions_on_macos() {
    use crate::app_search::SearchEdit;
    let classify = |mods, ev: &aterm_window::event::KeyEvent| super::field_edit_action(mods, ev);
    let ctrl = ModifiersState::CONTROL;
    assert_eq!(
        classify(ctrl, &named(NamedKey::ArrowLeft)),
        Some(SearchEdit::MoveCharLeft)
    );
    assert_eq!(
        classify(ctrl, &named(NamedKey::Backspace)),
        Some(SearchEdit::DeleteBack)
    );
    assert_eq!(
        classify(ctrl, &named(NamedKey::Delete)),
        Some(SearchEdit::DeleteForward)
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn f3_and_shift_f3_step_the_matches_off_macos() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let term = app.pool.get(0).expect("session 0").term.clone();
    term_lock(&term).process(b"hit one\r\nhit two\r\nhit three\r\n");
    app.search_enter();
    type_str(&mut app, wid, "hit");
    let current = |app: &App| app.windows[&wid].search.as_ref().map(|s| s.current);
    let start = current(&app).expect("the field is open");
    app.on_key(wid, named(NamedKey::F3));
    assert_eq!(
        current(&app),
        Some((start + 1) % 3),
        "F3 steps to the next match"
    );
    set_mods(&mut app, wid, ModifiersState::SHIFT);
    app.on_key(wid, named(NamedKey::F3));
    set_mods(&mut app, wid, ModifiersState::empty());
    assert_eq!(current(&app), Some(start), "Shift+F3 steps back");
    assert_eq!(
        query(&app, wid),
        Some(("hit".to_string(), 3)),
        "nothing typed"
    );
    let ev = named(NamedKey::F3);
    assert!(
        app.search_repeat_action(wid, ModifiersState::CONTROL, &ev)
            .is_none(),
        "Ctrl+F3 is not the field's"
    );
}

/// Open find on the front tab, start a query, run `close` (which must leave the front tab
/// and its focused pane in place), then finish the query: every key must have reached
/// the field, none the shell.
fn find_survives(app: &mut App, case: &str, close: impl FnOnce(&mut App)) {
    let wid = WindowId(0);
    let front = app.focused_session_id(wid);
    app.search_enter();
    type_str(app, wid, "dep");
    close(app);
    assert_eq!(
        app.focused_session_id(wid),
        front,
        "{case}: fixture keeps the front"
    );
    assert_eq!(
        query(app, wid),
        Some(("dep".to_string(), 3)),
        "{case}: the find field and its query outlive a close behind them"
    );
    type_str(app, wid, "loy");
    assert_eq!(
        query(app, wid),
        Some(("deploy".to_string(), 6)),
        "{case}: the rest of the query reached the field"
    );
    app.on_key(wid, named(NamedKey::Enter));
    assert!(app.windows[&wid].search.is_none(), "{case}: ⏎ accepted");
    assert_eq!(
        app.search_last_query, "deploy",
        "{case}: ⏎ accepted the whole query"
    );
}

#[test]
fn a_close_behind_the_front_tab_keeps_its_find_field() {
    let wid = WindowId(0);

    let mut app = App::headless_for_test();
    let background = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(background));
    app.switch_tab_in(wid, 0);
    find_survives(&mut app, "aterm ctl close of a background tab", |app| {
        assert!(app.close_session_by_id(background).is_ok());
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            1,
            "the background tab closed"
        );
    });

    let mut app = App::headless_for_test();
    let background = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(background));
    app.switch_tab_in(wid, 0);
    find_survives(&mut app, "a background tab's shell exiting", |app| {
        assert!(app.exit_session_logical(background).is_empty());
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            1,
            "the background tab closed"
        );
    });

    let mut app = App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    app.switch_tab_in(wid, 0);
    find_survives(&mut app, "aterm ctl settings close", |app| {
        assert!(app.close_settings_tabs());
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            1,
            "the Settings tab closed"
        );
    });

    let mut app = App::headless_for_test();
    let sibling = app.split_active_stub_tab(wid);
    app.focus_pane(crate::pane::FocusDir::Left);
    assert_eq!(
        app.focused_session_id(wid),
        Some(0),
        "fixture: session 0 focused"
    );
    find_survives(&mut app, "a split sibling's shell exiting", |app| {
        assert!(app.exit_session_logical(sibling).is_empty());
        assert_eq!(
            app.windows[&wid].layouts[0].sessions(),
            vec![0],
            "the split collapsed"
        );
    });
}

#[test]
fn find_closes_with_the_content_it_searches() {
    let wid = WindowId(0);
    let open = |app: &mut App| {
        app.search_enter();
        type_str(app, wid, "dep");
        assert!(app.windows[&wid].search.is_some(), "fixture: find is open");
    };

    let mut app = App::headless_for_test();
    app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
    open(&mut app);
    app.switch_tab_in(wid, 0);
    assert!(
        app.windows[&wid].search.is_none(),
        "a tab switch closes find"
    );

    let mut app = App::headless_for_test();
    let front = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(front));
    open(&mut app);
    assert!(app.exit_session_logical(front).is_empty());
    assert!(
        app.windows[&wid].search.is_none(),
        "the front tab's close closes find"
    );

    let mut app = App::headless_for_test();
    let _ = app.split_active_stub_tab(wid);
    open(&mut app);
    app.focus_pane(crate::pane::FocusDir::Left);
    assert_eq!(app.focused_session_id(wid), Some(0), "fixture: focus moved");
    assert!(
        app.windows[&wid].search.is_none(),
        "a pane focus move closes find"
    );
}

/// A CONTROL CLIENT'S FIND NEXT RECALLS NONE OF THE PERSON'S FINDS: the person finds `hunter2`
/// (not on screen) in a terminal and accepts it; a control client's Find Next (its `invoke`, or a
/// key it sent, under the controller mark) and its `^S` in an empty find field recall a find of
/// the client's own (none), so the find bar it reads never shows the person's query. The
/// client's own accepted find is recalled for it, and the person's Find Next still recalls
/// theirs.
/// NEGATIVE CONTROL: with `remembered_find` mutated to pick the person's find for every gesture
/// (main's one find memory, `147b22abdc`), the client's Find Next found the person's `hunter2`
/// to step (it answered `Ok(())` where it must refuse).
#[test]
fn a_controllers_find_next_recalls_none_of_the_persons_finds() {
    use crate::app_search::SearchEdit;
    const SECRET: &str = "hunter2";
    let wid = WindowId(0);
    let mut app = App::headless_for_test();
    let shown = |app: &App| query(app, wid).map(|(query, _)| query);
    let accept = |app: &mut App, text: &str| {
        app.search_enter();
        app.search_edit_in(wid, SearchEdit::Insert(text.to_string()));
        app.search_accept_in(wid);
    };
    accept(&mut app, SECRET);
    assert_eq!(shown(&app), None, "PRECONDITION: the person accepted it");

    let stepped = app.with_gesture_source(true, |app| app.search_find_again_in(wid, true));
    assert!(
        stepped.is_err(),
        "the client has no find to step: {stepped:?}"
    );
    assert_eq!(shown(&app), None, "the client's Find Next");
    app.with_gesture_source(true, |app| {
        app.search_enter();
        app.search_repeat_in(wid, true);
    });
    assert_eq!(shown(&app).as_deref(), Some(""), "the client's ^S");
    app.search_close_in(wid, false);
    app.with_gesture_source(true, |app| accept(app, "own"));
    let stepped = app.with_gesture_source(true, |app| app.search_find_again_in(wid, true));
    assert!(stepped.is_ok(), "{stepped:?}");
    assert_eq!(shown(&app).as_deref(), Some("own"), "the client's own find");
    app.search_close_in(wid, false);

    assert!(app.search_find_again_in(wid, true).is_ok());
    assert_eq!(
        shown(&app).as_deref(),
        Some(SECRET),
        "the person's Find Next"
    );
}
