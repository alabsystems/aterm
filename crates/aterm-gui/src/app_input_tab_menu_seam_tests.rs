// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::input::{InputEvent, Source};
use crate::{App, WindowId};
use aterm_types::keyboard::{Key as EKey, KeyEventType, Modifiers, NamedKey as ENamed};

const CTL: Source = Source::Controller;

fn app_with_strip() -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.tab_strip_rows = 1;
    (app, wid)
}

fn key(named: ENamed, mods: Modifiers) -> InputEvent {
    InputEvent::Key {
        key: EKey::Named(named),
        mods,
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    }
}

#[test]
fn ctl_key_menu_pops_the_card_the_physical_key_pops() {
    let (mut app, wid) = app_with_strip();
    app.input(wid, key(ENamed::ContextMenu, Modifiers::empty()), CTL);
    let menu = app.windows[&wid]
        .tab_menu
        .as_ref()
        .expect("the controller pops the same card");
    assert!(
        menu.highlight.is_some(),
        "…as a KEYBOARD open, entered on the first live row"
    );
    assert!(
        app.windows[&wid]
            .overlay_consumed_keys
            .contains(&EKey::Named(ENamed::ContextMenu)),
        "the swallowed press is booked so its release cannot orphan"
    );
}

/// With the default config, `aterm ctl key shift+f10` writes terminfo `kf22`
/// (`ESC[21;2~`) and pops nothing, and a human-sourced press through the
/// same seam writes the same bytes. Under `tab_menu_chord = "on"` the
/// controller's press pops the card and the PTY sees nothing.
#[cfg(unix)]
#[test]
fn shift_f10_reaches_the_program_by_default_on_both_sources() {
    use super::tab_menu_key_tests::{app_with_private_pty, close};
    use super::typed_kitty_summon_tests::drain;

    let (mut app, wid, pipe) = app_with_private_pty();
    for src in [Source::Human, CTL] {
        assert_eq!(
            app.input(wid, key(ENamed::F10, Modifiers::SHIFT), src),
            crate::input::InputOutcome::Ok,
            "{src:?}: the press was delivered"
        );
        assert!(
            app.windows[&wid].tab_menu.is_none(),
            "{src:?}: an unedited config pops no card on ⇧F10"
        );
        assert_eq!(
            drain(pipe),
            b"\x1b[21;2~".to_vec(),
            "{src:?}: the program receives terminfo kf22"
        );
    }

    app.config.tab_menu_chord = Some("on".to_string());
    app.input(wid, key(ENamed::F10, Modifiers::SHIFT), CTL);
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "`on` pops the card for the controller"
    );
    assert!(
        drain(pipe).is_empty(),
        "…and the claimed press writes nothing"
    );
    close(pipe);
}

#[test]
fn the_controller_walks_and_activates_the_card() {
    let (mut app, wid) = app_with_strip();
    app.input(wid, key(ENamed::ContextMenu, Modifiers::empty()), CTL);
    let first = app.windows[&wid].tab_menu.as_ref().unwrap().highlight;
    app.input(wid, key(ENamed::ArrowDown, Modifiers::empty()), CTL);
    let stepped = app.windows[&wid].tab_menu.as_ref().unwrap().highlight;
    assert_ne!(first, stepped, "↓ moved the highlight");
    app.input(wid, key(ENamed::Escape, Modifiers::empty()), CTL);
    assert!(app.windows[&wid].tab_menu.is_none(), "Esc dismissed it");
}

#[test]
fn a_wheel_notch_under_the_card_is_swallowed() {
    use aterm_window::event::MouseScrollDelta;

    let (mut app, wid) = app_with_strip();
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut t = crate::term_lock(&term);
        for line in 0..100 {
            t.process(format!("history {line}\r\n").as_bytes());
        }
        assert!(t.grid().scrollback_lines() > 0);
        assert_eq!(t.grid().display_offset(), 0);
    }

    app.input(wid, key(ENamed::ContextMenu, Modifiers::empty()), CTL);
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    assert!(
        app.windows[&wid].tab_menu.is_some(),
        "the notch neither dismissed the card"
    );
    assert_eq!(
        crate::term_lock(&term).grid().display_offset(),
        0,
        "…nor scrolled the viewport underneath it"
    );
    assert!(
        app.windows[&wid].scroll_glide.is_none(),
        "and it armed no smooth-scroll glide either"
    );

    app.input(wid, key(ENamed::Escape, Modifiers::empty()), CTL);
    assert!(app.windows[&wid].tab_menu.is_none());
    app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
    assert!(
        app.windows[&wid].scroll_glide.is_some()
            || crate::term_lock(&term).grid().display_offset() > 0
    );
}

#[test]
fn a_controller_press_under_the_card_dismisses_and_acts_on_nothing() {
    use aterm_types::mouse::MouseButton;

    let (mut app, wid) = app_with_strip();
    let press = |pressed| InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed,
        row: 4,
        col: 4,
        mods: 0,
        click_count: 1,
        side: aterm_core::selection::SelectionSide::Left,
        block: false,
        suppress_copy_on_select: false,
        px_off: crate::input::PixelOffset::CELL_ORIGIN,
    };

    app.input(wid, key(ENamed::ContextMenu, Modifiers::empty()), CTL);
    assert!(app.windows[&wid].tab_menu.is_some());
    app.input(wid, press(true), CTL);
    assert!(
        app.windows[&wid].tab_menu.is_none(),
        "the press dismissed the card"
    );
    assert!(
        !app.windows[&wid].selecting,
        "…and did NOT also start a selection underneath it — click-away \
         costs a click, on this route exactly as on the glass"
    );

    app.input(wid, press(true), CTL);
    assert!(
        app.windows[&wid].selecting,
        "the same press starts a selection once the card is gone"
    );
    app.input(wid, press(false), CTL);
}
