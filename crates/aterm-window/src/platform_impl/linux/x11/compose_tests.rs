// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A composition the input method ends without saying so does not stay open.

use std::cell::RefCell;

use super::event_processor::EventProcessor;
use super::ffi::{self, XIFocusInEvent, XKeyEvent};
use super::ime::ImeEvent;
use super::Window;
use crate::event::{ElementState, Event, Ime, WindowEvent};
use crate::keyboard::{KeyCode, PhysicalKey};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// XI2's `XINotifyNormal` focus mode (XI2.h).
const NORMAL: i32 = 0;

/// The Left arrow's X keycode: evdev's `KEY_LEFT` (105) plus X's offset of 8, which the
/// physical key is read from whatever the server's keymap.
const LEFT: u32 = 113;

/// What the application was told, of what these tests look at.
#[derive(Debug, Clone, PartialEq)]
enum Told {
    Preedit(String),
    Enabled,
    Disabled,
    Left,
}

/// The recorder's reading of one event.
fn told(event: Event<()>) -> Option<Told> {
    let Event::WindowEvent { event, .. } = event else { return None };
    match event {
        WindowEvent::Ime(Ime::Preedit(text, _)) => Some(Told::Preedit(text)),
        WindowEvent::Ime(Ime::Enabled) => Some(Told::Enabled),
        WindowEvent::Ime(Ime::Disabled) => Some(Told::Disabled),
        WindowEvent::KeyboardInput { event, .. }
            if event.physical_key == PhysicalKey::Code(KeyCode::ArrowLeft) =>
        {
            Some(Told::Left)
        },
        _ => None,
    }
}

/// A test's loop, its window and what the window was told.
struct Bed {
    event_loop: EventLoop<()>,
    window: Window,
    seen: RefCell<Vec<Told>>,
}

impl Bed {
    fn new() -> Bed {
        let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
        let window = {
            let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
                panic!("an X11 loop");
            };
            assert!(
                target.ime.is_some(),
                "an input method opens on $DISPLAY (XMODIFIERS's, else Xlib's local one)"
            );
            Window::new(target, WindowAttributes::default().with_visible(false))
                .expect("an X11 window")
        };
        let mut bed = Bed { event_loop, window, seen: RefCell::new(Vec::new()) };
        bed.process_ime();
        bed.take();
        bed
    }

    fn processor(&mut self) -> &mut EventProcessor {
        let EventLoop::X(x11) = &mut self.event_loop else { panic!("an X11 loop") };
        &mut x11.event_processor
    }

    fn xwindow(&self) -> ffi::Window {
        self.window.id().0 as ffi::Window
    }

    /// What the window was told since the last look.
    fn take(&self) -> Vec<Told> {
        std::mem::take(&mut *self.seen.borrow_mut())
    }

    fn process_ime(&mut self) {
        let seen = &self.seen;
        let EventLoop::X(x11) = &mut self.event_loop else { panic!("an X11 loop") };
        x11.event_processor.process_ime(|_, event| seen.borrow_mut().extend(told(event)));
    }

    /// The window's focus moves in (`true`) or out, as XI2 reports it with no grab.
    fn focus(&mut self, gained: bool) {
        // SAFETY: a plain C struct; every pointer in it stays null and the handlers read none.
        let mut event: XIFocusInEvent = unsafe { std::mem::zeroed() };
        event.event = self.xwindow();
        event.mode = NORMAL;
        let seen = &self.seen;
        let EventLoop::X(x11) = &mut self.event_loop else { panic!("an X11 loop") };
        let record = |_: &_, event| seen.borrow_mut().extend(told(event));
        if gained {
            x11.event_processor.xinput2_focused(&event, record);
        } else {
            x11.event_processor.xinput2_unfocused(&event, record);
        }
    }

    /// The input method's preedit starts, as its preedit-start callback reports it.
    fn open_composition(&mut self) {
        let window = self.xwindow();
        let sender = self.ime(|ime| ime.event_sender_for_test());
        sender.send((window, ImeEvent::Start)).expect("the loop's IME channel");
        self.process_ime();
        assert_eq!(self.take(), [Told::Preedit(String::new())], "the preedit starts");
        let composing = self.processor().composing;
        assert_eq!(composing, Some(window as u32), "and is open");
    }

    /// The Left arrow is pressed in the focused window.
    fn press_left(&mut self) {
        let window = self.xwindow();
        let (display, root) = {
            let PlatformActiveEventLoop::X(target) = &self.processor().target.p else {
                panic!("an X11 loop");
            };
            let xconn = target.x_connection();
            (xconn.display, xconn.default_root().root)
        };
        // SAFETY: a plain C struct; its display is the backend's live one, the one pointer Xlib's
        // lookup reads.
        let mut event: XKeyEvent = unsafe { std::mem::zeroed() };
        event.type_ = ffi::event_type::KEY_PRESS;
        event.display = display;
        event.window = window;
        event.root = root as ffi::Window;
        event.time = 1;
        event.keycode = LEFT;
        event.same_screen = ffi::TRUE;
        let seen = &self.seen;
        let EventLoop::X(x11) = &mut self.event_loop else { panic!("an X11 loop") };
        x11.event_processor.xinput_key_input(&mut event, ElementState::Pressed, |_, event| {
            seen.borrow_mut().extend(told(event))
        });
    }

    fn ime<R>(&mut self, with: impl FnOnce(&mut super::ime::Ime) -> R) -> R {
        let PlatformActiveEventLoop::X(target) = &self.processor().target.p else {
            panic!("an X11 loop");
        };
        let ime = target.ime.as_ref().expect("the backend's input method");
        with(&mut ime.borrow_mut())
    }
}

#[test]
fn a_focus_loss_ends_the_composition_the_input_method_left_open() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let mut bed = Bed::new();
    bed.focus(true);
    bed.press_left();
    assert_eq!(bed.take(), [Told::Left], "with no composition the arrow is a key");

    bed.open_composition();
    bed.press_left();
    assert_eq!(bed.take(), [], "in a composition the arrow goes to the input method");

    bed.focus(false);
    assert_eq!(bed.take(), [Told::Preedit(String::new())], "the focus loss ends the preedit");
    assert_eq!(bed.processor().composing, None);
    bed.focus(true);
    bed.press_left();
    assert_eq!(bed.take(), [Told::Left], "the focus back, the arrow is a key again");
}

#[test]
fn a_new_input_context_ends_the_composition_its_window_had_open() {
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("SKIP: no DISPLAY");
        return;
    }
    let _turn = super::shared_display_turn();
    let mut bed = Bed::new();
    bed.focus(true);

    bed.open_composition();
    bed.window.set_ime_allowed(true);
    bed.process_ime();
    assert_eq!(bed.take(), [Told::Enabled], "the new context is told");
    assert_eq!(bed.processor().composing, None, "and has no composition");
    bed.press_left();
    assert_eq!(bed.take(), [Told::Left], "the arrow is a key");

    bed.open_composition();
    assert!(bed.ime(|ime| ime.replace_for_test()), "an input method opens again");
    bed.process_ime();
    assert_eq!(bed.take(), [Told::Enabled], "the remade context is told");
    assert_eq!(bed.processor().composing, None, "and has no composition");
    bed.press_left();
    assert_eq!(bed.take(), [Told::Left], "the arrow is a key");
}
