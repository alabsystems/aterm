// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::SpawnAim;
use crate::{App, TabAction, WindowId};

fn two_windows() -> App {
    let mut app = App::headless_for_test();
    let front = app.insert_logical_window(crate::stub_session(1), 24, 80);
    assert_eq!(front, WindowId(1));
    assert_eq!(app.frontmost_window, Some(front));
    let back = WindowId(0);
    app.push_stub_tab(back, crate::stub_session(2));
    assert_eq!(app.windows[&back].tab_set.len(), 2);
    assert_eq!(app.windows[&back].tab_set.active_index(), Some(1));
    assert_eq!(
        app.frontmost_window,
        Some(front),
        "a background tab is not a focus change"
    );
    app
}

#[test]
fn a_named_or_hosting_window_resolves_and_an_unknown_one_is_named_back() {
    for headless in [true, false] {
        let mut app = two_windows();
        app.headless = headless;
        assert_eq!(app.frontmost_window, Some(WindowId(1)));
        assert_eq!(app.aimed_window(SpawnAim::Front), Ok(None));
        assert_eq!(app.aimed_window(SpawnAim::Window(0)), Ok(Some(WindowId(0))));
        assert_eq!(app.aimed_window(SpawnAim::Window(1)), Ok(Some(WindowId(1))));
        assert_eq!(
            app.aimed_window(SpawnAim::Window(7)),
            Err("no such window 7".to_string()),
            "headless={headless}"
        );
        assert_eq!(
            app.aimed_window(SpawnAim::Session(0)),
            Ok(Some(WindowId(0)))
        );
        assert_eq!(
            app.aimed_window(SpawnAim::Session(1)),
            Ok(Some(WindowId(1)))
        );
        assert_eq!(
            app.aimed_window(SpawnAim::Session(2)),
            Ok(Some(WindowId(0))),
            "headless={headless}"
        );
        assert_eq!(
            app.aimed_window(SpawnAim::Session(99)),
            Err("no window hosts the target session".to_string())
        );
    }
}

#[test]
fn aimed_tab_commands_drive_the_hosting_window_not_the_front_one() {
    let mut app = two_windows();
    app.headless = false;
    let (front, back) = (WindowId(1), WindowId(0));
    assert_eq!(app.apply_tab_cmd_in(back, TabAction::Next), Ok((0, 2)));
    assert_eq!(app.apply_tab_cmd_in(back, TabAction::Prev), Ok((1, 2)));
    assert_eq!(app.apply_tab_cmd_in(back, TabAction::Select(0)), Ok((0, 2)));
    assert_eq!(app.windows[&back].tab_set.active_index(), Some(0));
    assert_eq!(app.windows[&front].tab_set.len(), 1);
    assert_eq!(app.windows[&front].tab_set.active_index(), Some(0));
    assert_eq!(app.frontmost_window, Some(front));
    assert_eq!(app.apply_tab_cmd(TabAction::Next), Ok((0, 1)));
    assert_eq!(
        app.apply_tab_cmd_in(WindowId(9), TabAction::Next),
        Ok((0, 0))
    );
    assert!(app.structural_invariants_ok());
}

#[test]
fn aimed_tab_close_closes_in_the_hosting_window() {
    let mut app = two_windows();
    app.headless = false;
    let (front, back) = (WindowId(1), WindowId(0));
    assert_eq!(
        app.apply_tab_cmd_in(back, TabAction::Close(Some(1))),
        Ok((0, 1))
    );
    assert_eq!(app.windows[&back].tab_set.len(), 1);
    assert!(!app.windows[&back].pending_close);
    assert_eq!(app.windows[&front].tab_set.len(), 1);
    assert!(!app.windows[&front].pending_close);
    assert!(app.structural_invariants_ok());
}
