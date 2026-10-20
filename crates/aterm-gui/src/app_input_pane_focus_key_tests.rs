// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Alt+arrow is pane focus only while focus can move: in an unsplit tab, or at
//! a split's edge, the program gets its `ESC[1;3<A-D>` (Emacs word motion,
//! fish's directory history, tmux's resize), as Windows Terminal hands the key
//! back when no pane moved.

use super::typed_kitty_summon_tests::{app_with_private_pty, drain};
use crate::{App, WindowId, pane};
use aterm_window::event::{ElementState, KeyEvent};
use aterm_window::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

/// A press and release of an arrow key, Alt held.
fn alt_arrow(app: &mut App, wid: WindowId, key: NamedKey, code: KeyCode) {
    app.windows.get_mut(&wid).unwrap().mods = ModifiersState::ALT;
    for state in [ElementState::Pressed, ElementState::Released] {
        let ev = KeyEvent::synthetic_for_test(
            PhysicalKey::Code(code),
            Key::Named(key),
            None,
            KeyLocation::Standard,
            state,
            false,
        );
        app.on_key(wid, ev);
    }
}

#[test]
fn alt_arrow_moves_pane_focus_or_reaches_the_program() {
    let (mut app, _sink, top_pipe) = app_with_private_pty();
    let wid = WindowId(0);
    // The seeded table (non-macOS), spelled out so macOS runs the same rule.
    app.keybindings = crate::keybinding::Keybindings::resolved(Some(
        &[
            ("alt+left", "focus_pane_left"),
            ("alt+up", "focus_pane_up"),
            ("alt+down", "focus_pane_down"),
        ]
        .into_iter()
        .map(|(c, a)| (c.to_string(), a.to_string()))
        .collect(),
    ));
    let top = app.focused_session_id(wid).expect("front terminal");

    alt_arrow(&mut app, wid, NamedKey::ArrowLeft, KeyCode::ArrowLeft);
    assert_eq!(drain(top_pipe), b"\x1b[1;3D", "unsplit: the program's key");

    let (bottom_pipe, bottom_sink) = {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        assert_eq!(
            unsafe { libc::fcntl(pipe[0], libc::F_SETFL, libc::O_NONBLOCK) },
            0
        );
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(pipe[1]));
        (pipe, sink)
    };
    let bottom =
        app.split_active_stub_tab_dir_with_sink(wid, pane::SplitDir::Horizontal, bottom_sink);
    assert_eq!(app.focused_session_id(wid), Some(bottom));
    let _ = drain(top_pipe);

    alt_arrow(&mut app, wid, NamedKey::ArrowUp, KeyCode::ArrowUp);
    assert_eq!(app.focused_session_id(wid), Some(top), "Alt+Up moved focus");
    assert_eq!(drain(top_pipe), b"", "and wrote nothing");
    assert_eq!(drain(bottom_pipe), b"");

    alt_arrow(&mut app, wid, NamedKey::ArrowUp, KeyCode::ArrowUp);
    assert_eq!(app.focused_session_id(wid), Some(top));
    assert_eq!(
        drain(top_pipe),
        b"\x1b[1;3A",
        "the top edge: the program's key"
    );

    alt_arrow(&mut app, wid, NamedKey::ArrowDown, KeyCode::ArrowDown);
    assert_eq!(app.focused_session_id(wid), Some(bottom), "and back down");
    assert_eq!(drain(top_pipe), b"");
    assert_eq!(drain(bottom_pipe), b"");
    for pipe in [top_pipe, bottom_pipe] {
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
    }
}
