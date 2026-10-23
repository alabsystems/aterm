// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! X11 tests for `redraw_is_exposure` on server `Expose` events.

use std::time::{Duration, Instant};

use aterm_x11::proto::xproto::{self, image_format};
use aterm_x11::proto::ValueList;
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

/// Pump `event_loop` once.
fn pump(event_loop: &mut EventLoop<()>, redraws: &mut Vec<bool>, timeout: Duration) {
    event_loop.pump_events(Some(timeout), |event, _| {
        if let Event::WindowEvent { event: WindowEvent::RedrawRequested, .. } = event {
            redraws.push(redraw_is_exposure());
        }
    });
}

/// A hidden window on `$DISPLAY` with its loop pumped dry, and a stand-in client's connection.
fn hidden_window() -> (EventLoop<()>, Window, Connection) {
    let mut event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let window = {
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        Window::new(target, WindowAttributes::default().with_visible(false)).expect("an X11 window")
    };
    let (stand_in, _) =
        Connection::connect(None).expect("DISPLAY is set: the connection must open");
    window.xconn.sync_with_server().expect("the window's round trip");
    pump(&mut event_loop, &mut Vec::new(), Duration::ZERO);
    (event_loop, window, stand_in)
}

/// Has `stand_in` send `xwindow` a 1x1 `Expose` at (`x`, `y`), and returns once the server sent it.
fn expose(stand_in: &Connection, xwindow: u32, (x, y): (u16, u16)) {
    let mut event = [0u8; 32];
    event[0] = EXPOSE;
    event[4..8].copy_from_slice(&xwindow.to_ne_bytes());
    event[8..10].copy_from_slice(&x.to_ne_bytes());
    event[10..12].copy_from_slice(&y.to_ne_bytes());
    event[12..14].copy_from_slice(&1u16.to_ne_bytes());
    event[14..16].copy_from_slice(&1u16.to_ne_bytes());
    stand_in.send_void(xproto::send_event(false, xwindow, 0, &event)).unwrap();
    stand_in.send(xproto::get_input_focus()).unwrap().reply().expect("the stand-in's sync");
}

/// Pumps `event_loop` until `redraws` holds `count`.
fn until(event_loop: &mut EventLoop<()>, redraws: &mut Vec<bool>, count: usize, what: &str) {
    let deadline = Instant::now() + HANG;
    while redraws.len() < count {
        assert!(Instant::now() < deadline, "{what} never came in {HANG:?}: {redraws:?}");
        pump(event_loop, redraws, Duration::from_millis(5));
    }
}

#[test]
fn a_redraw_the_server_asked_for_is_dispatched_as_an_exposure() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (mut event_loop, window, stand_in) = hidden_window();
    let xwindow = window.id().0 as u32;
    let mut redraws = Vec::new();

    window.request_redraw();
    until(&mut event_loop, &mut redraws, 1, "the application's redraw");
    assert_eq!(redraws, [false], "the application's redraw is not an exposure");

    expose(&stand_in, xwindow, (0, 0));
    until(&mut event_loop, &mut redraws, 2, "the Expose's redraw");
    assert_eq!(redraws, [false, true], "the Expose's redraw is one");

    expose(&stand_in, xwindow, (0, 0));
    window.xconn.sync_with_server().expect("the window's round trip");
    window.request_redraw();
    until(&mut event_loop, &mut redraws, 3, "the redraw both asked for");
    assert_eq!(redraws, [false, true, true], "one redraw, which the Expose asked for");

    window.request_redraw();
    until(&mut event_loop, &mut redraws, 4, "the application's next redraw");
    assert_eq!(redraws, [false, true, true, false], "the exposure does not outlast its redraw");
    assert!(!redraw_is_exposure(), "nothing is an exposure outside a dispatch");
}

#[test]
fn an_expose_a_later_post_drew_again_is_no_exposure() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (mut event_loop, window, stand_in) = hidden_window();
    let xwindow = window.id().0 as u32;
    let conn = window.xconn.xcb_connection();
    let depth = conn.send(xproto::get_geometry(xwindow)).unwrap().reply().unwrap().depth;
    let gc = conn.generate_id().unwrap();
    conn.send_void(xproto::create_gc(gc, xwindow, &ValueList::new())).unwrap().check().unwrap();
    // A 2x2 `PutImage` at the origin, noted as the window's post of that area.
    let post = || {
        let image =
            xproto::put_image(image_format::Z_PIXMAP, xwindow, gc, 2, 2, 0, 0, 0, depth, &[0; 16]);
        let sequence = conn.send_void(image).unwrap().sequence();
        window.area_posted(sequence, [0, 0, 2, 2]);
    };
    let mut redraws = Vec::new();

    expose(&stand_in, xwindow, (1, 1));
    post();
    until(&mut event_loop, &mut redraws, 1, "the Expose's redraw");
    assert_eq!(redraws, [false], "the post the server processed after the Expose drew it again");

    post();
    window.xconn.sync_with_server().expect("the window's round trip");
    expose(&stand_in, xwindow, (1, 1));
    until(&mut event_loop, &mut redraws, 2, "the Expose's redraw");
    assert_eq!(redraws, [false, true], "a post the server processed first draws nothing again");

    expose(&stand_in, xwindow, (2, 0));
    post();
    until(&mut event_loop, &mut redraws, 3, "the Expose's redraw");
    assert_eq!(redraws, [false, true, true], "a later post beside the exposed area leaves it owed");
}
