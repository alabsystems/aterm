// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId, term_lock};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr};

/// A pressed key event shaped like the platform's: `text` is what winit reports,
/// including the control strings for ⎋ (`\u{1b}`), ⏎ (`\r`) and ⇥ (`\t`), which
/// must never reach the query as text.
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

/// ⎋ closes the find bar; it is a command, never a character (winit reports
/// `text = "\u{1b}"` for it).
#[test]
fn escape_closes_the_bar_and_types_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    type_str(&mut app, wid, "abc");
    assert_eq!(query(&app, wid), Some(("abc".to_string(), 3)));

    app.on_key(wid, named(NamedKey::Escape));
    assert!(app.windows[&wid].search.is_none(), "⎋ closed the find bar");
    // And the cancelled query never became text: reopening starts empty.
    app.search_enter();
    assert_eq!(query(&app, wid), Some((String::new(), 0)));
}

/// ⏎ accepts (closes, keeping the match) and ⇥ is inert; neither leaves its
/// control character in the query.
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

/// ^A / ^E / ←→ / ⌥← / ^K / ^U / ^W behave like they do in any macOS text field,
/// and typing lands at the caret rather than being appended.
#[test]
fn standard_text_field_chords_edit_the_query() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.search_enter();
    type_str(&mut app, wid, "beta");

    // ^A → start of line, then typing inserts there.
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("a"));
    assert_eq!(query(&app, wid), Some(("beta".to_string(), 0)), "^A");
    set_mods(&mut app, wid, ModifiersState::empty());
    type_str(&mut app, wid, "al");
    assert_eq!(query(&app, wid), Some(("albeta".to_string(), 2)));

    // ^E → end of line.
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("e"));
    assert_eq!(query(&app, wid), Some(("albeta".to_string(), 6)), "^E");

    // ^B / ^F step by character.
    app.on_key(wid, character("b"));
    assert_eq!(query(&app, wid).unwrap().1, 5, "^B");
    app.on_key(wid, character("f"));
    assert_eq!(query(&app, wid).unwrap().1, 6, "^F");

    // ^K from the middle kills to the end; ^U kills to the start.
    set_mods(&mut app, wid, ModifiersState::empty());
    app.on_key(wid, named(NamedKey::ArrowLeft));
    app.on_key(wid, named(NamedKey::ArrowLeft));
    assert_eq!(query(&app, wid), Some(("albeta".to_string(), 4)), "←←");
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("k"));
    assert_eq!(query(&app, wid), Some(("albe".to_string(), 4)), "^K");
    app.on_key(wid, character("u"));
    assert_eq!(query(&app, wid), Some((String::new(), 0)), "^U");

    // ^W deletes the previous word.
    set_mods(&mut app, wid, ModifiersState::empty());
    type_str(&mut app, wid, "one two");
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("w"));
    assert_eq!(query(&app, wid), Some(("one ".to_string(), 4)), "^W");

    // ⌥← walks back a word (a plain ⌥+letter must stay free for composed input,
    // so word motion lives on the arrows).
    set_mods(&mut app, wid, ModifiersState::empty());
    type_str(&mut app, wid, "two three");
    set_mods(&mut app, wid, ModifiersState::ALT);
    app.on_key(wid, named(NamedKey::ArrowLeft));
    assert_eq!(
        query(&app, wid),
        Some(("one two three".to_string(), 8)),
        "⌥←"
    );
    // ⌥⌫ eats that word; ⌘⌫ clears back to the start.
    app.on_key(wid, named(NamedKey::Backspace));
    assert_eq!(query(&app, wid), Some(("one three".to_string(), 4)), "⌥⌫");
    set_mods(&mut app, wid, ModifiersState::SUPER);
    app.on_key(wid, named(NamedKey::Backspace));
    assert_eq!(query(&app, wid), Some(("three".to_string(), 0)), "⌘⌫");
    // ⌘→ / ⌘← are the line-edge motions.
    app.on_key(wid, named(NamedKey::ArrowRight));
    assert_eq!(query(&app, wid).unwrap().1, 5, "⌘→");
    app.on_key(wid, named(NamedKey::ArrowLeft));
    assert_eq!(query(&app, wid).unwrap().1, 0, "⌘←");

    // Home / End / ⌦ round out the field's vocabulary.
    set_mods(&mut app, wid, ModifiersState::empty());
    app.on_key(wid, named(NamedKey::End));
    assert_eq!(query(&app, wid).unwrap().1, 5, "End");
    app.on_key(wid, named(NamedKey::Home));
    assert_eq!(query(&app, wid).unwrap().1, 0, "Home");
    app.on_key(wid, named(NamedKey::Delete));
    assert_eq!(query(&app, wid), Some(("hree".to_string(), 0)), "⌦");
}

