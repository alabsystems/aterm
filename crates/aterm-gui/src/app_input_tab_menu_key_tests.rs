// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::typed_kitty_summon_tests::drain;
use crate::{App, WindowId};
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey, SmolStr,
};

fn named(key: NamedKey) -> KeyEvent {
    KeyEvent::synthetic_for_test(
        PhysicalKey::Code(KeyCode::KeyA),
        Key::Named(key),
        key.to_text().map(SmolStr::new),
        KeyLocation::Standard,
        ElementState::Pressed,
        false,
    )
}

fn app_with_strip() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    (app, wid)
}

/// [`app_with_strip`] with the session sink on a test-owned pipe, so a test can read what the PTY
/// received. Shared with the seam tests.
#[cfg(unix)]
pub(super) fn app_with_private_pty() -> (App, WindowId, [i32; 2]) {
    let (mut app, _sink, pipe) = super::typed_kitty_summon_tests::app_with_private_pty();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    (app, wid, pipe)
}

#[cfg(unix)]
pub(super) fn close(pipe: [i32; 2]) {
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// By default, Shift+F10 (`kf22`) passes through to the PTY; `tab_menu_chord = "on"` claims it.
#[cfg(unix)]
#[test]
fn shift_f10_reaches_the_program_by_default() {
    let (mut app, wid, pipe) = app_with_private_pty();
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;
    app.on_key(wid, named(NamedKey::F10));
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "an unedited config pops no card on ⇧F10"
    );
    assert!(
        !app.windows[&wid]
            .consumed_press_keys
            .contains(&PhysicalKey::Code(KeyCode::KeyA)),
        "…and does not book the press as swallowed"
    );
    assert_eq!(
        drain(pipe),
        b"\x1b[21;2~".to_vec(),
        "the program receives terminfo kf22"
    );

    app.config.tab_menu_chord = Some("on".to_string());
    app.on_key(wid, named(NamedKey::F10));
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "`on` pops the card on the same press"
    );
    assert!(
        drain(pipe).is_empty(),
        "…and a claimed press writes nothing"
    );
    close(pipe);
}

#[test]
fn shift_f10_pops_the_menu_and_bare_f10_does_not() {
    let (mut app, wid) = app_with_strip();
    app.config.tab_menu_chord = Some("on".to_string());
    app.on_key(wid, named(NamedKey::F10));
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "bare F10 belongs to the program in the terminal"
    );
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;
    app.on_key(wid, named(NamedKey::F10));
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "⇧F10 pops the focused tab's menu"
    );
}

#[test]
fn arrows_step_the_highlight_over_enabled_actions_only() {
    let (mut app, wid) = app_with_strip();
    app.on_key(wid, named(NamedKey::ContextMenu));
    let lit = |app: &App| app.windows[&wid].tab_menu.as_ref().unwrap().highlight;
    let entered = lit(&app).expect("a keyboard open enters on a row");
    let mut seen = vec![entered];
    for _ in 0..6 {
        app.on_key(wid, named(NamedKey::ArrowDown));
        let at = lit(&app).expect("the highlight never falls off the card");
        let entries = &app.windows[&wid].tab_menu.as_ref().unwrap().entries;
        assert!(
            crate::tab_menu::is_selectable(&entries[at]),
            "↓ only ever lands on an enabled action"
        );
        if at == entered {
            break;
        }
        seen.push(at);
    }
    assert!(seen.len() >= 2, "more than one action to walk: {seen:?}");
    app.on_key(wid, named(NamedKey::ArrowUp));
    assert!(lit(&app).is_some());
}

#[test]
fn an_unowned_key_dismisses_without_reaching_the_terminal() {
    let (mut app, wid) = app_with_strip();
    app.on_key(wid, named(NamedKey::ContextMenu));
    assert!(app.windows[&wid].tab_menu.is_some());
    app.on_key(wid, named(NamedKey::Tab));
    assert!(app.windows[&wid].tab_menu.is_none(), "dismissed");
    assert!(
        app.windows[&wid]
            .consumed_press_keys
            .contains(&PhysicalKey::Code(KeyCode::KeyA)),
        "a key swallowed by the menu never reaches the seam"
    );
}

