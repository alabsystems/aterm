// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! The resize sync protocol (`sync_request.rs`) against a stand-in window manager on its own
//! connection, which sends a window of the backend a `_NET_WM_SYNC_REQUEST` and resizes it, as
//! mutter does, while the test pumps the window's event loop and reads the window's counter from
//! the server.
//!
//! [`a_resize_is_answered_by_the_frame_drawn_at_its_size`]: the window names its counter in
//! `_NET_WM_SYNC_REQUEST_COUNTER` and the protocol in `WM_PROTOCOLS`; a request whose serial's
//! high half is non-zero (and whose low half has its top bit set) is still unanswered when the
//! application handles the `Resized` and its redraw, and answered, to the serial, once the
//! application says it presented a frame (`frame_presented`); the window's drop destroys the
//! counter. Its negative controls: with `SetCounter` setting 0, the counter reads 0 after the
//! present; with a configure that changed the size answered at once, it reads the serial at the
//! `Resized`; without the present's answer, it reads 0 after the present.
//!
//! [`a_request_no_frame_follows_is_answered_anyway`]: an application that draws nothing at the
//! new size still answers, at the end of the event-loop turn that brought the `Resized` (it reads
//! unanswered while the application handles it); a configure that moved the window without
//! resizing it answers the request before it with no frame at all; and a request no configure
//! follows is answered by its deadline. Its negative controls: without the turn's end
//! (`end_turn_of_sync_requests`), the counter reads below the serial after the `Resized`'s turn;
//! with no deadline, the wait for the last answer ends the test.
//!
//! The window is unmapped and made override-redirect by the stand-in, so a window manager
//! running on the display (mutter, on `:7`) neither manages it nor sends it requests, and the
//! stand-in's resize goes to the server rather than to that manager as a `ConfigureRequest` it
//! would apply later. Round trips of both connections before each pump put what the stand-in
//! sent in the window's queue first, so a request and a resize sent together arrive together.
//!
//! They run where `$DISPLAY` names an X server with SYNC; elsewhere they say SKIP and return.

use std::cell::Cell;
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

use aterm_x11::proto::event::ClientMessage;
use aterm_x11::proto::xproto::{self, config, cw};
use aterm_x11::proto::{atom, sync, ValueList, CURRENT_TIME};
use aterm_x11::Connection;

use super::sync_request::SyncCounter;
use super::Window;
use crate::event::{Event, WindowEvent};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop};
use crate::window::WindowAttributes;

/// The hang detector: how long a wait for what must happen may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

/// The stand-in window manager, on its own connection.
struct Manager {
    conn: Connection,
    sync_major: u8,
    /// SYNC's first error: `Counter` (a counter that does not exist) is its error 0.
    bad_counter: u8,
    wm_protocols: u32,
    wm_delete_window: u32,
    net_wm_ping: u32,
    sync_request: u32,
    sync_request_counter: u32,
    /// The window's counter, from its `_NET_WM_SYNC_REQUEST_COUNTER`.
    counter: u32,
}

impl Manager {
    /// `None`, after saying SKIP, where the server has no SYNC.
    fn new() -> Option<Manager> {
        let (conn, _) =
            Connection::connect(None).expect("DISPLAY is set: the connection must open");
        let Some(ext) = conn.extension(sync::NAME).expect("QueryExtension SYNC") else {
            eprintln!("SKIP: the X server has no SYNC");
            return None;
        };
        conn.send(sync::initialize(ext.major_opcode, 3, 1))
            .unwrap()
            .reply()
            .expect("SYNC Initialize");
        let [wm_protocols, wm_delete_window, net_wm_ping, sync_request, sync_request_counter] =
            conn.intern_atoms(&[
                b"WM_PROTOCOLS",
                b"WM_DELETE_WINDOW",
                b"_NET_WM_PING",
                b"_NET_WM_SYNC_REQUEST",
                b"_NET_WM_SYNC_REQUEST_COUNTER",
            ])
            .unwrap()[..]
        else {
            unreachable!()
        };
        Some(Manager {
            conn,
            sync_major: ext.major_opcode,
            bad_counter: ext.first_error,
            wm_protocols,
            wm_delete_window,
            net_wm_ping,
            sync_request,
            sync_request_counter,
            counter: 0,
        })
    }

    /// `window`'s `property`: its type and 32-bit values.
    fn property32(&self, window: u32, property: u32) -> (u32, Vec<u32>) {
        let reply = self
            .conn
            .send(xproto::get_property(false, window, property, atom::ANY, 0, 64))
            .unwrap()
            .reply()
            .expect("GetProperty");
        (reply.type_, reply.value32().unwrap_or_default())
    }

