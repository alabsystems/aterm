// Copyright (c) 2018 Victor Berger
// SPDX-License-Identifier: MIT
//
// Derived from smithay-client-toolkit 0.19.2 src/seat/pointer/mod.rs (the
// `wl_pointer` event handling: a frame's grouping and its axis merge) and
// modified by the aterm project in 2026. See ../LICENSE-MIT for the upstream
// MIT license.

//! `wl_pointer.frame` event grouping state machine ([`Frames`], [`crate::seat`]).

/// A scroll along one axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AxisScroll {
    /// In surface-local pixels.
    pub absolute: f64,
    /// In steps (a wheel's clicks).
    pub discrete: i32,
    /// The scroll stopped (a finger lifted).
    pub stop: bool,
}

impl AxisScroll {
    /// Whether nothing moved along this axis.
    #[inline]
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    #[inline]
    fn merge(&mut self, other: &Self) {
        self.absolute += other.absolute;
        self.discrete = self.discrete.saturating_add(other.discrete);
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

/// One pointer event.
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
    #[inline]
    pub const fn new(version: u32) -> Frames<S> {
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
    #[inline]
    pub const fn latest_enter(&self) -> Option<u32> {
        self.latest_enter
    }

    /// The serial of the newest `button`, pressed or released.
    #[inline]
    pub const fn latest_button(&self) -> Option<u32> {
        self.latest_button
    }

    /// The surface the pointer is on.
    #[inline]
    pub const fn surface(&self) -> Option<S> {
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
            return Some(vec![event]);
        }
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
#[path = "pointer_frame_tests.rs"]
mod tests;
