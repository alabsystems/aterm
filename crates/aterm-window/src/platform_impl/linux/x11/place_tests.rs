// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A position given to a window before its first map is where it appears. An X11 window manager
//! places a new window itself unless its normal hints say its position was chosen (ICCCM
//! 4.1.2.3, USPosition or PPosition); mutter (GNOME 46 on Xorg) mapped a window moved while
//! unmapped at its own first-fit spot, so aterm's restored first window appeared there and
//! jumped to its saved place a frame later (the desk audit, 2026-10-22). The test moves a hidden
//! window, reads the hint the server holds, shows the window and, where a window manager runs,
//! waits for it to frame the window and for the frame to stand where the window was put (the
//! client's place on the root less `_NET_FRAME_EXTENTS`' left and top). The geometry is the
//! server's: the window's own `outer_position` reads frame extents it caches from events, and
//! nothing pumps this loop. With no window manager the window stays where it was moved, which
//! the test reads once it is mapped.
//!
//! The negative control: without the hint in `set_outer_position`, the flag reads clear, and
//! under mutter (with that assertion also taken out) the frame stands at mutter's spot, so the
//! wait for its place ends the test.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use std::time::{Duration, Instant};

use aterm_x11::proto::{atom, xproto};
use aterm_x11::Connection;

use super::Window;
use crate::dpi::PhysicalPosition;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// The hang detector: how long a wait for what must happen may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

/// `WM_SIZE_HINTS`' `USPosition` flag (ICCCM 4.1.2.3).
const US_POSITION: u32 = 1;

/// The 32-bit values of `window`'s `property`, empty where it has none.
fn cardinals(conn: &Connection, window: u32, property: u32) -> Vec<u32> {
    conn.send(xproto::get_property(false, window, property, atom::ANY, 0, 64))
        .unwrap()
        .reply()
        .expect("GetProperty")
        .value32()
        .unwrap_or_default()
}

#[test]
fn a_window_moved_before_its_map_appears_where_it_was_put() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (watcher, screen) =
        Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let root = watcher.setup().roots[screen].root;
    let [normal_hints, frame_extents, wm_check] = watcher
        .intern_atoms(&[b"WM_NORMAL_HINTS", b"_NET_FRAME_EXTENTS", b"_NET_SUPPORTING_WM_CHECK"])
        .unwrap()[..]
    else {
        unreachable!()
    };
    let managed = !cardinals(&watcher, root, wm_check).is_empty();

    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let window = Window::new(target, WindowAttributes::default().with_visible(false))
        .expect("an X11 window");
    let xwindow = window.id().0 as u32;
    let place = PhysicalPosition::new(237, 151);
    window.set_outer_position(place.into());
    window.xconn.sync_with_server().expect("the window's requests reach the server");
    let flags = cardinals(&watcher, xwindow, normal_hints).first().copied().unwrap_or(0);
    assert_ne!(flags & US_POSITION, 0, "a hidden window's position is in its hints: {flags:#x}");

    window.set_visible(true);
    window.xconn.sync_with_server().expect("the map reaches the server");
    let deadline = Instant::now() + HANG;
    loop {
        let parent =
            watcher.send(xproto::query_tree(xwindow)).unwrap().reply().expect("QueryTree").parent;
        let extents = cardinals(&watcher, xwindow, frame_extents);
        let at = watcher
            .send(xproto::translate_coordinates(xwindow, root, 0, 0))
            .unwrap()
            .reply()
            .expect("TranslateCoordinates");
        let (x, y) = (i32::from(at.dst_x), i32::from(at.dst_y));
        let frame = match (managed, &extents[..]) {
            (false, _) => Some(PhysicalPosition::new(x, y)),
            (true, &[left, _, top, _]) if parent != root => {
                Some(PhysicalPosition::new(x - left as i32, y - top as i32))
            },
            (true, _) => None,
        };
        if frame == Some(place) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the window's frame stands at {frame:?} (client at {x},{y}, extents {extents:?}, \
             parent {parent:#x}), not where it was put"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