/// Composed text (macOS ⌥-dead keys, CJK IME) reaches the field, not the shell.
/// On macOS it arrives as an IME commit rather than key text, so the find bar
/// claims that path too.
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
    // With find closed the commit takes the ordinary PTY path again (headless has
    // no shell to echo it, so this pins only that the find branch stopped claiming
    // it).
    app.search_cancel();
    app.on_ime_commit(wid, "\u{e9}".to_string());
    assert!(app.windows[&wid].search.is_none());
}

/// Command chords through the real key routing: ^G aborts (restoring the
/// pre-find viewport, like ⎋), and ⌥⌘C / ⌥⌘R flip match-case / regex even when
/// macOS composes the key (⌥⌘C arrives as "ç").
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
    // The user was reading 5 lines above the live bottom when they pressed ⌘F.
    term_lock(&term).scroll_display(5);
    let origin = term_lock(&term).grid().display_offset();

    app.search_enter();
    type_str(&mut app, wid, "needle");
    assert!(
        term_lock(&term).grid().display_offset() != origin,
        "find navigated away from the origin viewport"
    );
    // The event text carries the ⌥-composed glyph (ç / ®); these arms match on
    // `base_logical_key`.
    set_mods(&mut app, wid, ModifiersState::SUPER | ModifiersState::ALT);
    app.on_key(wid, press(Key::Character(SmolStr::new("c")), Some("ç")));
    assert!(
        app.windows[&wid].search.as_ref().unwrap().case_sensitive,
        "⌥⌘C"
    );
    app.on_key(wid, press(Key::Character(SmolStr::new("r")), Some("®")));
    assert!(app.windows[&wid].search.as_ref().unwrap().is_regex, "⌥⌘R");

    // ^G: the emacs abort — closes and restores the viewport the find started from.
    set_mods(&mut app, wid, ModifiersState::CONTROL);
    app.on_key(wid, character("g"));
    assert!(app.windows[&wid].search.is_none(), "^G closed the find bar");
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        origin,
        "^G restored the pre-find viewport"
    );
}

/// The field claims text input, not the whole keyboard: ⌘C still copies the
/// highlighted match, and ⇧Home does not move the viewport under an open find.
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

    // ⇧Home would jump to the oldest line; with find open it is suspended.
    set_mods(&mut app, wid, ModifiersState::SHIFT);
    app.on_key(wid, named(NamedKey::Home));
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        found,
        "a scrollback chord cannot desync the viewport from the find"
    );
    assert!(app.windows[&wid].search.is_some(), "…and find stays open");

    // ⌘C is not swallowed: it falls through to the terminal copy of the match.
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

