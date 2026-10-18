// Copyright (c) 2018 Victor Berger
// SPDX-License-Identifier: MIT
//
// Derived from smithay-client-toolkit 0.19.2 src/seat/pointer/mod.rs (the
// `wl_pointer` event handling: a frame's grouping and its axis merge) and
// modified by the aterm project in 2026. See ../LICENSE-MIT for the upstream
// MIT license.

//! A pointer's events, grouped by `wl_pointer.frame` as the winit fork has
//! seen them through smithay-client-toolkit until LW.7: each event names the
//! surface the pointer is on and its position there; a frame's axis events
//! merge into one (both axes, the source, discrete steps and the stop); a
//! pointer before version 5, which sends no frames, hands each event out at
//! once. An event with no entered surface is dropped.
//!
//! [`Frames`] is the state machine, fed the decoded events one at a time
//! ([`crate::seat`] decodes them); it is generic over the surface, so it is
//! tested without a compositor.

/// A scroll along one axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AxisScroll {
    /// In surface-local pixels.
    pub absolute: f64,
    /// In steps (a wheel's clicks); 0 from a continuous source.
    pub discrete: i32,
    /// The scroll stopped (a finger lifted).
    pub stop: bool,
}

impl AxisScroll {
    /// Whether nothing moved along this axis.
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    fn merge(&mut self, other: &Self) {
        self.absolute += other.absolute;
        self.discrete += other.discrete;
        self.stop |= other.stop;
    }
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerKind {
    Enter {
        serial: u32,
    },
    Leave {
        serial: u32,
    },
    Motion {
        time: u32,
    },
    Press {
        time: u32,
        button: u32,
        serial: u32,
    },
    Release {
        time: u32,
        button: u32,
        serial: u32,
    },
    Axis {
        /// 0 when no event of the frame said.
        time: u32,
        horizontal: AxisScroll,
        vertical: AxisScroll,
        /// `wl_pointer.axis_source`, when the frame named one.
        source: Option<u32>,
    },
}

/// One pointer event: on which surface, where, what.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent<S> {
    pub surface: S,
    /// Surface-local, as of this event.
    pub position: (f64, f64),
    pub kind: PointerKind,
}

/// A `wl_pointer` event as the wire carries it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Raw<S> {
    /// `surface` is `None` when the client destroyed it already.
    Enter {
        serial: u32,
        surface: Option<S>,
        x: f64,
        y: f64,
    },
    Leave {
        serial: u32,
        surface: Option<S>,
    },
    Motion {
        time: u32,
        x: f64,
        y: f64,
    },
    Button {
        serial: u32,
        time: u32,
        button: u32,
        state: u32,
    },
    Axis {
        time: u32,
        axis: u32,
        value: f64,
    },
    Frame,
    AxisSource(u32),
    AxisStop {
        time: u32,
        axis: u32,
    },
    AxisDiscrete {
        axis: u32,
        discrete: i32,
    },
}

const VERTICAL: u32 = 0;
const HORIZONTAL: u32 = 1;
/// `wl_pointer.axis_source`'s values run to `wheel_tilt` (3).
const LAST_SOURCE: u32 = 3;
const RELEASED: u32 = 0;
const PRESSED: u32 = 1;

/// One pointer's state between frames.
#[derive(Debug)]
pub struct Frames<S> {
    version: u32,
    /// The surface the pointer last entered, until it leaves.
    surface: Option<S>,
    position: (f64, f64),
    /// The frame being built (version 5 and later).
    pending: Vec<PointerEvent<S>>,
    latest_enter: Option<u32>,
    latest_button: Option<u32>,
}

/// One axis scroll's two axes, `value` on `axis` (`None` for an axis the
/// protocol does not name).
fn on_axis(axis: u32, set: impl FnOnce(&mut AxisScroll)) -> Option<(AxisScroll, AxisScroll)> {
    let (mut horizontal, mut vertical) = (AxisScroll::default(), AxisScroll::default());
    match axis {
        VERTICAL => set(&mut vertical),
        HORIZONTAL => set(&mut horizontal),
        _ => {
            aterm_log::warn!("wayland: pointer axis {axis:#x} is not one the protocol names");
            return None;
        }
    }
    Some((horizontal, vertical))
}

impl<S: Copy + PartialEq> Frames<S> {
    /// The state of a fresh `wl_pointer` of `version`.
    pub fn new(version: u32) -> Frames<S> {
        Frames {
            version,
            surface: None,
            position: (0.0, 0.0),
            pending: Vec::new(),
            latest_enter: None,
            latest_button: None,
        }
    }

