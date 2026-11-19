// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn prompt(term: &mut Terminal) {
    let key = [0x61; 32];
    let nonce = aterm_core::terminal::ShellIntegrationNonce(key).to_hex();
    term.authorize_shell_integration(key);
    term.set_require_shell_integration_nonce(true);
    term.process(
        format!("\x1b]133;A;id={nonce}\x07$ \x1b]133;B;id={nonce};aterm_input=start\x07")
            .as_bytes(),
    );
}

#[test]
fn a_prompt_must_be_signed_and_its_entire_input_region_empty() {
    let mut unsigned = Terminal::new(3, 32);
    unsigned.process(b"\x1b]133;A\x07$ \x1b]133;B;aterm_input=start\x07");
    assert!(!empty_shell_prompt(&unsigned));
    let mut term = Terminal::new(3, 32);
    prompt(&mut term);
    assert!(empty_shell_prompt(&term));
    term.process(b"unfinished\x1b[10D");
    assert!(
        !empty_shell_prompt(&term),
        "Home leaves a draft to the right"
    );
    term.process(b"\x1b[K");
    assert!(empty_shell_prompt(&term));
    term.process(b"\r\nmore\x1b[1A\r\x1b[2C");
    assert!(
        !empty_shell_prompt(&term),
        "a multiline draft survives Home"
    );
    term.process(b"\x1b[?1049h");
    assert!(
        !empty_shell_prompt(&term),
        "a full-screen program stays open"
    );
}

#[test]
fn cleanup_closes_only_idle_unprotected_shells_and_keeps_each_windows_selection() {
    let mut app = App::headless_for_test();
    let first = WindowId(0);
    // Shells 0 and 7 are idle. Shell 1 has a job, 2 a pin, 3 a hold and 4 an unfinished line.
    for id in 1..=6 {
        app.push_stub_tab(first, crate::stub_session(id));
    }
    let second = app.insert_logical_window(crate::stub_session(7), 24, 80);
    app.push_stub_tab(second, crate::stub_session(8));
    for id in 0..=8 {
        let session = app.pool.get(id).unwrap();
        prompt(&mut crate::term_lock(&session.term));
    }
    app.pool.get(2).unwrap().ctx.meta.lock().unwrap().pinned = true;
    let held = &app.pool.get(3).unwrap().ctx;
    held.fabric
        .seed_holds(crate::fabric::parse_holds(["local mine"]), &held.timeline);
    crate::feed_session_for_test(&app, 4, b"draft\x1b[5D");
    // A worker in the process of launching can still be at its shell prompt.
    app.windows
        .get_mut(&first)
        .unwrap()
        .tab_set
        .tab_at_mut(5)
        .unwrap()
        .ephemeral = true;

    assert_eq!(
        app.clean_up_idle_tabs(),
        0,
        "an unreadable PTY is never idle"
    );
    let foregrounds = [first, second].map(|w| app.windows[&w].tab_set.active_id());
    assert_eq!(app.clean_up_idle_tabs_with(|session| session.id != 1), 2);
    assert!(app.pool.get(0).is_none());
    assert!(app.pool.get(7).is_none());
    for id in [1, 2, 3, 4, 5, 6, 8] {
        assert!(app.pool.get(id).is_some(), "session {id} should survive");
    }
    assert_eq!(
        [first, second].map(|w| app.windows[&w].tab_set.active_id()),
        foregrounds
    );
    assert_eq!(app.frontmost_window, Some(second));
    assert!(app.can_reopen_closed_tab());
    assert!(app.structural_invariants_ok());
    assert_eq!(app.clean_up_idle_tabs_with(|session| session.id != 1), 0);
}

#[test]
fn cleanup_in_tab_control_keeps_the_chip_behind_another_windows_preview() {
    let mut app = App::headless_for_test();
    let home = app.tab_home_window().unwrap();
    let background = app.focused_session_id(home).unwrap();
    let editing = 1;
    app.push_stub_tab(home, crate::stub_session(editing));
    let anchor = app.windows[&home].tab_set.active_id().unwrap();

    let borrowed = 2;
    let aux = app.insert_logical_window(crate::stub_session(borrowed), 24, 80);
    app.select_dock_session(home, borrowed);
    assert!(app.windows[&home].preview_on_chip(anchor).is_some());
    assert!(app.open_settings_tab_in_window(aux, crate::native_settings::SettingsRoute::Tabs));
    for id in [background, editing, borrowed] {
        prompt(&mut crate::term_lock(&app.pool.get(id).unwrap().term));
    }

    assert_eq!(app.clean_up_idle_tabs_with(|_| true), 1);
    assert!(app.pool.get(background).is_none());
    assert_eq!(app.focused_session_id(home), Some(borrowed));
    assert!(app.windows[&home].strip_index_of(anchor).is_some());
    assert!(app.dismiss_session_preview(home));
    assert_eq!(app.focused_session_id(home), Some(editing));
    assert_eq!(app.pool.views(borrowed), Some(1));
    assert!(app.structural_invariants_ok());
}
