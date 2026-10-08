// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::terminal_emacs_search_direction;
#[cfg(unix)]
use crate::{App, PhysicalPressOwner, SearchRepeatAction, WindowId, term_lock};
use winit::keyboard::{Key, ModifiersState};
#[cfg(unix)]
use winit::keyboard::{KeyCode, PhysicalKey};

fn character(value: &str) -> Key {
    Key::Character(value.into())
}

/// Exhaust all 16 modifier subsets for both navigation keys (plus case and
/// a non-navigation negative control). Only the exact bare-Super subset
/// qualifies, and only where the hardcoded Cmd suite is live: on Linux
/// (`HARDCODED_SUPER_CHORDS` off) Super belongs to the desktop and every
/// subset falls through to the PTY.
#[test]
fn bare_super_s_r_classifier_is_exhaustive_over_modifier_power_set() {
    let flags = [
        ModifiersState::SHIFT,
        ModifiersState::CONTROL,
        ModifiersState::ALT,
        ModifiersState::SUPER,
    ];
    for mask in 0u8..16 {
        let mut mods = ModifiersState::empty();
        for (bit, flag) in flags.into_iter().enumerate() {
            if mask & (1 << bit) != 0 {
                mods |= flag;
            }
        }
        let expected = super::HARDCODED_SUPER_CHORDS && mods == ModifiersState::SUPER;
        assert_eq!(
            terminal_emacs_search_direction(&character("s"), mods),
            expected.then_some(true),
            "S mask={mask:04b}"
        );
        assert_eq!(
            terminal_emacs_search_direction(&character("R"), mods),
            expected.then_some(false),
            "R mask={mask:04b}"
        );
        assert_eq!(
            terminal_emacs_search_direction(&character("f"), mods),
            None,
            "Cmd-F keeps its legacy path, mask={mask:04b}"
        );
    }
}

#[cfg(unix)]
fn observe_pty(app: &mut App, wid: WindowId) -> [i32; 2] {
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let mut pipe = [0; 2];
    assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
    let flags = unsafe { libc::fcntl(pipe[0], libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(pipe[0], libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    app.windows
        .get_mut(&wid)
        .unwrap()
        .active_terminal
        .as_mut()
        .unwrap()
        .sink = Arc::new(SinkWriter::new(pipe[1]));
    pipe
}

/// Silence is EAGAIN on a live, empty observer pipe, and nothing else is. A
/// positive read is the regression these tests guard (bytes reached the
/// PTY). A zero read is EOF, which a pipe whose write end this test still
/// holds cannot answer: one of the observer's descriptors was closed out
/// from under the test (by another thread closing a number it no longer
/// owned), and it is reported as that rather than as emitted bytes.
#[cfg(unix)]
fn assert_pty_silent_and_close(pipe: [i32; 2]) {
    let mut bytes = [0u8; 64];
    let read = unsafe { libc::read(pipe[0], bytes.as_mut_ptr().cast(), bytes.len()) };
    let error = std::io::Error::last_os_error();
    assert_ne!(
        read, 0,
        "the PTY observer read EOF: a descriptor it owns was closed out from under \
         it, so silence was never observed"
    );
    assert!(
        read < 0,
        "host-owned search press/repeat/release emitted PTY bytes: {:?}",
        &bytes[..usize::try_from(read).unwrap_or(0)]
    );
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::WouldBlock,
        "the observer read failed with {error}, not EAGAIN"
    );
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
}

/// Normal-shell route through the shipping physical-owner seams: Cmd-S opens,
/// typing searches, a held key repeats locally, Cmd-R reverses and wraps, and both
/// releases are swallowed even with Kitty event-type reporting enabled.
#[cfg(unix)]
#[test]
fn normal_terminal_cmd_s_r_press_repeat_release_emit_zero_kitty_bytes() {
    let _serial = crate::control::search_cap_test_guard();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let pipe = observe_pty(&mut app, wid);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>2uhit one\r\nhit two");

    let cmd_s = PhysicalKey::Code(KeyCode::KeyS);
    app.terminal_emacs_search_pressed(wid, cmd_s, true);
    assert!(matches!(
        app.physical_press_owners.get(&cmd_s),
        Some(PhysicalPressOwner::Local { .. })
    ));
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("hit".into())),
    );
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 0);
    app.route_physical_repeat(wid, cmd_s);
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);
    app.release_physical_press(wid, cmd_s);
    assert!(!app.physical_press_owners.contains_key(&cmd_s));

    let cmd_r = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, cmd_r, false);
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 0);
    app.route_physical_repeat(wid, cmd_r);
    assert_eq!(
        app.windows[&wid].search.as_ref().unwrap().current,
        1,
        "backward repeat wraps first → last"
    );
    app.release_physical_press(wid, cmd_r);
    assert_pty_silent_and_close(pipe);
}

