// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A shown X11 window that another client activates with a startup id asks the window manager
//! for focus at the user action the id stamps, and ends the launch sequence the id names. GNOME
//! Shell 46 sends such an id (`ActivationToken`) before it reports a click on a notification,
//! and made it by starting a launch sequence for the click, for which mutter shows a busy cursor
//! until the launchee's `remove` message or a timeout. A second client watching the root's
//! substructure and property changes, as a window manager and a launch monitor do, receives the
//! window's `_NET_ACTIVE_WINDOW` request with the id's `_TIME` as its timestamp, and the `remove`
//! message for the id in a `_NET_STARTUP_INFO_BEGIN` chunk and its `_NET_STARTUP_INFO`
//! continuations. The negative control is the plain raise (`focus_window`), which the watcher
//! sees at `CURRENT_TIME` and with no `remove`.
//!
//! No timing decides it. Each step ends in a round trip on the window's own connection, so the
//! server has sent the watcher its messages before it answers the watcher's round trip that
//! follows, and the watcher reads what it was sent up to that answer.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use aterm_x11::proto::event::{ClientMessage, Event};
use aterm_x11::proto::xproto::{self, cw, event_mask};
use aterm_x11::proto::ValueList;
use aterm_x11::Connection;

use super::window::Visibility;
use super::Window;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// A startup id as GNOME Shell's launch context makes one (libstartup-notification's form).
const TOKEN: &str = "gnome-shell/aterm/4242-1-host_TIME5678";

/// The client messages the server sent `watcher` from `window`, up to the answer to a round trip
/// of the watcher's own that follows one of the window's.
fn messages_from(watcher: &Connection, window: &Window) -> Vec<ClientMessage> {
    window.xconn.sync_with_server().expect("the window's requests reach the server");
    watcher.send(xproto::get_input_focus()).unwrap().reply().expect("GetInputFocus");
    let xwindow = window.id().0 as u32;
    let mut seen = Vec::new();
    while let Some(packet) = watcher.poll_for_event().expect("the watcher's connection") {
        if let Ok(Event::ClientMessage(m)) = Event::parse(&packet) {
            if m.window == xwindow {
                seen.push(m);
            }
        }
    }
    seen
}

#[test]
fn an_activation_token_stamps_the_raise_and_ends_its_launch() {
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
            &ValueList::new()
                .set(cw::EVENT_MASK, event_mask::SUBSTRUCTURE_NOTIFY | event_mask::PROPERTY_CHANGE),
        ))
        .unwrap()
        .check()
        .expect("SubstructureNotify and PropertyChange on the root");
    let [active, begin, more] = watcher
        .intern_atoms(&[b"_NET_ACTIVE_WINDOW", b"_NET_STARTUP_INFO_BEGIN", b"_NET_STARTUP_INFO"])
        .unwrap()[..]
    else {
        unreachable!()
    };
    // Each `_NET_ACTIVE_WINDOW` request as its source indication and timestamp.
    let raises = |seen: &[ClientMessage]| -> Vec<[u32; 2]> {
        seen.iter()
            .filter(|m| m.type_ == active)
            .map(|m| {
                let [source, time, ..] = m.data32();
                [source, time]
            })
            .collect()
    };
    // The launch message the chunks spell, up to its NUL.
    let launch = |seen: &[ClientMessage]| -> Vec<u8> {
        let mut text = Vec::new();
        for (i, m) in seen.iter().filter(|m| m.type_ == begin || m.type_ == more).enumerate() {
            assert_eq!(m.type_ == begin, i == 0, "a BEGIN chunk, then its continuations");
            assert_eq!(m.format, 8, "a launch message is format 8");
            text.extend_from_slice(&m.data);
        }
        text.truncate(text.iter().position(|&b| b == 0).unwrap_or(text.len()));
        text
    };

    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let window = Window::new(target, WindowAttributes::default()).expect("an X11 window");
    let _creation = messages_from(&watcher, &window);
    // Shown, as the loop marks a window when its map is reported; the test pumps no loop.
    window.shared_state_lock().visibility = Visibility::Yes;

    window.focus_window();
    let plain = messages_from(&watcher, &window);
    assert_eq!(raises(&plain), [[1, 0]], "the plain raise: an application's, at CURRENT_TIME");
    assert_eq!(launch(&plain), b"", "it ends no launch");

    window.focus_window_with_activation_token(TOKEN);
    let activated = messages_from(&watcher, &window);
    assert_eq!(raises(&activated), [[1, 5678]], "the activated raise, at the token's _TIME");
    assert_eq!(
        String::from_utf8_lossy(&launch(&activated)),
        format!("remove: ID=\"{TOKEN}\""),
        "and the launch the token names, ended"
    );
}
