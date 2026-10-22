// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Where and when the input method is told the caret is (`XNSpotLocation`), the desk audit's
//! two findings (GNOME 46 on Xorg with IBus through XIM, 2026-10-22).
//!
//! Where: the spot is the caret box's bottom-left. ibus-x11 opens its candidate popup at the
//! spot, and upstream's top-left put the popup over the line being typed. A box aterm-gui
//! reports (`set_ime_cursor_area`, the caret cell in window pixels) becomes the request the
//! event loop sends on. The negative control: with the box's size dropped again, as upstream
//! dropped it, the request carries the box's top, 20, where its bottom is 36.
//!
//! When: setting the spot is a synchronous XIM request (`XSetICValues`, no timeout) on the event
//! loop's thread, and upstream sent one whenever any window's caret moved, an unfocused one's
//! too, so a stalled ibus-x11 froze every window. Only the focused window's input context is
//! told, only a spot it does not have, and an unfocused window's latest goes out once when it
//! gains the focus. Each spot sent is counted at the send (`ime::SPOTS_SENT`); the focus events
//! are XI2 focus events the server would send, handed to the event processor's handlers, as
//! `focus_tests` does, under the input method `XMODIFIERS` names on `$DISPLAY`, else Xlib's
//! local one. The negative controls: without the focus check in `Ime::send_xim_spot`, the
//! unfocused window's move is sent; without `ImeContext::set_spot`'s comparison, the unmoved
//! caret is sent again; without the send in `Ime::focus`, the focus in sends nothing.
//!
//! A context remade (the input method replaced, or the window's context remade by
//! `set_ime_allowed`) is told the focused window's spot alone, and the focused window's new
//! context takes the focus its old one had, which IBus needs to compose in it; each focus
//! given is counted too (`ime::FOCUSES_SENT`). The negative controls: with `replace_im`'s
//! upstream spot (every old context's), the unfocused window's goes out too; without
//! `ImeInner::refocus` in it, or the focus in `Ime::create_context`, no new context has the
//! focus; with no spot for a new context, as upstream gave none, the remade one is told none.
//!
//! The windows' parts run where `$DISPLAY` names an X server; elsewhere they say SKIP and
//! return.

use super::event_processor::EventProcessor;
use super::ffi::{self, XIFocusInEvent};
use super::ime::{ImeRequest, FOCUSES_SENT, SPOTS_SENT};
use super::window::xim_spot;
use super::Window;
use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::event::Event;
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// XI2's `XINotifyNormal` focus mode (XI2.h).
const NORMAL: i32 = 0;

#[test]
fn the_spot_is_the_caret_boxs_bottom_left() {
    assert_eq!(xim_spot(PhysicalPosition::new(10, 20), PhysicalSize::new(8, 16)), (10, 36));
    let low = PhysicalPosition::new(-3, i32::from(i16::MAX) - 4);
    assert_eq!(xim_spot(low, PhysicalSize::new(8, 16)), (-3, i16::MAX), "clamped to 16 bits");

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
    window
        .set_ime_cursor_area(PhysicalPosition::new(10, 20).into(), PhysicalSize::new(8, 16).into());
    let EventLoop::X(x11) = &mut event_loop else { panic!("an X11 loop") };
    match x11.event_processor.ime_receiver.try_recv() {
        Ok(ImeRequest::Position(to, x, y)) => {
            assert_eq!((to, x, y), (window.id().0 as _, 10, 36), "the box's bottom-left")
        },
        _ => panic!("the caret box queues a spot request"),
    }
}

/// A loop whose handlers the spot test calls.
struct Desk(EventLoop<()>);

impl Desk {
    fn processor(&mut self) -> &mut EventProcessor {
        let EventLoop::X(x11) = &mut self.0 else { panic!("an X11 loop") };
        &mut x11.event_processor
    }

    fn ime<R>(&mut self, with: impl FnOnce(&mut super::ime::Ime) -> R) -> R {
        let PlatformActiveEventLoop::X(target) = &self.processor().target.p else {
            panic!("an X11 loop");
        };
        with(&mut target.ime.as_ref().expect("the backend's input method").borrow_mut())
    }

    /// The event loop's turn to send the IME requests the windows queued.
    fn send_requests(&mut self) {
        self.processor().process_ime(|_, _: Event<()>| {});
    }

    /// `window`'s caret cell at (`x`, `y`), 8x16, as aterm-gui reports it.
    fn caret(&mut self, window: &Window, x: i32, y: i32) {
        let size = PhysicalSize::new(8u32, 16);
        window.set_ime_cursor_area(PhysicalPosition::new(x, y).into(), size.into());
        self.send_requests();
    }

