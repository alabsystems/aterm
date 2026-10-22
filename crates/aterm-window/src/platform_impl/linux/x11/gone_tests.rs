// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A display that goes between two of the event loop's looks at its connection fails every
//! request a caller makes in between, and none of those calls panics: each returns what it can,
//! and the loop's next look ends the run with `EventLoopError::DisplayLost` (X11-GONE). The
//! Linux lane found the crash journal's `outer_position` panicking there, at upstream's unwrap
//! of `XTranslateCoordinates` (`docs/LINUX-RUNTIME-LANE.md`, "The display gone").
//!
//! The loss is the client's socket shut down under libxcb and Xlib (`shutdown(2)`, both ways):
//! a write fails and a read finds end of file, as when the server dies, and the lane's Xvfb
//! stays up for the other tests. The X11 backend is one connection per process
//! (`X11_BACKEND`), and Xlib's IO error is noted process-wide, so the work runs in a child: this
//! test binary run again on this one test, with [`CHILD`] set. The parent holds the child to a
//! one-minute hang detector and passes only when it exited 0, printed [`DONE`] and never
//! panicked.
//!
//! Its negative controls: upstream's unwrap restored in `UnownedWindow::inner_position_physical`,
//! and the child panics in `outer_position`; the broken connection's check taken out of
//! `XConnection::set_cursor_icon`, and the child dies of SIGSEGV inside libXcursor, as it did
//! when this test was first run; the full-screen state kept when `set_fullscreen` found no
//! position, and the child's assertion fails; the lost display's check taken out of
//! `ImeInner::server_side_gone`, and on a desk with an input method (IBus under GNOME) the child
//! dies of SIGSEGV in libX11's `XSendEvent` under the event loop's `XDestroyIC`, as it did on such
//! a desk on 2026-10-22 (Xvfb, with no input method, makes no input context). Each way the parent
//! fails.
//!
//! It runs where `$DISPLAY` names an X server (the Linux lane's Xvfb); elsewhere it says SKIP and
//! returns.

use std::io::Read as _;
use std::time::{Duration, Instant};

use super::window::Visibility;
use super::Window;
use crate::cursor::Cursor;
use crate::cursor_icon::CursorIcon;
use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::error::EventLoopError;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop, Fullscreen};
use crate::window::{Theme, UserAttentionType, WindowAttributes};

/// Set in the child's environment: run the test's body.
const CHILD: &str = "ATERM_WINDOW_GONE_CHILD";

/// What the child prints once every call returned and the run ended with the loss.
const DONE: &str = "X11-GONE: every call returned";

/// The hang detector: how long the child may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

#[test]
fn a_lost_display_fails_each_call_and_ends_the_run_without_a_panic() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    if std::env::var_os(CHILD).is_some() {
        lose_the_display_under_a_window();
        return;
    }
    let name = "a_lost_display_fails_each_call_and_ends_the_run_without_a_panic";
    let module = module_path!().split_once("::").map_or(module_path!(), |(_, path)| path);
    let scratch = aterm_tempfile::tempdir().expect("a scratch dir");
    let out = scratch.path().join("out");
    let err = scratch.path().join("err");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .args(["--exact", &format!("{module}::{name}"), "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .stdout(std::fs::File::create(&out).expect("the child's stdout"))
        .stderr(std::fs::File::create(&err).expect("the child's stderr"))
        .spawn()
        .expect("the test binary runs again");
    let deadline = Instant::now() + HANG;
    let status = loop {
        if let Some(status) = child.try_wait().expect("the child's status") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let read = |path: &std::path::Path| {
        let mut text = String::new();
        let _ = std::fs::File::open(path).and_then(|mut f| f.read_to_string(&mut text));
        text
    };
    let (stdout, stderr) = (read(&out), read(&err));
    let said = format!("its stdout:\n{stdout}\nits stderr:\n{stderr}");
    let status = status.unwrap_or_else(|| panic!("the child still ran after {HANG:?}; {said}"));
    assert!(!stderr.contains("panicked at"), "the child panicked; {said}");
    assert!(status.success(), "the child exited {status}; {said}");
    assert!(stdout.contains(DONE), "the child never got through its calls; {said}");
    // The loss is said once, by the run's end: a broken connection's failed requests say
    // nothing of their own (`XConnection::report`).
    assert!(!stderr.contains("aterm-window:"), "a failed request was reported; {said}");
}

/// The child's work: a window on `$DISPLAY`, its socket shut down, every call that asks the
/// server something, then the event loop's run.
fn lose_the_display_under_a_window() {
    let mut event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
        panic!("an X11 loop");
    };
    let window = Window::new(target, WindowAttributes::default()).expect("an X11 window");
    let xconn = std::sync::Arc::clone(&window.xconn);

    // Live, the server answers: the position (and with it the frame extents, now cached) and
    // the size the window was made with (800x600, no window manager to change it).
    assert!(window.outer_position().is_ok(), "the position, live");
    assert_eq!(window.inner_size(), PhysicalSize::new(800, 600), "the size, live");
    assert!(!xconn.is_broken(), "the connection, live");

    // The display goes.
    let socket = xconn.xcb_connection().file_descriptor();
    // SAFETY: the connection's own socket, open; shutting it down closes nothing.
    assert_eq!(unsafe { libc::shutdown(socket, libc::SHUT_RDWR) }, 0, "shutdown(2)");

    // What the crash journal asks between the loop's looks: the round trip upstream unwrapped.
    assert!(window.outer_position().is_err(), "a position from a display that has gone");
    assert!(xconn.is_broken(), "the failed round trip found the connection broken");
    assert!(window.inner_position().is_err(), "no inner position either");
    // A size is the last the server reported, here the one the window was made with.
    assert_eq!(window.inner_size(), PhysicalSize::new(800, 600), "the size, gone");
    let _ = window.outer_size();
    let _ = window.is_maximized();
    let _ = window.is_minimized();
    let _ = window.available_monitors();
    let _ = window.primary_monitor();
    assert!(window.drag_window().is_err(), "a drag needs the pointer");
    window.set_title("gone");
    window.set_theme(Some(Theme::Dark));
    window.set_cursor(Cursor::Icon(CursorIcon::Text));
    window.set_maximized(true);
    window.set_minimized(true);
    window.set_visible(false);
    window.set_visible(true);
    window.set_outer_position(PhysicalPosition::new(10, 10).into());
    let _ = window.request_inner_size(PhysicalSize::new(640, 480).into());
    window.set_min_inner_size(Some(PhysicalSize::new(100, 100).into()));
    window.set_resize_increments(Some(PhysicalSize::new(8, 16).into()));
    window.request_user_attention(Some(UserAttentionType::Critical));
    window.focus_window();
    // IME requests wait for the loop, which ends on the loss before it sends them.
    window
        .set_ime_cursor_area(PhysicalPosition::new(10, 10).into(), PhysicalSize::new(9, 20).into());
    window.set_ime_allowed(true);
    // A shown window's full screen asks its position first, and without one sends nothing and
    // keeps the state the window had.
    window.shared_state_lock().visibility = Visibility::Yes;
    assert!(!window.shared_state_lock().last_monitor.is_dummy(), "a monitor to fill");
    window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    assert!(window.fullscreen().is_none(), "a full screen the server never heard of");
    window.request_redraw();

    // The loop's look at the connection ends the run with the loss.
    let run = event_loop.run_on_demand(|_, _| {});
    match run {
        Err(EventLoopError::DisplayLost(why)) => println!("{DONE}; the run ended: {why}"),
        other => panic!("the run ended {other:?}, not with the display's loss"),
    }
    drop(window);
}
