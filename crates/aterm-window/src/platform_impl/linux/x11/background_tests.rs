// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A window built with a background (`platform::x11::WindowAttributesExtX11::with_x11_background`)
//! shows it from its map, before anything draws, and keeps its contents at its top-left when it
//! grows (`NorthWestGravity`), the server painting only the area it gained, in the background
//! `set_x11_background` set last (`UnownedWindow::set_background` here). Upstream's window had no background: under a compositing
//! manager a new window, and the strips a resize added, showed whatever the window's new pixmap
//! held, on this desk's NVIDIA Xorg other windows' pixels (the desk audit, GNOME 46 on Xorg,
//! 2026-10-22). The negative controls: the background taken out of the window's creation, and the
//! first pixel reads what the pixmap held; the gravity taken out, and the old corner reads the new
//! background after the resize.
//!
//! The windowed test runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use aterm_x11::proto::xproto;
use aterm_x11::proto::ValueList;
use aterm_x11::Connection;

use super::window::background_pixel;
use super::Window;
use crate::platform::x11::WindowAttributesExtX11 as _;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// A screen whose one visual (id 0x21) is `class` with these channel masks.
fn screen(class: u8, masks: [u32; 3]) -> xproto::Screen {
    xproto::Screen {
        root: 1,
        default_colormap: 0,
        white_pixel: 0,
        black_pixel: 0,
        current_input_masks: 0,
        width_in_pixels: 0,
        height_in_pixels: 0,
        width_in_millimeters: 0,
        height_in_millimeters: 0,
        min_installed_maps: 0,
        max_installed_maps: 0,
        root_visual: 0x21,
        backing_stores: 0,
        save_unders: false,
        root_depth: 24,
        allowed_depths: vec![xproto::Depth {
            depth: 24,
            visuals: vec![xproto::Visualtype {
                visual_id: 0x21,
                class,
                bits_per_rgb_value: 8,
                colormap_entries: 256,
                red_mask: masks[0],
                green_mask: masks[1],
                blue_mask: masks[2],
            }],
        }],
    }
}

/// A TrueColor or DirectColor visual takes the colour channel by channel, scaled to its masks; a
/// visual with a colormap, or one the screen does not have, takes none.
#[test]
fn a_background_is_a_pixel_of_a_decomposed_visual() {
    let rgb888 = screen(4, [0xff_0000, 0xff00, 0xff]);
    assert_eq!(background_pixel(&rgb888, 0x21, 0x33_6699), Some(0x33_6699));
    let bgr888 = screen(5, [0xff, 0xff00, 0xff_0000]);
    assert_eq!(background_pixel(&bgr888, 0x21, 0x33_6699), Some(0x99_6633));
    let rgb101010 = screen(4, [0x3ff0_0000, 0xf_fc00, 0x3ff]);
    assert_eq!(background_pixel(&rgb101010, 0x21, 0xff_00ff), Some(0x3ff0_03ff));
    let pseudo = screen(3, [0, 0, 0]);
    assert_eq!(background_pixel(&pseudo, 0x21, 0x33_6699), None);
    assert_eq!(background_pixel(&rgb888, 0x22, 0x33_6699), None);
}

/// `window`'s pixel at (`x`, `y`) as the server holds it, its colour channels only.
fn pixel(watcher: &Connection, window: &Window, x: i16, y: i16) -> u32 {
    window.xconn.sync_with_server().expect("the window's requests reach the server");
    let screen = &watcher.setup().roots[0];
    let reply = watcher
        .send(xproto::get_image(2, window.id().0 as u32, x, y, 1, 1, !0))
        .unwrap()
        .reply()
        .expect("GetImage of one pixel");
    let bytes: [u8; 4] = reply.data[..4].try_into().expect("a 32-bit pixel");
    let raw = if watcher.setup().image_byte_order == 0 {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    };
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|depth| &depth.visuals)
        .find(|v| v.visual_id == reply.visual)
        .expect("the window's visual");
    raw & (visual.red_mask | visual.green_mask | visual.blue_mask)
}

/// The hang detector: how long a wait for what must happen may take, which it never needs.
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// Ask `ready` until it answers, failing the test after [`HANG`].
fn until_within<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + HANG;
    loop {
        if let Some(value) = ready() {
            return value;
        }
        assert!(std::time::Instant::now() < deadline, "{what} never came in {HANG:?}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_window_shows_its_background_until_drawn_and_keeps_its_corner_when_grown() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (watcher, _) = Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let (first, second) = (0x33_6699, 0x99_6633);
    let screen = &watcher.setup().roots[0];
    let Some(want_first) = background_pixel(screen, screen.root_visual, first) else {
        aterm_uds::host::could_not_run("$DISPLAY's root visual has a colormap");
    };
    let want_second =
        background_pixel(screen, screen.root_visual, second).expect("the same visual");
    let attributes = WindowAttributes::default()
        .with_inner_size(crate::dpi::PhysicalSize::new(120, 90))
        .with_x11_background(first);
    let window = Window::new(target, attributes).expect("an X11 window");
    let xid = window.id().0 as u32;
    let viewable = || {
        let reply = watcher
            .send(xproto::get_window_attributes(xid))
            .unwrap()
            .reply()
            .expect("GetWindowAttributes");
        (reply.map_state == 2).then_some(reply)
    };
    let attrs = until_within("the window's map", viewable);
    assert_eq!(attrs.bit_gravity, 1, "the contents keep the top-left (NorthWestGravity)");
    until_within("the background on the mapped window", || {
        (pixel(&watcher, &window, 5, 5) == want_first).then_some(())
    });

    // What a frame would leave in the window's top-left corner, then a new background and a
    // resize: the corner stays, and the gained area takes the new background.
    let gc = watcher.generate_id().expect("an id");
    watcher.send_void(xproto::create_gc(gc, xid, &ValueList::new())).unwrap();
    let word = if watcher.setup().image_byte_order == 0 {
        want_second.to_le_bytes()
    } else {
        want_second.to_be_bytes()
    };
    let block: Vec<u8> = word.iter().copied().cycle().take(4 * 10 * 10).collect();
    let depth = screen.root_depth;
    watcher.send_void(xproto::put_image(2, xid, gc, 10, 10, 0, 0, 0, depth, &block)).unwrap();
    watcher.send_void(xproto::free_gc(gc)).unwrap();
    window.set_background(0x00_ff00);
    let want_green = background_pixel(screen, screen.root_visual, 0x00_ff00).expect("green");
    let _ = window.request_inner_size(crate::dpi::PhysicalSize::new(240, 180).into());
    until_within("the window's growth", || {
        let reply = watcher.send(xproto::get_geometry(xid)).unwrap().reply().expect("GetGeometry");
        (reply.width > 150 && reply.height > 120).then_some(())
    });
    until_within("the new background in the gained area", || {
        (pixel(&watcher, &window, 200, 150) == want_green).then_some(())
    });
    assert_eq!(pixel(&watcher, &window, 5, 5), want_second, "the corner a frame drew stays put");
}