    /// `window`'s focus moves in (`true`) or out, as XI2 reports it with no grab.
    fn focus(&mut self, window: &Window, gained: bool) {
        // SAFETY: a plain C struct; every pointer in it stays null and the handlers read none.
        let mut event: XIFocusInEvent = unsafe { std::mem::zeroed() };
        event.event = window.id().0 as ffi::Window;
        event.mode = NORMAL;
        let ignore = |_: &_, _: Event<()>| {};
        if gained {
            self.processor().xinput2_focused(&event, ignore);
        } else {
            self.processor().xinput2_unfocused(&event, ignore);
        }
    }
}

/// The spots sent since the last look, each with its window.
fn sent() -> Vec<(ffi::Window, i16, i16)> {
    SPOTS_SENT.with(|sent| std::mem::take(&mut *sent.borrow_mut()))
}

/// The windows whose input contexts were given the focus since the last look.
fn focused() -> Vec<ffi::Window> {
    FOCUSES_SENT.with(|sent| std::mem::take(&mut *sent.borrow_mut()))
}

/// A loop with two windows that take compositions, as aterm-gui's do.
fn two_windows() -> (Desk, [Window; 2]) {
    let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
    let windows = {
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        assert!(
            target.ime.is_some(),
            "an input method opens on $DISPLAY (XMODIFIERS's, else Xlib's local one)"
        );
        [(); 2].map(|()| {
            Window::new(target, WindowAttributes::default().with_visible(false))
                .expect("an X11 window")
        })
    };
    let mut desk = Desk(event_loop);
    for window in &windows {
        window.set_ime_allowed(true);
    }
    desk.send_requests();
    (desk, windows)
}

#[test]
fn only_the_focused_window_tells_the_input_method_its_caret_and_only_when_it_moved() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (mut desk, [a, b]) = two_windows();
    let id = |window: &Window| window.id().0 as ffi::Window;
    assert_eq!(sent(), [], "a new context of an unfocused window is told no spot");

    desk.focus(&a, true);
    assert_eq!(sent(), [], "a has no spot yet");
    desk.caret(&a, 10, 20);
    assert_eq!(sent(), [(id(&a), 10, 36)], "the focused window's caret is sent");
    desk.caret(&a, 10, 20);
    assert_eq!(sent(), [], "an unmoved caret is not sent again");
    desk.caret(&b, 30, 40);
    assert_eq!(sent(), [], "an unfocused window's caret is not sent");
    desk.caret(&a, 18, 20);
    assert_eq!(sent(), [(id(&a), 18, 36)], "a moved caret is");

    desk.focus(&a, false);
    desk.focus(&b, true);
    assert_eq!(sent(), [(id(&b), 30, 56)], "b's latest spot goes out once, at its focus");
    desk.caret(&b, 30, 40);
    desk.caret(&a, 26, 20);
    assert_eq!(sent(), [], "b's unmoved caret and unfocused a's move are not sent");

    desk.focus(&b, false);
    desk.focus(&a, true);
    assert_eq!(sent(), [(id(&a), 26, 36)], "a's move goes out at its focus");
    desk.focus(&a, false);
    desk.focus(&a, true);
    assert_eq!(sent(), [], "a focus with the caret unmoved sends nothing");
}

#[test]
fn a_remade_input_context_is_told_the_focused_windows_spot_and_focus() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let (mut desk, [a, b]) = two_windows();
    let id = |window: &Window| window.id().0 as ffi::Window;
    desk.focus(&b, true);
    desk.caret(&b, 30, 40);
    desk.focus(&b, false);
    desk.focus(&a, true);
    desk.caret(&a, 10, 20);
    assert_eq!(sent(), [(id(&b), 30, 56), (id(&a), 10, 36)], "each caret went out focused");
    focused();

    // The input method replaced, as its callbacks replace it when IBus restarts: only the
    // focused window's new context is told its spot, and it takes the focus.
    desk.ime(|ime| assert!(ime.replace_for_test(), "an input method opens again"));
    assert_eq!(sent(), [(id(&a), 10, 36)], "the focused window's spot, not b's");
    assert_eq!(focused(), [id(&a)], "the focused window's new context has the focus");

    // The focused window's context remade to take no compositions, then to take them again.
    a.set_ime_allowed(false);
    desk.send_requests();
    a.set_ime_allowed(true);
    desk.send_requests();
    assert_eq!(sent(), [(id(&a), 10, 36)], "the new context is told the spot");
    assert_eq!(focused(), [id(&a), id(&a)], "each new context has the focus");
}
