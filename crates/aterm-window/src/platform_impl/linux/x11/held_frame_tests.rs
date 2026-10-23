// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! X11 tiled and fullscreen held-frame state tests.

use std::time::{Duration, Instant};

use aterm_x11::proto::atom;
use aterm_x11::proto::xproto::{self, event_mask, PropMode};
use aterm_x11::Connection;

use super::Window;
use crate::dpi::{PhysicalPosition, PhysicalSize};
use crate::event::{Event, WindowEvent};
use crate::platform_impl::{ActiveEventLoop as PlatformActiveEventLoop, EventLoop, Fullscreen};
use crate::window::WindowAttributes;

/// The hang detector: how long a wait for what must happen may take, which it never needs.
const HANG: Duration = Duration::from_secs(60);

/// `ConfigureNotify`'s event code.
const CONFIGURE_NOTIFY: u8 = 22;

/// A window's frame: where it is and how big.
type Frame = (PhysicalPosition<i32>, PhysicalSize<u32>);

fn frame(x: i32, y: i32, width: u32, height: u32) -> Frame {
    (PhysicalPosition::new(x, y), PhysicalSize::new(width, height))
}

/// Returns whether the window's frame is held by the window manager (`app_restore::frame_held`).
fn held(window: &Window) -> bool {
    window.is_maximized() || window.is_tiled() || window.is_shown_fullscreen()
}

/// Builds a synthetic `ConfigureNotify` event for `window` at `(x, y)` with `width` by `height`.
fn configure_notify(window: u32, (x, y): (i16, i16), (width, height): (u16, u16)) -> [u8; 32] {
    let mut event = [0; 32];
    event[0] = CONFIGURE_NOTIFY;
    event[4..8].copy_from_slice(&window.to_ne_bytes());
    event[8..12].copy_from_slice(&window.to_ne_bytes());
    event[16..18].copy_from_slice(&x.to_ne_bytes());
    event[18..20].copy_from_slice(&y.to_ne_bytes());
    event[20..22].copy_from_slice(&width.to_ne_bytes());
    event[22..24].copy_from_slice(&height.to_ne_bytes());
    event
}

/// The `_NET_WM_STATE` atoms the stand-in sets.
struct States {
    vert: u32,
    horz: u32,
    fullscreen: u32,
}

/// The stand-in window manager: its own connection, and the atoms it sets.
struct StandIn {
    conn: Connection,
    window: u32,
    net_wm_state: u32,
}

impl StandIn {
    /// Connect to `$DISPLAY` for `window`: the stand-in and the atoms it sets.
    fn connect(window: &Window) -> (Self, States) {
        let (conn, _) =
            Connection::connect(None).expect("DISPLAY is set: the connection must open");
        let [net_wm_state, vert, horz, fullscreen] = conn
            .intern_atoms(&[
                b"_NET_WM_STATE",
                b"_NET_WM_STATE_MAXIMIZED_VERT",
                b"_NET_WM_STATE_MAXIMIZED_HORZ",
                b"_NET_WM_STATE_FULLSCREEN",
            ])
            .unwrap()[..]
        else {
            unreachable!()
        };
        let stand_in = Self { conn, window: window.id().0 as u32, net_wm_state };
        (stand_in, States { vert, horz, fullscreen })
    }

    /// Applies `states` and configures the window at `at` and `size`.
    fn show(&self, states: &[u32], at: (i16, i16), size: (u16, u16)) {
        let state = if states.is_empty() {
            xproto::delete_property(self.window, self.net_wm_state)
        } else {
            xproto::change_property32(
                PropMode::Replace,
                self.window,
                self.net_wm_state,
                atom::ATOM,
                states,
            )
        };
        self.conn.send_void(state).unwrap().check().expect("the stand-in sets the state");
        let moved = configure_notify(self.window, at, size);
        self.conn
            .send_void(xproto::send_event(false, self.window, event_mask::STRUCTURE_NOTIFY, &moved))
            .unwrap()
            .check()
            .expect("the stand-in sends the move");
    }
}

/// What the tracker made of the window's `Moved` and `Resized` ([`held`]).
#[derive(Debug, Default)]
struct Tracked {
    held: (Option<PhysicalPosition<i32>>, Option<PhysicalSize<u32>>),
    control: (Option<PhysicalPosition<i32>>, Option<PhysicalSize<u32>>),
    /// The last `Moved` and `Resized` the loop reported.
    shown: (Option<PhysicalPosition<i32>>, Option<PhysicalSize<u32>>),
}

impl Tracked {
    fn held(&self) -> Option<Frame> {
        Some((self.held.0?, self.held.1?))
    }

    fn control(&self) -> Option<Frame> {
        Some((self.control.0?, self.control.1?))
    }
}

/// A window on its own X11 event loop, the stand-in window manager that holds its frame.
struct Rig {
    event_loop: EventLoop<()>,
    window: Window,
    wm: StandIn,
    tracked: Tracked,
    /// The rule the fix under test replaced: the frame it holds the window off.
    control: fn(&Window) -> bool,
}

