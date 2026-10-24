// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window's bell (`UnownedWindow::ring_bell`, `platform::x11::WindowExtX11::ring_x11_bell`) is an
//! XKB bell request naming the window, which the X server announces to every client that selected
//! XKB's `BellNotify`.

use std::ffi::{c_int, c_ulong};
use std::time::{Duration, Instant};

use aterm_x11::xlib::{
    self, xkb_event, Display, XEvent, XPointer, XkbAnyEvent, XkbBellNotifyEvent,
};
use aterm_x11::Connection;

use super::Window;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

const HANG: Duration = Duration::from_secs(60);

/// `XkbBellNotifyMask` (`X11/extensions/XKB.h`).
const XKB_BELL_NOTIFY_MASK: c_ulong = 1 << 8;

unsafe extern "C" fn any_event(_: *mut Display, _: *mut XEvent, _: XPointer) -> xlib::Bool {
    xlib::TRUE
}

fn bells(xlib: &xlib::Xlib, listener: *mut Display, xkb_event_base: c_int) -> Vec<c_ulong> {
    let mut rung = Vec::new();
    loop {
        // SAFETY: a zeroed `XEvent` is a valid union of plain C data; `listener` is open.
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        // SAFETY: `listener` is open and `any_event` has the predicate's signature.
        let taken = unsafe {
            (xlib.XCheckIfEvent)(listener, &mut event, Some(any_event), std::ptr::null_mut())
        };
        if taken != xlib::TRUE {
            return rung;
        }
        // SAFETY: every Xlib event starts with its `type`; an XKB one is an `XkbAnyEvent`, and a
        // `BellNotify` an `XkbBellNotifyEvent`, which `XEvent`'s padding covers.
        unsafe {
            if event.type_ != xkb_event_base {
                continue;
            }
            let any = &*(&event as *const XEvent).cast::<XkbAnyEvent>();
            if any.xkb_type == xkb_event::BELL_NOTIFY {
                rung.push((*(&event as *const XEvent).cast::<XkbBellNotifyEvent>()).window);
            }
        }
    }
}

#[test]
fn a_windows_bell_is_announced_naming_the_window() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (watcher, _) = Connection::connect(None).expect("DISPLAY is set: the connection must open");
    let xkb =
        watcher.extension(b"XKEYBOARD").expect("QueryExtension").expect("the X server has XKB");
    let xkb_event_base = c_int::from(xkb.first_event);

    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let window = Window::new(target, WindowAttributes::default().with_visible(false))
        .expect("an X11 window");
    let xwindow = window.id().0 as c_ulong;

    let xlib = xlib::xlib().expect("libX11");
    // SAFETY: a null name opens `$DISPLAY`.
    let listener = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
    assert!(!listener.is_null(), "the listener's connection opens");
    // SAFETY: `listener` is open; XKB's core keyboard and the mask are the header's values.
    let selected = unsafe {
        (xlib.XkbSelectEvents)(
            listener,
            xlib::XKB_USE_CORE_KBD,
            XKB_BELL_NOTIFY_MASK,
            XKB_BELL_NOTIFY_MASK,
        )
    };
    assert_eq!(selected, xlib::TRUE, "the listener selects BellNotify");
    // SAFETY: `listener` is open. The round trip lands the selection.
    unsafe { (xlib.XSync)(listener, xlib::FALSE) };

    window.xconn.sync_with_server().expect("the window's requests reach the server");
    // SAFETY: `listener` is open.
    unsafe { (xlib.XSync)(listener, xlib::FALSE) };
    let early = bells(xlib, listener, xkb_event_base);
    assert!(!early.contains(&xwindow), "no bell names the window before it rings: {early:?}");

    window.ring_bell();
    window.xconn.sync_with_server().expect("the bell reaches the server");
    let deadline = Instant::now() + HANG;
    let mut rung = Vec::new();
    while !rung.contains(&xwindow) {
        rung.extend(bells(xlib, listener, xkb_event_base));
        assert!(
            Instant::now() < deadline,
            "no BellNotify named window {xwindow:#x} in {HANG:?}; bells named {rung:x?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // SAFETY: `listener` is open and nothing uses it after this.
    unsafe { (xlib.XCloseDisplay)(listener) };
}
