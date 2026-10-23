// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::prompt::fixtures::{
    LOGIN_CONSOLE_MENU, LOGIN_ERROR, LOGIN_EXPIRED, LOGIN_HANDOFF, LOGIN_INTERRUPTED,
    LOGIN_OPENING, LOGIN_PICKER, LOGIN_PICKER_CONSOLE, LOGIN_PICKER_WARNED, LOGIN_RELOOPED,
    LOGIN_SUCCESS, screen,
};

#[test]
fn every_measured_login_screen_is_read_as_its_state() {
    let picker = |text| match login_screen(&screen(text)) {
        Some(LoginScreen::Picker(p)) => p,
        other => panic!("{other:?}"),
    };
    let all = vec![
        LoginMethod::Subscription,
        LoginMethod::Console,
        LoginMethod::Platform,
    ];
    for text in [LOGIN_PICKER, LOGIN_PICKER_WARNED, LOGIN_RELOOPED] {
        let p = picker(text);
        assert_eq!(p.methods, all);
        assert_eq!(p.focused, Some(0));
        assert_eq!(p.title_row.trim(), "Login");
    }
    let console = picker(LOGIN_PICKER_CONSOLE);
    assert_eq!(
        console.focused.map(|i| console.methods[i]),
        Some(LoginMethod::Console)
    );
    assert_eq!(
        login_screen(&screen(LOGIN_CONSOLE_MENU)),
        Some(LoginScreen::ConsoleMenu)
    );
    for text in [LOGIN_OPENING, LOGIN_HANDOFF] {
        assert_eq!(login_screen(&screen(text)), Some(LoginScreen::Browser));
    }
    for id in ["login.no_browser", "login.paste"] {
        assert!(!LOGIN_OPENING.contains(crate::anchors::anchor(id)), "{id}");
    }
    assert!(LOGIN_OPENING.contains(crate::anchors::anchor("login.opening")));
    assert_eq!(
        login_screen(&screen(LOGIN_ERROR)),
        Some(LoginScreen::Failed)
    );
    assert!(matches!(
        login_screen(&screen(LOGIN_SUCCESS)),
        Some(LoginScreen::Success { row }) if row.contains("Press Enter to continue")
    ));
}

#[test]
fn a_screen_with_a_composer_is_no_login_dialog() {
    for text in [LOGIN_INTERRUPTED, LOGIN_EXPIRED] {
        assert_eq!(login_screen(&screen(text)), None);
    }
}

#[test]
fn esc_out_of_the_dialog_is_read_as_the_login_interrupted() {
    assert!(login_interrupted(&screen(LOGIN_INTERRUPTED)));
    for text in [LOGIN_EXPIRED, LOGIN_PICKER, LOGIN_HANDOFF] {
        assert!(!login_interrupted(&screen(text)));
    }
}

#[test]
fn a_login_typed_at_a_lost_login_is_read_as_the_walls() {
    let at_wall = |rows: &[String]| login_at_wall(rows);
    for text in [
        LOGIN_PICKER_WARNED,
        LOGIN_OPENING,
        LOGIN_HANDOFF,
        LOGIN_ERROR,
        LOGIN_SUCCESS,
        LOGIN_RELOOPED,
    ] {
        let got = at_wall(&screen(text)).unwrap_or_else(|| panic!("{text}"));
        assert!(got.message.contains("Not logged in"), "{got:?}");
        assert!(!got.left_before, "{text}");
    }
    let handoff = screen(LOGIN_HANDOFF);
    let typed = handoff
        .iter()
        .position(|r| r.trim_end() == "❯ /login")
        .expect("the /login row");
    let mut left: Vec<String> = handoff[..=typed].to_vec();
    left.extend(
        [
            "  ⎿  Login interrupted",
            "",
            &"─".repeat(144),
            "❯ ",
            &"─".repeat(144),
        ]
        .map(str::to_string),
    );
    assert_eq!(
        at_wall(&left).map(|w| w.left_before),
        Some(false),
        "{left:#?}"
    );
    let mut again: Vec<String> = handoff[..=typed].to_vec();
    again.push("  ⎿  Login interrupted".to_string());
    again.push(String::new());
    again.extend(handoff[typed..].iter().cloned());
    assert_eq!(at_wall(&again).map(|w| w.left_before), Some(true));
    for text in [
        LOGIN_INTERRUPTED,
        LOGIN_PICKER,
        LOGIN_CONSOLE_MENU,
        LOGIN_PICKER_CONSOLE,
        LOGIN_EXPIRED,
    ] {
        assert_eq!(at_wall(&screen(text)), None, "{text}");
    }
}
