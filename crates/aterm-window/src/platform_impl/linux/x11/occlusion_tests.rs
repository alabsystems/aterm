// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! X11 window occlusion tests for unmap, `_NET_WM_STATE_HIDDEN`, and `VisibilityNotify`.

use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use aterm_x11::proto::xproto::{self, window_class, PropMode};
use aterm_x11::proto::{atom, Request, ValueList, Void};
use aterm_x11::Connection;

use super::Window;
use crate::event::{Event, WindowEvent};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

const HANG: Duration = Duration::from_secs(60);

const VISIBILITY_NOTIFY: u8 = 15;
const UNOBSCURED: u8 = 0;
const FULLY_OBSCURED: u8 = 2;

struct Rig {
    window: Window,
    event_loop: EventLoop<()>,
    stand_in: Connection,
    xwindow: u32,
    net_wm_state: u32,
    hidden: u32,
    seen: Vec<bool>,
    _turn: MutexGuard<'static, ()>,
}

impl Rig {
    fn new() -> Option<Rig> {
        if std::env::var_os("DISPLAY").is_none() {
            eprintln!("SKIP: no DISPLAY");
            return None;
        }
        let turn = super::shared_display_turn();
        let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        let window = Window::new(target, WindowAttributes::default().with_visible(false))
            .expect("an X11 window");
        let xwindow = window.id().0 as u32;
        let (stand_in, screen) =
            Connection::connect(None).expect("DISPLAY is set: the connection must open");
        let root = stand_in.setup().roots[screen].root;
        let [net_wm_state, hidden] =
            stand_in.intern_atoms(&[b"_NET_WM_STATE", b"_NET_WM_STATE_HIDDEN"]).unwrap()[..]
        else {
            unreachable!()
        };
        let parent = stand_in.generate_id().unwrap();
        stand_in
            .send_void(xproto::create_window(
                0,
                parent,
                root,
                0,
                0,
                1,
                1,
                0,
                window_class::INPUT_OUTPUT,
                0,
                &ValueList::new(),
            ))
            .unwrap()
            .check()
            .expect("the stand-in's parent window");
        stand_in
            .send_void(xproto::reparent_window(xwindow, parent, 0, 0))
            .unwrap()
            .check()
            .expect("the window moved into the stand-in's");
        let mut rig = Rig {
            window,
            event_loop,
            stand_in,
            xwindow,
            net_wm_state,
            hidden,
            seen: Vec::new(),
            _turn: turn,
        };
        rig.window.set_visible(true);
        rig.fence();
        Some(rig)
    }

    fn pump(&mut self, timeout: Duration) {
        let Rig { event_loop, seen, .. } = self;
        event_loop.pump_events(Some(timeout), |event, _| {
            if let Event::WindowEvent { event: WindowEvent::Occluded(occluded), .. } = event {
                seen.push(occluded);
            }
        });
    }

    fn fence(&mut self) {
        self.stand_in
            .send(xproto::get_input_focus())
            .unwrap()
            .reply()
            .expect("the stand-in's sync");
        self.window.xconn.sync_with_server().expect("the window's round trip");
        self.pump(Duration::ZERO);
    }

    fn until(&mut self, what: &str, count: usize) {
        let deadline = Instant::now() + HANG;
        while self.seen.len() < count {
            assert!(
                Instant::now() < deadline,
                "{what} never came in {HANG:?}; the window reported {:?}",
                self.seen
            );
            self.pump(Duration::from_millis(5));
        }
        self.fence();
    }

    fn wm(&self, request: Request<Void>) {
        self.stand_in.send_void(request).unwrap();
        self.stand_in.flush().unwrap();
    }

    fn set_hidden(&self, hidden: bool) {
        let state: &[u32] = if hidden { &[self.hidden] } else { &[] };
        self.wm(xproto::change_property32(
            PropMode::Replace,
            self.xwindow,
            self.net_wm_state,
            atom::ATOM,
            state,
        ));
    }

    fn visibility(&self, state: u8) {
        let mut event = [0u8; 32];
        event[0] = VISIBILITY_NOTIFY;
        event[4..8].copy_from_slice(&self.xwindow.to_ne_bytes());
        event[8] = state;
        self.wm(xproto::send_event(false, self.xwindow, 0, &event));
    }
}

#[test]
fn a_window_the_window_manager_hides_is_occluded() {
    let Some(mut rig) = Rig::new() else {
        return;
    };
    assert_eq!(rig.seen, [], "a window shown and never hidden reports nothing");

    rig.wm(xproto::unmap_window(rig.xwindow));
    rig.until("the minimized window's occlusion", 1);
    assert_eq!(rig.seen, [true], "the unmapped window is occluded");
    rig.wm(xproto::map_window(rig.xwindow));
    rig.until("the restored window's reveal", 2);
    assert_eq!(rig.seen, [true, false], "the mapped window is not");

    rig.set_hidden(true);
    rig.until("the hidden window's occlusion", 3);
    assert_eq!(rig.seen, [true, false, true], "the hidden window is occluded");
    rig.visibility(FULLY_OBSCURED);
    rig.set_hidden(false);
    rig.fence();
    assert_eq!(
        rig.seen,
        [true, false, true],
        "covered while hidden, then shown but still covered: occluded throughout"
    );
    rig.visibility(UNOBSCURED);
    rig.until("the uncovered window's reveal", 4);
    assert_eq!(rig.seen, [true, false, true, false], "uncovered, it is not");

    rig.window.set_visible(false);
    rig.until("the window hidden by the application", 5);
    rig.window.set_visible(true);
    rig.until("the window shown again", 6);
    assert_eq!(rig.seen, [true, false, true, false, true, false]);
}