/// Codex's protected-footer mutation may force a search recompute, but the local
/// physical owner remains authoritative across the refresh: reverse direction and
/// last-match selection survive, while Kitty observes no press/repeat/release.
#[cfg(unix)]
#[test]
fn codex_footer_refresh_during_cmd_r_hold_is_directional_and_pty_silent() {
    let _serial = crate::control::search_cap_test_guard();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let rows = app.windows[&wid].rows;
    let pipe = observe_pty(&mut app, wid);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>2u");
    term_lock(&term)
        .process(format!("\x1b[{};1HCODEX_NEEDLE\x1b[{rows};1HCODEX_NEEDLE", rows - 1).as_bytes());

    let cmd_r = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, cmd_r, false);
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("CODEX_NEEDLE".into())),
    );
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);

    let region_bottom = rows - 2;
    term_lock(&term)
        .process(format!("\x1b[1;{region_bottom}r\x1b[{region_bottom};1H\r\nX\x1b[r").as_bytes());
    app.search_refresh_for_output(0);
    let refreshed = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(refreshed.current, refreshed.matches.len() - 1);
    app.route_physical_repeat(wid, cmd_r);
    app.release_physical_press(wid, cmd_r);
    assert_pty_silent_and_close(pipe);
}

/// Claude classic and alternate-screen layouts both remain behind the same host
/// intercept. Each initial Cmd-R query searches only the active grid, captures its
/// repeat locally, and retires without a Kitty byte.
#[cfg(unix)]
#[test]
fn claude_classic_and_alt_cmd_r_route_active_grid_and_emit_zero_kitty_bytes() {
    let _serial = crate::control::search_cap_test_guard();
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let pipe = observe_pty(&mut app, wid);
    let term = app.front_terminal(wid).expect("terminal").term.clone();
    term_lock(&term).process(b"\x1b[>2uCLAUDE_CLASSIC\r\nCLAUDE_CLASSIC");

    let classic_key = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, classic_key, false);
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert(
            "CLAUDE_CLASSIC".into(),
        )),
    );
    assert_eq!(app.windows[&wid].search.as_ref().unwrap().current, 1);
    app.release_physical_press(wid, classic_key);
    app.search_cancel();

    term_lock(&term).process(b"\x1b[?1049h\x1b[HCLAUDE_ALT\r\nCLAUDE_ALT");
    let alt_key = PhysicalKey::Code(KeyCode::KeyR);
    app.terminal_emacs_search_pressed(wid, alt_key, false);
    app.apply_search_repeat_action(
        wid,
        SearchRepeatAction::Edit(crate::app_search::SearchEdit::Insert("CLAUDE_ALT".into())),
    );
    let alternate = app.windows[&wid].search.as_ref().unwrap();
    assert_eq!(alternate.matches.len(), 2);
    assert_eq!(alternate.current, 1);
    app.route_physical_repeat(wid, alt_key);
    app.release_physical_press(wid, alt_key);
    assert_pty_silent_and_close(pipe);
}