    /// The serial of the newest `enter`.
    pub fn latest_enter(&self) -> Option<u32> {
        self.latest_enter
    }

    /// The serial of the newest `button`, pressed or released.
    pub fn latest_button(&self) -> Option<u32> {
        self.latest_button
    }

    /// The surface the pointer is on.
    pub fn surface(&self) -> Option<S> {
        self.surface
    }

    /// Take one event; a frame that completes with it comes back.
    pub fn feed(&mut self, raw: Raw<S>) -> Option<Vec<PointerEvent<S>>> {
        let mut leave_surface = None;
        let kind = match raw {
            Raw::Enter {
                serial,
                surface,
                x,
                y,
            } => {
                self.surface = surface;
                self.position = (x, y);
                self.latest_enter = Some(serial);
                PointerKind::Enter { serial }
            }
            Raw::Leave { serial, surface } => {
                // A surface the client destroyed arrives as none: the
                // pointer has left whatever it was on.
                let left = surface.or(self.surface);
                if self.surface == left {
                    self.surface = None;
                }
                leave_surface = left;
                PointerKind::Leave { serial }
            }
            Raw::Motion { time, x, y } => {
                self.position = (x, y);
                PointerKind::Motion { time }
            }
            Raw::Button {
                serial,
                time,
                button,
                state,
            } => {
                self.latest_button = Some(serial);
                match state {
                    PRESSED => PointerKind::Press {
                        time,
                        button,
                        serial,
                    },
                    RELEASED => PointerKind::Release {
                        time,
                        button,
                        serial,
                    },
                    other => {
                        aterm_log::warn!("wayland: pointer button state {other:#x} is unknown");
                        return None;
                    }
                }
            }
            Raw::Axis { time, axis, value } => {
                let (horizontal, vertical) = on_axis(axis, |a| a.absolute = value)?;
                PointerKind::Axis {
                    time,
                    horizontal,
                    vertical,
                    source: None,
                }
            }
            Raw::AxisSource(source) => {
                if source > LAST_SOURCE {
                    aterm_log::warn!("wayland: pointer axis source {source:#x} is unknown");
                    return None;
                }
                PointerKind::Axis {
                    time: 0,
                    horizontal: AxisScroll::default(),
                    vertical: AxisScroll::default(),
                    source: Some(source),
                }
            }
            Raw::AxisStop { time, axis } => {
                let (horizontal, vertical) = on_axis(axis, |a| a.stop = true)?;
                PointerKind::Axis {
                    time,
                    horizontal,
                    vertical,
                    source: None,
                }
            }
            Raw::AxisDiscrete { axis, discrete } => {
                let (horizontal, vertical) = on_axis(axis, |a| a.discrete = discrete)?;
                PointerKind::Axis {
                    time: 0,
                    horizontal,
                    vertical,
                    source: None,
                }
            }
            Raw::Frame => {
                let pending = std::mem::take(&mut self.pending);
                return (!pending.is_empty()).then_some(pending);
            }
        };
        let Some(surface) = leave_surface.or(self.surface) else {
            aterm_log::warn!("wayland: a pointer event with no entered surface: {kind:?}");
            return None;
        };
        let event = PointerEvent {
            surface,
            position: self.position,
            kind,
        };
        if self.version < 5 {
            // No frames before version 5: each event is its own.
            return Some(vec![event]);
        }
        // A frame's axis events merge into the one before them: one scroll,
        // diagonal when both axes moved.
        if let (
            Some(PointerEvent {
                kind:
                    PointerKind::Axis {
                        time: ot,
                        horizontal: oh,
                        vertical: ov,
                        source: os,
                    },
                ..
            }),
            PointerKind::Axis {
                time: nt,
                horizontal: nh,
                vertical: nv,
                source: ns,
            },
        ) = (self.pending.last_mut(), &event.kind)
        {
            // A time of 0 is "not said": take a real one.
            if *ot == 0 {
                *ot = *nt;
            }
            oh.merge(nh);
            ov.merge(nv);
            *os = os.or(*ns);
            return None;
        }
        self.pending.push(event);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: u32 = 1;
    const B: u32 = 2;

    fn enter(f: &mut Frames<u32>, s: u32) -> Option<Vec<PointerEvent<u32>>> {
        f.feed(Raw::Enter {
            serial: 10,
            surface: Some(s),
            x: 1.0,
            y: 2.0,
        })
    }

    #[test]
    fn a_frame_hands_out_its_events_together() {
        let mut f = Frames::new(5);
        assert_eq!(enter(&mut f, A), None);
        assert_eq!(
            f.feed(Raw::Motion {
                time: 7,
                x: 3.0,
                y: 4.0
            }),
            None
        );
        let frame = f.feed(Raw::Frame).expect("a frame");
        assert_eq!(frame.len(), 2);
        assert_eq!(frame[0].kind, PointerKind::Enter { serial: 10 });
        assert_eq!(frame[0].position, (1.0, 2.0));
        assert_eq!(frame[1].kind, PointerKind::Motion { time: 7 });
        assert_eq!(frame[1].position, (3.0, 4.0));
        assert_eq!(f.feed(Raw::Frame), None, "an empty frame is nothing");
        assert_eq!(f.latest_enter(), Some(10));
    }

    #[test]
    fn before_version_five_each_event_is_its_own() {
        let mut f = Frames::new(4);
        let got = enter(&mut f, A).expect("at once");
        assert_eq!(got.len(), 1);
        let got = f
            .feed(Raw::Button {
                serial: 11,
                time: 8,
                button: 0x110,
                state: PRESSED,
            })
            .expect("at once");
        assert_eq!(
            got[0].kind,
            PointerKind::Press {
                time: 8,
                button: 0x110,
                serial: 11
            }
        );
        assert_eq!(f.latest_button(), Some(11));
    }

    #[test]
    fn a_frame_s_axis_events_merge_into_one_scroll() {
        let mut f = Frames::new(5);
        enter(&mut f, A);
        f.feed(Raw::Frame);
        f.feed(Raw::AxisSource(0));
        f.feed(Raw::Axis {
            time: 9,
            axis: VERTICAL,
            value: 15.0,
        });
        f.feed(Raw::AxisDiscrete {
            axis: VERTICAL,
            discrete: 1,
        });
        f.feed(Raw::Axis {
            time: 9,
            axis: HORIZONTAL,
            value: -3.0,
        });
        f.feed(Raw::AxisStop {
            time: 9,
            axis: HORIZONTAL,
        });
        let frame = f.feed(Raw::Frame).expect("a frame");
        assert_eq!(frame.len(), 1, "{frame:?}");
        assert_eq!(
            frame[0].kind,
            PointerKind::Axis {
                time: 9,
                horizontal: AxisScroll {
                    absolute: -3.0,
                    discrete: 0,
                    stop: true
                },
                vertical: AxisScroll {
                    absolute: 15.0,
                    discrete: 1,
                    stop: false
                },
                source: Some(0),
            }
        );
    }

    #[test]
    fn a_leave_names_the_surface_left_and_events_after_it_are_dropped() {
        let mut f = Frames::new(5);
        enter(&mut f, A);
        f.feed(Raw::Leave {
            serial: 12,
            surface: Some(A),
        });
        f.feed(Raw::Motion {
            time: 1,
            x: 0.0,
            y: 0.0,
        });
        let frame = f.feed(Raw::Frame).expect("a frame");
        assert_eq!(frame.len(), 2, "the motion after the leave is dropped");
        assert_eq!(frame[1].kind, PointerKind::Leave { serial: 12 });
        assert_eq!(frame[1].surface, A);
        assert_eq!(f.surface(), None);
    }

    #[test]
    fn a_leave_of_a_destroyed_surface_leaves_the_one_entered() {
        let mut f = Frames::new(5);
        enter(&mut f, B);
        f.feed(Raw::Leave {
            serial: 13,
            surface: None,
        });
        let frame = f.feed(Raw::Frame).expect("a frame");
        assert_eq!(frame[1].surface, B);
        assert_eq!(f.surface(), None);
    }

    #[test]
    fn unknown_values_are_dropped_not_guessed() {
        let mut f = Frames::new(5);
        enter(&mut f, A);
        f.feed(Raw::Axis {
            time: 1,
            axis: 7,
            value: 1.0,
        });
        f.feed(Raw::AxisSource(9));
        f.feed(Raw::Button {
            serial: 14,
            time: 1,
            button: 0x110,
            state: 5,
        });
        let frame = f.feed(Raw::Frame).expect("the enter");
        assert_eq!(frame.len(), 1, "{frame:?}");
    }
}
