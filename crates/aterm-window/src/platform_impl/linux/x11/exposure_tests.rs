// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A redraw the X server asked for is dispatched as one. The server sends `Expose` when it holds
//! nothing of what part of a window showed (a VT switch, NVIDIA's video-memory purge, a
//! compositor's unredirection); upstream turned it into the same `RedrawRequested` as the
//! application's own requests, and aterm's redraw path skips a frame whose content did not
//! change, so the window stayed as the server left it until a cell moved (the desk audit, GNOME
//! 46 on Xorg, 2026-10-22). Here `platform::x11::redraw_is_exposure` is `true` while the redraw
//! an Expose asked for is dispatched, also when the application asked for the window's redraw in
//! the same loop turn (one event carries both), and `false` for the application's redraw alone
//! and outside the dispatch.
//!
//! The Expose comes from a stand-in on its own connection (SendEvent to the hidden window, whose
//! creator receives it). The negative controls: without the mark in `EventProcessor::expose`,
//! or with the redraw loop of `single_iteration` dispatching every redraw plainly, the Expose's
//! redraw reads `false`.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use std::time::{Duration, Instant};

use aterm_x11::proto::xproto;
use aterm_x11::Connection;

use super::Window;
use crate::event::{Event, WindowEvent};
use crate::platform::x11::redraw_is_exposure;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// The hang detector: how long a wait for what must happen may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

/// `Expose`'s event code.
const EXPOSE: u8 = 12;

/// Pump `event_loop` once, for at most `timeout`, noting whether each `RedrawRequested` was
/// dispatched as an exposure.
fn pump(event_loop: &mut EventLoop<()>, redraws: &mut Vec<bool>, timeout: Duration) {
    event_loop.pump_events(Some(timeout), |event, _| {
        if let Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } = event {
            redraws.push(redraw_is_exposure());
        }
    });
}

#[test]
fn a_redraw_the_server_asked_for_is_dispatched_as_an_exposure() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let mut event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let window = {
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        Window::new(target, WindowAttributes::default().with_visible(false)).expect("an X11 window")
    };
    let xwindow = window.id().0 as u32;
    let (stand_in, _) =
        Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let expose = || {
        let mut event = [0u8; 32];
        event[0] = EXPOSE;
        event[4..8].copy_from_slice(&xwindow.to_ne_bytes());
        event[12..14].copy_from_slice(&1u16.to_ne_bytes());
        event[14..16].copy_from_slice(&1u16.to_ne_bytes());
        stand_in.send_void(xproto::send_event(false, xwindow, 0, &event)).unwrap();
        stand_in.send(xproto::get_input_focus()).unwrap().reply().expect("the stand-in's sync");
    };
    let mut redraws = Vec::new();
    // Whatever the window's creation asked for.
    window.xconn.sync_with_server().expect("the window's round trip");
    pump(&mut event_loop, &mut redraws, Duration::ZERO);
    redraws.clear();

    let mut until = |redraws: &mut Vec<bool>, count: usize, what: &str| {
        let deadline = Instant::now() + HANG;
        while redraws.len() < count {
            assert!(Instant::now() < deadline, "{what} never came in {HANG:?}: {redraws:?}");
            pump(&mut event_loop, redraws, Duration::from_millis(5));
        }
    };

    window.request_redraw();
    until(&mut redraws, 1, "the application's redraw");
    assert_eq!(redraws, [false], "the application's redraw is not an exposure");

    expose();
    until(&mut redraws, 2, "the Expose's redraw");
    assert_eq!(redraws, [false, true], "the Expose's redraw is one");

    // Both in one loop turn: the Expose queued before the loop looks, the application's
    // request beside it.
    expose();
    window.xconn.sync_with_server().expect("the window's round trip");
    window.request_redraw();
    until(&mut redraws, 3, "the redraw both asked for");
    assert_eq!(redraws, [false, true, true], "one redraw, which the Expose asked for");

    window.request_redraw();
    until(&mut redraws, 4, "the application's next redraw");
    assert_eq!(redraws, [false, true, true, false], "the exposure does not outlast its redraw");
    assert!(!redraw_is_exposure(), "nothing is an exposure outside a dispatch");
}