/// A client that negotiated Kitty `DISAMBIGUATE_ESCAPE_CODES` keeps the Menu key (`57363`).
#[test]
fn a_kitty_client_that_negotiated_for_the_menu_key_keeps_it() {
    let (mut app, wid) = app_with_strip();
    let term = app.pool.get(0).expect("session 0").term.clone();

    app.on_key(wid, named(NamedKey::ContextMenu));
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "with no client claim the Menu key pops the card"
    );
    app.on_key(wid, named(NamedKey::Escape));

    crate::term_lock(&term).process(b"\x1b[>1u");
    app.on_key(wid, named(NamedKey::ContextMenu));
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "a negotiated Menu key belongs to the application, not the chrome"
    );

    crate::term_lock(&term).process(b"\x1b[<u");
    app.on_key(wid, named(NamedKey::ContextMenu));
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "…and it returns the moment the app stops asking for it"
    );
}

/// Under `tab_menu_chord = "on"`, only `REPORT_ALL_KEYS_AS_ESC` overrides Shift+F10.
#[test]
fn shift_f10_defers_only_to_report_all_keys() {
    let (mut app, wid) = app_with_strip();
    app.config.tab_menu_chord = Some("on".to_string());
    let term = app.pool.get(0).expect("session 0").term.clone();
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;

    crate::term_lock(&term).process(b"\x1b[>1u"); // disambiguate only
    app.on_key(wid, named(NamedKey::F10));
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "disambiguate does not take ⇧F10 away from the chrome"
    );
    app.on_key(wid, named(NamedKey::Escape));

    crate::term_lock(&term).process(b"\x1b[>8u"); // report all keys
    app.on_key(wid, named(NamedKey::F10));
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "\"report every key\" means every key"
    );
}

#[test]
fn the_config_knob_hands_each_spelling_back() {
    let shift_f10 = |app: &mut App, wid| {
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;
        app.on_key(wid, named(NamedKey::F10));
        let open = app.windows[&wid].tab_menu.is_some();
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        if open {
            app.on_key(wid, named(NamedKey::Escape));
        }
        open
    };
    let menu_key = |app: &mut App, wid| {
        app.on_key(wid, named(NamedKey::ContextMenu));
        let open = app.windows[&wid].tab_menu.is_some();
        if open {
            app.on_key(wid, named(NamedKey::Escape));
        }
        open
    };

    let (mut app, wid) = app_with_strip();
    app.config.tab_menu_chord = Some("menu_key".to_string());
    assert!(menu_key(&mut app, wid), "menu_key keeps the Menu key");
    assert!(!shift_f10(&mut app, wid), "…and surrenders ⇧F10");

    app.config.tab_menu_chord = Some("off".to_string());
    assert!(!menu_key(&mut app, wid), "off surrenders the Menu key");
    assert!(!shift_f10(&mut app, wid), "…and ⇧F10");

    app.config.tab_menu_chord = Some("on".to_string());
    assert!(menu_key(&mut app, wid), "on claims the Menu key");
    assert!(shift_f10(&mut app, wid), "…and ⇧F10, by request");

    app.config.tab_menu_chord = None;
    assert!(menu_key(&mut app, wid), "the default claims the Menu key");
    assert!(
        !shift_f10(&mut app, wid),
        "…and leaves ⇧F10 (terminfo kf22) to the application"
    );
}

#[test]
fn a_bare_modifier_press_leaves_the_card_up() {
    let (mut app, wid) = app_with_strip();
    app.on_key(wid, named(NamedKey::ContextMenu));
    let lit = app.windows[&wid].tab_menu.as_ref().unwrap().highlight;
    for m in [
        NamedKey::Shift,
        NamedKey::Control,
        NamedKey::Alt,
        NamedKey::CapsLock,
        NamedKey::NumLock,
        NamedKey::AltGraph,
    ] {
        app.on_key(wid, named(m));
        assert!(
            app.windows[&wid].tab_menu.is_some(),
            "{m:?} must not dismiss the card"
        );
    }
    assert_eq!(
        app.windows[&wid].tab_menu.as_ref().unwrap().highlight,
        lit,
        "…and must not move the highlight either"
    );
}

#[test]
fn losing_focus_dismisses_the_card() {
    let (mut app, wid) = app_with_strip();
    app.on_key(wid, named(NamedKey::ContextMenu));
    assert!(app.windows[&wid].tab_menu.is_some());
    app.on_focus(wid, false);
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "an alt-tab away takes the card with it"
    );
    assert!(app.windows[&wid].tab_menu_rect.is_none());
}
