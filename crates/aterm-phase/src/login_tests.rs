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
    // The first three seconds draw no URL and no paste field: the hand-off is
    // read by its opening row alone.
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
