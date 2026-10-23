// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! X11 tests that a position given to a window before its first map is kept via `USPosition`.

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
