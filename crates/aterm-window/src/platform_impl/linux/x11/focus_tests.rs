// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A keyboard grab is not a focus change. On X11 a window manager holds its shortcuts as passive
//! key grabs (on GNOME: Super+Space, Alt+Tab, Print, every media key gsd-media-keys grabs,
//! each detent of a volume knob), and while one is active the focused window gets FocusOut with
//! a grab mode, then FocusIn with an ungrab mode, though the focus never moved. Upstream took
//! each pair for a real one: aterm then wrote `ESC[O` `ESC[I` to a program that asked for focus
//! reports, ended a drag-selection, closed its menus and unfocused the input context, which
//! cancels a composition (the desk audit, GNOME 46 on Xorg, 2026-10-22). xterm ignores them.
//! A focus that moves while the keyboard is grabbed (an Alt+Tab that switches windows) is
//! still a change.
//!
//! The events are XI2 focus events the server would send, handed to the event processor's
//! handlers for a window of the backend: grabbing the desk's keyboard and moving its focus is
//! not a test's to do. The negative control: without `is_grab_focus_change`'s checks, the grab
//! pair reports `Focused(false)` and `Focused(true)`.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use super::ffi::XIFocusInEvent;
use super::Window;
use crate::event::{Event, WindowEvent};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// XI2 focus modes (XI2.h).
const NORMAL: i32 = 0;
const GRAB: i32 = 1;
const UNGRAB: i32 = 2;
const WHILE_GRABBED: i32 = 3;
const PASSIVE_GRAB: i32 = 4;
const PASSIVE_UNGRAB: i32 = 5;

#[test]
fn a_keyboard_grab_is_not_a_focus_change() {
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
    let focus = |mode: i32| {
        // SAFETY: a plain C struct; every pointer in it stays null and the handlers read none.
        let mut event: XIFocusInEvent = unsafe { std::mem::zeroed() };
        event.event = window.id().0 as _;
        event.mode = mode;
        event
    };
    let mut seen = Vec::new();
    {
        let mut record = |_: &_, event: Event<()>| {
            if let Event::WindowEvent { event: WindowEvent::Focused(focused), .. } = event {
                seen.push(focused);
            }
        };
        let EventLoop::X(x11) = &mut event_loop else { panic!("an X11 loop") };
        let processor = &mut x11.event_processor;

        processor.xinput2_focused(&focus(NORMAL), &mut record);
        for (out, back) in [(GRAB, UNGRAB), (PASSIVE_GRAB, PASSIVE_UNGRAB)] {
            processor.xinput2_unfocused(&focus(out), &mut record);
            processor.xinput2_focused(&focus(back), &mut record);
        }
        processor.xinput2_unfocused(&focus(WHILE_GRABBED), &mut record);
        processor.xinput2_focused(&focus(NORMAL), &mut record);
        processor.xinput2_unfocused(&focus(NORMAL), &mut record);
    }
    assert_eq!(
        seen,
        [true, false, true, false],
        "a grab's FocusOut and FocusIn report nothing; a focus moved during a grab, or without \
         one, does"
    );
}
