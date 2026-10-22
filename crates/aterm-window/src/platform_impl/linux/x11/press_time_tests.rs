// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A press's server time is what a launch for it carries (`platform::x11::user_time`, which
//! aterm hands `xdg-open` as `DESKTOP_STARTUP_ID=_TIME<t>`). mutter refuses an activation stamped
//! before the newest press it knows of, so a browser that presented its window with its own older
//! user time stayed behind aterm, marked "is ready" (the desk audit, GNOME 46 on Xorg,
//! 2026-10-22). Here the event loop's button handler is fed a press and a later release: the
//! press's time is noted, and the release's is not.
//!
//! The negative control: without the note in `xinput2_button_input`, `user_time` stays `None`.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use super::ffi::XIDeviceEvent;
use crate::event::ElementState;
use crate::platform::x11::user_time;
use crate::platform_impl::EventLoop;

#[test]
fn a_press_notes_its_server_time_and_a_release_does_not() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let mut event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let button = |time: u64| {
        // SAFETY: a plain C struct; every pointer in it stays null and the handler reads none.
        let mut event: XIDeviceEvent = unsafe { std::mem::zeroed() };
        event.detail = 1;
        event.time = time as _;
        event
    };
    let EventLoop::X(x11) = &mut event_loop else { panic!("an X11 loop") };
    let processor = &mut x11.event_processor;

    processor.xinput2_button_input(
        &button(424_242),
        ElementState::Pressed,
        |_, _: crate::event::Event<()>| {},
    );
    assert_eq!(user_time(), Some(424_242), "a press notes its time");
    processor.xinput2_button_input(
        &button(525_252),
        ElementState::Released,
        |_, _: crate::event::Event<()>| {},
    );
    assert_eq!(user_time(), Some(424_242), "a release is no new press");
}