    /// The window's counter once the window's own requests have reached the server.
    fn counter(&self, window: &Window) -> i64 {
        window.xconn.sync_with_server().expect("the window's requests reach the server");
        self.conn
            .send(sync::query_counter(self.sync_major, self.counter))
            .unwrap()
            .reply()
            .expect("QueryCounter")
            .counter_value
    }

    /// Ask `window` to answer `value` before the resize that follows, as mutter does: a
    /// `WM_PROTOCOLS` client message, the serial's low half in `data.l[2]` and its high half
    /// in `data.l[3]`.
    fn request(&self, window: u32, value: i64) {
        let bits = value.cast_unsigned();
        let message = ClientMessage::new32(window, self.wm_protocols, [
            self.sync_request,
            CURRENT_TIME,
            bits as u32,
            (bits >> 32) as u32,
            0,
        ]);
        self.conn.send_void(xproto::send_event(false, window, 0, &message.to_bytes())).unwrap();
    }

    fn configure(&self, window: u32, values: &ValueList) {
        self.conn.send_void(xproto::configure_window(window, values)).unwrap();
    }
}

/// A hidden, override-redirect window of the backend, its event loop, and the stand-in manager.
/// Fields drop in order: the window before its loop.
struct Rig {
    window: Window,
    event_loop: EventLoop<()>,
    manager: Manager,
    xwindow: u32,
    _turn: MutexGuard<'static, ()>,
}

impl Rig {
    /// `None`, after saying SKIP, where no X server with SYNC is named.
    fn new() -> Option<Rig> {
        if std::env::var_os("DISPLAY").is_none() {
            eprintln!("SKIP: no DISPLAY");
            return None;
        }
        let turn = super::shared_display_turn();
        let mut manager = Manager::new()?;
        let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        let window = Window::new(target, WindowAttributes::default().with_visible(false))
            .expect("an X11 window");
        let xwindow = window.id().0 as u32;
        manager
            .conn
            .send_void(xproto::change_window_attributes(
                xwindow,
                &ValueList::new().set(cw::OVERRIDE_REDIRECT, 1),
            ))
            .unwrap()
            .check()
            .expect("the window made override-redirect");
        let (type_, value) = manager.property32(xwindow, manager.sync_request_counter);
        assert_eq!(type_, atom::CARDINAL, "the counter's property is a CARDINAL");
        let [counter] = value[..] else {
            panic!("the window names one counter: {value:?}");
        };
        manager.counter = counter;
        Some(Rig { window, event_loop, manager, xwindow, _turn: turn })
    }

    /// Round trips of the stand-in's connection and then the window's: the server has handled
    /// what the stand-in sent, and the events it made for the window, which reach the window
    /// before the window's own reply, are in the window's queue.
    fn settle(&self) {
        let reply = self.manager.conn.send(xproto::get_input_focus()).unwrap().reply();
        reply.expect("GetInputFocus");
        self.window.xconn.sync_with_server().expect("the window's round trip");
    }

    /// Resize the window by `dw` x `dh` from its size now.
    fn grow(&self, dw: u32, dh: u32) {
        let size = self.window.inner_size();
        self.manager.configure(
            self.xwindow,
            &ValueList::new()
                .set(config::WIDTH, size.width + dw)
                .set(config::HEIGHT, size.height + dh),
        );
    }

    /// Pump the event loop one turn at a time, handing each window event to `on`, until `done`,
    /// failing the test after [`HANG`].
    fn until(
        &mut self,
        what: &str,
        mut on: impl FnMut(&Manager, &Window, WindowEvent),
        mut done: impl FnMut(&Manager, &Window) -> bool,
    ) {
        let Rig { window, event_loop, manager, .. } = self;
        let deadline = Instant::now() + HANG;
        while !done(manager, window) {
            event_loop.pump_events(Some(Duration::from_millis(5)), |event, _| {
                if let Event::WindowEvent { event, .. } = event {
                    on(manager, window, event);
                }
            });
            assert!(Instant::now() < deadline, "{what} never came in {HANG:?}");
        }
    }
}

