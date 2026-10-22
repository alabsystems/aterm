// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A window the window manager hides is occluded. Under a compositing window manager (mutter,
//! GNOME 46 on Xorg) the server reports every redirected window unobscured at its map and never
//! fully obscured after, and upstream learned occlusion from VisibilityNotify alone, so a window
//! minimized or on another workspace kept presenting, animating and running its probes, and a
//! task that finished in its front tab counted as seen (the desk audit, 2026-10-22). Here the
//! window is occluded while it is unmapped after a map (mutter unmaps an iconified window and
//! one on another workspace), while its `_NET_WM_STATE` holds `_NET_WM_STATE_HIDDEN`, or while
//! VisibilityNotify says it is fully obscured, and `WindowEvent::Occluded` reports each change
//! of that, and nothing else.
//!
//! A stand-in window manager on its own connection plays the window manager's part: it unmaps
//! and maps the window, writes its `_NET_WM_STATE`, and sends the VisibilityNotify a server
//! without compositing would. The window is made override-redirect first, so a real window
//! manager on the display (`:7` may run mutter) leaves it alone: its map is not redirected, and
//! nothing rewrites its state between the stand-in's write and the window's read of it. The test
//! pumps the window's event loop until the event it waits for comes, then fences: a round trip
//! of the stand-in, then of the window's connection, puts every event the stand-in's requests made
//! in the window's queue, and one more pump handles them, so what was reported is all there is.
//!
//! The negative controls: without the UnmapNotify arm in `process_xevent`, the minimized window
//! never reads occluded and the wait for it ends the test; without the `_NET_WM_STATE` branch in
//! `property_notify`, the hidden one never does; reporting each VisibilityNotify's state as it
//! comes as well, as upstream did, the map reports `Occluded(false)` before anything hid the
//! window.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use aterm_x11::proto::xproto::{self, cw, PropMode};
use aterm_x11::proto::{atom, Request, ValueList, Void};
use aterm_x11::Connection;

use super::Window;
use crate::event::{Event, WindowEvent};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// The hang detector: how long a wait for what must happen may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

/// `VisibilityNotify`'s event code and its `Unobscured` and `FullyObscured` states.
const VISIBILITY_NOTIFY: u8 = 15;
const UNOBSCURED: u8 = 0;
const FULLY_OBSCURED: u8 = 2;

/// A window of the backend and a stand-in window manager on its own connection. The window holds
/// the shared connection's turn while it lives. Fields drop in order: the window before its loop.
struct Rig {
    window: Window,
    event_loop: EventLoop<()>,
    /// The stand-in window manager's connection.
    stand_in: Connection,
    xwindow: u32,
    net_wm_state: u32,
    hidden: u32,
    /// What `WindowEvent::Occluded` reported for the window, in order.
    seen: Vec<bool>,
    _turn: MutexGuard<'static, ()>,
}

impl Rig {
    /// The window, made override-redirect and shown; `None`, after saying SKIP, where no X
    /// server is named.
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
        let (stand_in, _) =
            Connection::connect(None).expect("DISPLAY is set: the connection must open");
        let [net_wm_state, hidden] =
            stand_in.intern_atoms(&[b"_NET_WM_STATE", b"_NET_WM_STATE_HIDDEN"]).unwrap()[..]
        else {
            unreachable!()
        };
        stand_in
            .send_void(xproto::change_window_attributes(
                xwindow,
                &ValueList::new().set(cw::OVERRIDE_REDIRECT, 1),
            ))
            .unwrap()
            .check()
            .expect("the window made override-redirect");
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

    /// Pump the window's event loop once, for at most `timeout`.
    fn pump(&mut self, timeout: Duration) {
        let Rig { event_loop, seen, .. } = self;
        event_loop.pump_events(Some(timeout), |event, _| {
            if let Event::WindowEvent { event: WindowEvent::Occluded(occluded), .. } = event {
                seen.push(occluded);
            }
        });
    }

    /// Handle every event the stand-in's requests so far made for the window.
    fn fence(&mut self) {
        self.stand_in
            .send(xproto::get_input_focus())
            .unwrap()
            .reply()
            .expect("the stand-in's sync");
        self.window.xconn.sync_with_server().expect("the window's round trip");
        self.pump(Duration::ZERO);
    }

    /// Pump until the window has reported `count` occlusions, failing the test after [`HANG`],
    /// then fence.
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

    /// Send the request as the stand-in window manager.
    fn wm(&self, request: Request<Void>) {
        self.stand_in.send_void(request).unwrap();
        self.stand_in.flush().unwrap();
    }

    /// `_NET_WM_STATE` holding `_NET_WM_STATE_HIDDEN`, or nothing.
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

    /// A VisibilityNotify in `state`, as a server without compositing sends it.
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

    // Minimized: mutter unmaps the window, and maps it again when it is restored.
    rig.wm(xproto::unmap_window(rig.xwindow));
    rig.until("the minimized window's occlusion", 1);
    assert_eq!(rig.seen, [true], "the unmapped window is occluded");
    rig.wm(xproto::map_window(rig.xwindow));
    rig.until("the restored window's reveal", 2);
    assert_eq!(rig.seen, [true, false], "the mapped window is not");

    // `_NET_WM_STATE_HIDDEN`, and a VisibilityNotify while it holds.
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

    // Hidden by the application: unmapped, so occluded too, until it is shown again.
    rig.window.set_visible(false);
    rig.until("the window hidden by the application", 5);
    rig.window.set_visible(true);
    rig.until("the window shown again", 6);
    assert_eq!(rig.seen, [true, false, true, false, true, false]);
}