/// The routing predicates the other input doors consult: AppKit menu equivalents
/// (⌘V / ⌘A never reach `on_key`) and configured `[keybindings]` actions.
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
    // Text- and viewport-level configured actions are suspended; app-level ones
    // (windows, tabs, panes, font, find itself) keep working with the panel open.
    use crate::keybinding::Action;
    for action in [
        Action::Paste,
        Action::Copy,
        Action::ScrollToTop,
        Action::ScrollPageUp,
        Action::JumpPrevPrompt,
        Action::ToggleViMode,
        // Not text-level, but its default chord is the field's only word-motion
        // chord off macOS (Alt+←/→), and this block runs above the find gate.
        Action::FocusPaneLeft,
        Action::FocusPaneRight,
    ] {
        assert!(super::find_suspends_action(action), "{action:?}");
    }
    for action in [
        Action::Find,
        Action::NewTab,
        Action::CloseTab,
        Action::FontIncrease,
        Action::SplitVertical,
        Action::ToggleSettings,
        // Alt+↑/↓ mean nothing to a single-line field, so vertical pane focus
        // keeps working with the panel open.
        Action::FocusPaneUp,
        Action::FocusPaneDown,
    ] {
        assert!(!super::find_suspends_action(action), "{action:?}");
    }
}

/// The find field owns the keystroke: none of its editing chords reach the PTY,
/// and the query still drives the real search (the caret moves are not searches).
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
    // Backspacing to a prefix widens the match set again.
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

/// A chord bound to `Action::Paste` pastes the (stubbed, `PBPASTE_STUB`)
/// clipboard into the query through the real `on_key` routing, never into the
/// PTY: the find gate's suspension arm routes the action into the field.
#[test]
fn a_bound_paste_chord_pastes_into_the_query_never_the_pty() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    // A user rebind, overlaid as App::new overlays it. On macOS the platform
    // table is empty, so this also pins the suspension arm from a bare user
    // table.
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

/// The seeded paste spellings (`Keybindings::PLATFORM_DEFAULT_PAIRS`) all
/// paste into the query. Plain ctrl+v is seeded on Windows only: on Linux it
/// stays the PTY's (readline quoted-insert, vim visual-block), so in the find
/// field it does nothing. Platform defaults installed, no user table. Not on
/// macOS, which seeds no table (⌘V is its hardcoded find-field paste).
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

/// Off macOS, Ctrl is the platform word modifier in the shared field keymap
/// (`field_edit_action`'s cfg'd `by_word` term): Ctrl+←/→ walk words and
/// Ctrl+⌫/⌦ eat them, Alt stays an alias, the ctrl-letter emacs arms stay
/// by-character, and Ctrl+Alt (an AltGr spelling) is neither, so composed
/// input never word-jumps.
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_named_keys_are_word_motions_in_the_field_off_macos() {
    use crate::app_search::SearchEdit;
    let classify = |mods, ev: &winit::event::KeyEvent| super::field_edit_action(mods, ev);
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
    // Alt stays an alias — the pane-nav suspension preserves it for the field.
    assert_eq!(
        classify(ModifiersState::ALT, &named(NamedKey::ArrowLeft)),
        Some(SearchEdit::MoveWordLeft),
        "⌥← still walks a word"
    );
    // The ctrl-letter emacs arms are untouched: ^B is by one character.
    assert_eq!(
        classify(ctrl, &character("b")),
        Some(SearchEdit::MoveCharLeft),
        "^B stays by-character"
    );
    // Ctrl+Alt together qualifies for neither word term (AltGr safety).
    assert_eq!(
        classify(ctrl | ModifiersState::ALT, &named(NamedKey::ArrowLeft)),
        Some(SearchEdit::MoveCharLeft),
        "Ctrl+Alt+← is not a word motion"
    );
}

/// On macOS Ctrl+named-key is not a word motion (Ctrl+arrow belongs to
/// Mission Control / Spaces); ⌥ alone owns words.
#[cfg(target_os = "macos")]
#[test]
fn ctrl_named_keys_stay_character_motions_on_macos() {
    use crate::app_search::SearchEdit;
    let classify = |mods, ev: &winit::event::KeyEvent| super::field_edit_action(mods, ev);
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