#[test]
fn a_resize_is_answered_by_the_frame_drawn_at_its_size() {
    let Some(mut rig) = Rig::new() else {
        return;
    };
    let (type_, protocols) = rig.manager.property32(rig.xwindow, rig.manager.wm_protocols);
    assert_eq!(type_, atom::ATOM);
    assert_eq!(
        protocols,
        [rig.manager.wm_delete_window, rig.manager.net_wm_ping, rig.manager.sync_request],
        "WM_PROTOCOLS offers the sync request"
    );
    assert_eq!(rig.manager.counter(&rig.window), 0, "a new window's counter is 0");

    let value = (3 << 32) | 0x8000_0005;
    rig.manager.request(rig.xwindow, value);
    rig.grow(40, 30);
    rig.settle();
    let (at_resize, before, after) = (Cell::new(None), Cell::new(None), Cell::new(None));
    rig.until(
        "the frame after the resize",
        |manager, window, event| match event {
            WindowEvent::Resized(_) => {
                at_resize.set(Some(manager.counter(window)));
                window.request_redraw();
            },
            WindowEvent::RedrawRequested if at_resize.get().is_some() && after.get().is_none() => {
                before.set(Some(manager.counter(window)));
                window.frame_presented();
                after.set(Some(manager.counter(window)));
            },
            _ => {},
        },
        |_, _| after.get().is_some(),
    );
    assert_eq!(at_resize.get(), Some(0), "unanswered while the application handles the resize");
    assert_eq!(before.get(), Some(0), "unanswered until the frame is presented");
    assert_eq!(after.get(), Some(value), "the present answers it, high half and all");

    // The offer can succeed before a later constructor step fails, before any Window
    // wrapper owns the counter. Exercise that scope with a real offered counter: the
    // Err drops it, while the display and this unrelated live counter remain alive.
    let failed_counter = Cell::new(None);
    let failed: Result<(), ()> = {
        let _counter = SyncCounter::offer(&rig.window.xconn, rig.xwindow).expect("a counter");
        rig.window.xconn.sync_with_server().expect("the offer reaches the server");
        let (_, property) = rig.manager.property32(rig.xwindow, rig.manager.sync_request_counter);
        let [counter] = property[..] else { panic!("one offered counter: {property:?}") };
        assert_ne!(counter, rig.manager.counter, "a fresh counter was offered");
        failed_counter.set(Some(counter));
        Err(())
    };
    assert!(failed.is_err());
    rig.window.xconn.sync_with_server().expect("the failed scope's drop reaches the server");
    match rig
        .manager
        .conn
        .send(sync::query_counter(rig.manager.sync_major, failed_counter.get().expect("offered")))
        .unwrap()
        .reply()
    {
        Err(aterm_x11::Error::X11(e)) => {
            assert_eq!(e.error_code, rig.manager.bad_counter, "the failed scope's counter is gone")
        },
        other => panic!("the failed scope releases its counter: {other:?}"),
    }
    assert_eq!(rig.manager.counter(&rig.window), value, "the original counter stays live");

    let xconn = Arc::clone(&rig.window.xconn);
    let Rig { window, manager, .. } = rig;
    drop(window);
    xconn.sync_with_server().expect("the window's drop reaches the server");
    match manager
        .conn
        .send(sync::query_counter(manager.sync_major, manager.counter))
        .unwrap()
        .reply()
    {
        Err(aterm_x11::Error::X11(e)) => {
            assert_eq!(e.error_code, manager.bad_counter, "the counter is gone: {e:?}")
        },
        other => panic!("the window's drop destroys its counter: {other:?}"),
    }
}

#[test]
fn a_request_no_frame_follows_is_answered_anyway() {
    let Some(mut rig) = Rig::new() else {
        return;
    };
    let value = (7 << 32) | 1;
    rig.manager.request(rig.xwindow, value);
    rig.grow(40, 30);
    rig.settle();
    let at_resize = Cell::new(None);
    rig.until(
        "the resize",
        |manager, window, event| {
            if let WindowEvent::Resized(_) = event {
                at_resize.set(Some(manager.counter(window)));
            }
        },
        |_, _| at_resize.get().is_some(),
    );
    assert_eq!(at_resize.get(), Some(0), "unanswered while the application handles the resize");
    assert_eq!(
        rig.manager.counter(&rig.window),
        value,
        "answered by the end of the turn that brought it"
    );

    let value = value + 1;
    rig.manager.request(rig.xwindow, value);
    rig.manager.configure(rig.xwindow, &ValueList::new().set(config::X, 37).set(config::Y, 21));
    rig.settle();
    let mut resized = false;
    rig.until(
        "the answer to a move",
        |_, _, event| resized |= matches!(event, WindowEvent::Resized(_)),
        |manager, window| manager.counter(window) == value,
    );
    assert!(!resized, "a move is no resize");

    let value = value + 1;
    rig.manager.request(rig.xwindow, value);
    rig.settle();
    rig.until(
        "the answer to a request no configure follows",
        |_, _, _| {},
        |manager, window| manager.counter(window) == value,
    );
}