impl Rig {
    fn new(control: fn(&Window) -> bool) -> (Self, States) {
        let event_loop = EventLoop::<()>::new_x11_any_thread().expect("an X11 loop on $DISPLAY");
        let PlatformActiveEventLoop::X(target) = &event_loop.window_target().p else {
            panic!("an X11 loop");
        };
        let window = Window::new(target, WindowAttributes::default().with_visible(false))
            .expect("an X11 window");
        let (wm, states) = StandIn::connect(&window);
        (Self { event_loop, window, wm, tracked: Tracked::default(), control }, states)
    }

    /// The stand-in shows the window in `states` at `shown` ([`HANG`]).
    fn show(&mut self, states: &[u32], shown: Frame) {
        let (at, size) = shown;
        self.wm.show(states, (at.x as i16, at.y as i16), (size.width as u16, size.height as u16));
        let Self { event_loop, window, tracked, control, .. } = self;
        let deadline = Instant::now() + HANG;
        while tracked.shown != (Some(shown.0), Some(shown.1)) {
            event_loop.pump_events(Some(Duration::from_millis(5)), |event, _| {
                let Event::WindowEvent { event, .. } = event else { return };
                let (held, control) = (held(window), control(window));
                match event {
                    WindowEvent::Moved(position) => {
                        tracked.shown.0 = Some(position);
                        if !held {
                            tracked.held.0 = Some(position);
                        }
                        if !control {
                            tracked.control.0 = Some(position);
                        }
                    },
                    WindowEvent::Resized(size) => {
                        tracked.shown.1 = Some(size);
                        if !held {
                            tracked.held.1 = Some(size);
                        }
                        if !control {
                            tracked.control.1 = Some(size);
                        }
                    },
                    _ => {},
                }
            });
            assert!(
                Instant::now() < deadline,
                "the window was never reported at {shown:?} in {HANG:?}: {tracked:?}"
            );
        }
    }
}

/// Whether `$DISPLAY` names an X server to test on; SKIP otherwise.
fn have_display() -> bool {
    let have = std::env::var_os("DISPLAY").is_some();
    if !have {
        eprintln!("SKIP: no DISPLAY");
    }
    have
}

#[test]
fn a_tiled_windows_frame_is_never_its_normal_one() {
    if !have_display() {
        return;
    }
    let _turn = super::shared_display_turn();
    let (mut rig, States { vert, horz, .. }) = Rig::new(|window| window.is_maximized());

    let normal = frame(300, 200, 640, 400);
    rig.show(&[], normal);
    assert!(!held(&rig.window));
    assert_eq!(rig.tracked.held(), Some(normal));

    rig.show(&[vert], frame(0, 0, 800, 1000));
    assert!(rig.window.is_tiled(), "a window maximized vertically alone is tiled");
    assert!(!rig.window.is_maximized(), "and not maximized");
    assert_eq!(rig.tracked.held(), Some(normal), "the tile's frame is not the normal one");
    assert_eq!(
        rig.tracked.control(),
        Some(frame(0, 0, 800, 1000)),
        "negative control: the both-flags rule takes the tiled frame for the normal one"
    );

    rig.show(&[vert, horz], frame(0, 0, 1600, 1000));
    assert!(rig.window.is_maximized() && !rig.window.is_tiled(), "both directions are maximized");
    rig.show(&[vert], frame(800, 0, 800, 1000));
    assert_eq!(rig.tracked.held(), Some(normal));

    rig.show(&[], normal);
    rig.show(&[], frame(320, 210, 660, 420));
    assert!(!rig.window.is_tiled());
    assert_eq!(rig.tracked.held(), Some(frame(320, 210, 660, 420)), "a floating window's frame is");
}

#[test]
fn a_full_screen_windows_frame_is_never_its_normal_one() {
    if !have_display() {
        return;
    }
    let _turn = super::shared_display_turn();
    let (mut rig, States { vert, fullscreen, .. }) =
        Rig::new(|window| window.is_maximized() || window.is_tiled());

    let normal = frame(300, 200, 640, 400);
    rig.show(&[], normal);
    assert!(!rig.window.is_shown_fullscreen());
    assert_eq!(rig.tracked.held(), Some(normal));

    rig.window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    assert!(rig.window.is_shown_fullscreen(), "a full screen asked for is held");
    rig.window.set_fullscreen(None);
    assert!(!rig.window.is_shown_fullscreen(), "and no longer once left");

    rig.show(&[fullscreen], frame(0, 0, 1600, 1000));
    assert!(rig.window.is_shown_fullscreen(), "_NET_WM_STATE_FULLSCREEN is a full screen");
    assert!(
        !rig.window.is_maximized() && !rig.window.is_tiled(),
        "and neither maximized nor tiled"
    );
    assert_eq!(rig.tracked.held(), Some(normal), "the screen's frame is not the normal one");
    assert_eq!(
        rig.tracked.control(),
        Some(frame(0, 0, 1600, 1000)),
        "negative control: the maximized-or-tiled rule takes the full screen for the normal frame"
    );

    rig.show(&[vert], frame(0, 0, 800, 1000));
    rig.show(&[vert, fullscreen], frame(0, 0, 1600, 1000));
    assert!(rig.window.is_shown_fullscreen());
    assert_eq!(rig.tracked.held(), Some(normal));

    rig.show(&[], normal);
    rig.show(&[], frame(320, 210, 660, 420));
    assert!(!rig.window.is_shown_fullscreen());
    assert_eq!(rig.tracked.held(), Some(frame(320, 210, 660, 420)), "a floating window's frame is");
}
