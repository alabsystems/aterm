// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A new X11 window clears keep-above and keep-below once it is mapped, live. winit's
//! creation-time `set_window_level(WindowLevel::Normal)` sent the root two `_NET_WM_STATE`
//! REMOVE messages, `_NET_WM_STATE_ABOVE` and then `_NET_WM_STATE_BELOW`, for every window. WC.8
//! left the setter to Windows and keeps the two messages
//! ([`super::window::UnownedWindow`]'s `clear_window_level`), so a window manager's keep-above
//! or keep-below rule applied at the map is still cancelled. A second client watching the root's
//! substructure, as a window manager does, receives both, in that order. The negative control is
//! the call removed from `UnownedWindow::new`: the watcher then sees neither.
//!
//! No timing decides it. The window's creation ends in a round trip on its own connection, so
//! the server has sent both messages before it answers the watcher's round trip that follows,
//! and the watcher reads what it was sent up to that answer.
//!
//! It runs where `$DISPLAY` names an X server (the Linux lane's Xvfb); elsewhere it says SKIP and
//! returns.

use aterm_x11::proto::event::Event;
use aterm_x11::proto::xproto::{self, cw, event_mask};
use aterm_x11::proto::ValueList;
use aterm_x11::Connection;

use super::util::StateOperation;
use super::Window;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// The `_NET_WM_STATE` messages the server sent `watcher` about `xwindow`, as (action, first
/// property), up to the answer to a round trip of the watcher's own.
fn state_messages(watcher: &Connection, state: u32, xwindow: u32) -> Vec<(u32, u32)> {
    watcher.send(xproto::get_input_focus()).unwrap().reply().expect("GetInputFocus");
    let mut seen = Vec::new();
    while let Some(packet) = watcher.poll_for_event().expect("the watcher's connection") {
        if let Ok(Event::ClientMessage(m)) = Event::parse(&packet) {
            if m.window == xwindow && m.type_ == state {
                assert_eq!(m.format, 32, "_NET_WM_STATE is a format-32 message");
                let item =
                    |i: usize| u32::from_ne_bytes(m.data[i * 4..i * 4 + 4].try_into().unwrap());
                seen.push((item(0), item(1)));
            }
        }
    }
    seen
}

#[test]
fn a_new_window_clears_keep_above_and_keep_below_after_its_map() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (watcher, screen) =
        Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let root = watcher.setup().roots[screen].root;
    watcher
        .send_void(xproto::change_window_attributes(
            root,
            &ValueList::new().set(cw::EVENT_MASK, event_mask::SUBSTRUCTURE_NOTIFY),
        ))
        .unwrap()
        .check()
        .expect("SubstructureNotify on the root");
    let [state, above, below] = watcher
        .intern_atoms(&[b"_NET_WM_STATE", b"_NET_WM_STATE_ABOVE", b"_NET_WM_STATE_BELOW"])
        .unwrap()[..]
    else {
        unreachable!()
    };

    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let window = Window::new(target, WindowAttributes::default()).expect("an X11 window");
    let xwindow = window.id().0 as u32;

    let remove = StateOperation::Remove as u32;
    assert_eq!(
        state_messages(&watcher, state, xwindow),
        [(remove, above), (remove, below)],
        "the window's _NET_WM_STATE messages to the root, after its map"
    );
}
