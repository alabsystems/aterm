// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A request for attention sets an unfocused X11 window's urgency hint (`WM_HINTS`' 0x100), the
//! window's focus clears it, and a request while it has focus leaves it clear.

use aterm_x11::proto::xproto;
use aterm_x11::Connection;

use super::Window;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::{UserAttentionType, WindowAttributes};

/// `WM_HINTS`' `XUrgencyHint` flag (ICCCM 4.1.2.4).
const URGENCY: u32 = 1 << 8;

/// Whether `xwindow`'s `WM_HINTS`, as the server holds it after a round trip of the window's own
/// connection, has the urgency flag.
fn urgent(watcher: &Connection, wm_hints: u32, window: &Window) -> bool {
    window.xconn.sync_with_server().expect("the window's requests reach the server");
    let reply = watcher
        .send(xproto::get_property(false, window.id().0 as u32, wm_hints, wm_hints, 0, 9))
        .unwrap()
        .reply()
        .expect("GetProperty WM_HINTS");
    reply.value32().and_then(|v| v.first().copied()).is_some_and(|flags| flags & URGENCY != 0)
}

#[test]
fn attention_is_cleared_by_focus_and_never_asked_while_focused() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (watcher, _) = Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let [wm_hints] = watcher.intern_atoms(&[b"WM_HINTS"]).unwrap()[..] else { unreachable!() };

    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let window = Window::new(target, WindowAttributes::default()).expect("an X11 window");
    assert!(!urgent(&watcher, wm_hints, &window), "a new window asks for nothing");

    window.request_user_attention(Some(UserAttentionType::Informational));
    assert!(urgent(&watcher, wm_hints, &window), "an unfocused window's request sets the hint");

    window.shared_state_lock().has_focus = true;
    window.focused();
    assert!(!urgent(&watcher, wm_hints, &window), "the window's focus clears it");

    window.request_user_attention(Some(UserAttentionType::Critical));
    assert!(!urgent(&watcher, wm_hints, &window), "a focused window's request leaves it clear");
}
