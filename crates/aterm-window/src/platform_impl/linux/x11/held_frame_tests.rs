// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A window whose frame the window manager holds says so, so that frame is never taken for its
//! normal one: tiled to a screen edge (`Window::is_tiled`) or full screen however it got there
//! (`Window::is_shown_fullscreen`). GNOME's edge tiling (Super+Left/Right, a drag to an edge)
//! sets `_NET_WM_STATE_MAXIMIZED_VERT` alone, and `is_maximized` asks for both directions; a full
//! screen is `_NET_WM_STATE_FULLSCREEN`, which `is_maximized` does not read either. So aterm-gui
//! recorded a tiled window's half-screen frame, and a full-screen window's screen, as the frame
//! it restores to: after a restart it reopened floating at that size and place (the desk audit,
//! 2026-10-22).
//!
//! A stand-in window manager on its own connection does what one does to the hidden window (no
//! real window manager manages a window that was never mapped): it places it, sets the state, and
//! sends the synthetic `ConfigureNotify` ICCCM 4.1.5 has it send for every move, from which the
//! event loop reports `Moved` and `Resized`. At each, the test applies the rule aterm-gui's
//! normal-frame tracker applies (`app_restore::frame_held`, feeding `NormalFrame` through
//! `WindowState::note_normal_origin` and `note_normal_grid`): a frame is the window's normal one
//! only while it shows neither maximized, tiled nor full screen. Through a tile or a full screen
//! the normal frame stays the one before it, and after it ends the tracker follows the window
//! again. Each test's negative control is in the test: the rule its fix replaced records the
//! held frame.
//!
//! It runs where `$DISPLAY` names an X server; elsewhere it says SKIP and returns.

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

/// The rule aterm-gui's tracker holds a window off its normal frame by, off Windows
/// (`app_restore::frame_held`).
fn held(window: &Window) -> bool {
    window.is_maximized() || window.is_tiled() || window.is_shown_fullscreen()
}

/// A synthetic `ConfigureNotify` for `window` at (`x`, `y`) on the root, `width` × `height`, as a
/// window manager sends one (ICCCM 4.1.5): root coordinates, no border, no sibling.
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

    /// Show the window in `states` (`_NET_WM_STATE`'s atoms, none for a normal window) at `at`,
    /// `size`: the state first, then the move, as mutter sets them.
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

/// What the tracker made of the window's `Moved` and `Resized`: the normal frame under the rule
/// aterm-gui reads ([`held`]), and under the rule a fix replaced (`control`), each the last
/// position and size it recorded.
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

/// A window on its own X11 event loop, the stand-in window manager that holds its frame, and
/// what the tracker made of it.
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

    /// The stand-in shows the window in `states` at `shown`, and the loop is pumped until it has
    /// reported that frame, feeding each `Moved` and `Resized` to the trackers as it comes; the
    /// test fails after [`HANG`].
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
    // The rule the tile fix replaced: held only while maximized in both directions.
    let (mut rig, States { vert, horz, .. }) = Rig::new(|window| window.is_maximized());

    // Floating where the user put it: the normal frame, under either rule.
    let normal = frame(300, 200, 640, 400);
    rig.show(&[], normal);
    assert!(!held(&rig.window));
    assert_eq!(rig.tracked.held(), Some(normal));

    // Super+Left: maximized vertically alone, on the left half.
    rig.show(&[vert], frame(0, 0, 800, 1000));
    assert!(rig.window.is_tiled(), "a window maximized vertically alone is tiled");
    assert!(!rig.window.is_maximized(), "and not maximized");
    assert_eq!(rig.tracked.held(), Some(normal), "the tile's frame is not the normal one");
    assert_eq!(
        rig.tracked.control(),
        Some(frame(0, 0, 800, 1000)),
        "negative control: the both-flags rule takes the tiled frame for the normal one"
    );

    // Maximized from the tile, then tiled to the other side: the normal frame stays put.
    rig.show(&[vert, horz], frame(0, 0, 1600, 1000));
    assert!(rig.window.is_maximized() && !rig.window.is_tiled(), "both directions are maximized");
    rig.show(&[vert], frame(800, 0, 800, 1000));
    assert_eq!(rig.tracked.held(), Some(normal));

    // Untiled: floating again, and the tracker follows a move.
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
    // The rule the full-screen fix replaced: held only while maximized or tiled.
    let (mut rig, States { vert, fullscreen, .. }) =
        Rig::new(|window| window.is_maximized() || window.is_tiled());

    // Floating where the user put it: the normal frame, under either rule.
    let normal = frame(300, 200, 640, 400);
    rig.show(&[], normal);
    assert!(!rig.window.is_shown_fullscreen());
    assert_eq!(rig.tracked.held(), Some(normal));

    // A full screen this window asks for counts before the window manager shows it (aterm-window
    // moves the window to the monitor's origin first, so that `Moved` can come first), and stops
    // counting when it asks to leave.
    rig.window.set_fullscreen(Some(Fullscreen::Borderless(None)));
    assert!(rig.window.is_shown_fullscreen(), "a full screen asked for is held");
    rig.window.set_fullscreen(None);
    assert!(!rig.window.is_shown_fullscreen(), "and no longer once left");

    // The window manager's own full screen (a key or rule of its own: aterm asked for nothing).
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

    // Full screen from a tile, and back to the tile: still not the normal frame.
    rig.show(&[vert], frame(0, 0, 800, 1000));
    rig.show(&[vert, fullscreen], frame(0, 0, 1600, 1000));
    assert!(rig.window.is_shown_fullscreen());
    assert_eq!(rig.tracked.held(), Some(normal));

    // Out of full screen: floating again, and the tracker follows a move.
    rig.show(&[], normal);
    rig.show(&[], frame(320, 210, 660, 420));
    assert!(!rig.window.is_shown_fullscreen());
    assert_eq!(rig.tracked.held(), Some(frame(320, 210, 660, 420)), "a floating window's frame is");
}
