// Copyright 2019 x11rb Contributers
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from x11rb 0.13.2 (MIT OR Apache-2.0, used under Apache-2.0)
// src/properties.rs (`WmClass`, `WmSizeHints`, `WmHints` and their parsers),
// and rewritten by the aterm project in 2026 over this crate's codec: the
// property bytes in and out, no connection.

//! ICCCM properties (section 4.1.2): `WM_CLASS`, `WM_NORMAL_HINTS`
//! (`WM_SIZE_HINTS`) and `WM_HINTS`, encoded as x11rb encoded them and read
//! as it read them, short legacy forms included.

use crate::proto::xproto::{GetPropertyReply, PropMode, change_property};
use crate::proto::{Atom, ParseError, Pixmap, Reader, Request, Void, Window, atom};

/// The number of 32-bit items in a `WM_SIZE_HINTS` property.
pub const WM_SIZE_HINTS_ITEMS: u32 = 18;
/// The number of 32-bit items in a `WM_HINTS` property.
pub const WM_HINTS_ITEMS: u32 = 9;

const US_POSITION: u32 = 1;
const US_SIZE: u32 = 1 << 1;
const P_POSITION: u32 = 1 << 2;
const P_SIZE: u32 = 1 << 3;
const P_MIN_SIZE: u32 = 1 << 4;
const P_MAX_SIZE: u32 = 1 << 5;
const P_RESIZE_INC: u32 = 1 << 6;
const P_ASPECT: u32 = 1 << 7;
const P_BASE_SIZE: u32 = 1 << 8;
const P_WIN_GRAVITY: u32 = 1 << 9;

const HINT_INPUT: u32 = 1;
const HINT_STATE: u32 = 1 << 1;
const HINT_ICON_PIXMAP: u32 = 1 << 2;
const HINT_ICON_WINDOW: u32 = 1 << 3;
const HINT_ICON_POSITION: u32 = 1 << 4;
const HINT_ICON_MASK: u32 = 1 << 5;
const HINT_WINDOW_GROUP: u32 = 1 << 6;
const HINT_URGENCY: u32 = 1 << 8;

/// `NorthWest`, the gravity a pre-ICCCM-1 short `WM_SIZE_HINTS` implies.
pub const GRAVITY_NORTH_WEST: u32 = 1;

/// `WM_CLASS`: the instance and class names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WmClass {
    pub instance: Vec<u8>,
    pub class: Vec<u8>,
}

impl WmClass {
    /// Read a `WM_CLASS` property: `None` when it is absent, an error when it
    /// is not 8-bit `STRING` data.
    pub fn from_reply(reply: &GetPropertyReply) -> Result<Option<WmClass>, ParseError> {
        if reply.type_ == atom::ANY {
            return Ok(None);
        }
        if reply.type_ != atom::STRING || reply.format != 8 {
            return Err(ParseError::Invalid);
        }
        let v = &reply.value;
        let split = v.iter().position(|&b| b == 0).unwrap_or(v.len());
        let start = split + 1;
        let class = if start >= v.len() {
            Vec::new()
        } else {
            let end = if v.last() == Some(&0) {
                v.len() - 1
            } else {
                v.len()
            };
            v.get(start..end).unwrap_or_default().to_vec()
        };
        Ok(Some(WmClass {
            instance: v[..split].to_vec(),
            class,
        }))
    }

    /// The ChangeProperty that sets it: `instance\0class\0`.
    pub fn set(&self, window: Window) -> Request<Void> {
        let mut data = Vec::with_capacity(self.instance.len() + self.class.len() + 2);
        data.extend_from_slice(&self.instance);
        data.push(0);
        data.extend_from_slice(&self.class);
        data.push(0);
        change_property(
            PropMode::Replace,
            window,
            atom::WM_CLASS,
            atom::STRING,
            8,
            &data,
        )
    }
}

/// Who chose a size hint's position or size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Specified {
    User,
    Program,
}

/// `WM_SIZE_HINTS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WmSizeHints {
    pub position: Option<(Specified, i32, i32)>,
    pub size: Option<(Specified, i32, i32)>,
    pub min_size: Option<(i32, i32)>,
    pub max_size: Option<(i32, i32)>,
    pub size_increment: Option<(i32, i32)>,
    /// Minimum and maximum aspect ratios, each (numerator, denominator).
    pub aspect: Option<((i32, i32), (i32, i32))>,
    pub base_size: Option<(i32, i32)>,
    pub win_gravity: Option<u32>,
}

fn pair(r: &mut Reader<'_>) -> Result<(i32, i32), ParseError> {
    Ok((r.i32()?, r.i32()?))
}

fn flagged<T>(flags: u32, bit: u32, value: T) -> Option<T> {
    (flags & bit != 0).then_some(value)
}

impl WmSizeHints {
    /// Parse the property's value (`format` 32 items).
    pub fn parse(value: &[u8]) -> Result<WmSizeHints, ParseError> {
        let mut r = Reader::new(value);
        let flags = r.u32()?;
        let (x, y) = pair(&mut r)?;
        let (width, height) = pair(&mut r)?;
        let min_size = flagged(flags, P_MIN_SIZE, pair(&mut r)?);
        let max_size = flagged(flags, P_MAX_SIZE, pair(&mut r)?);
        let size_increment = flagged(flags, P_RESIZE_INC, pair(&mut r)?);
        let aspect = flagged(flags, P_ASPECT, (pair(&mut r)?, pair(&mut r)?));
        // A pre-ICCCM-1 property stops after the aspect: its base size is
        // the minimum and its gravity NorthWest, as Xlib reads it.
        let (base_size, win_gravity) = if value.len() <= 60 {
            (min_size, Some(GRAVITY_NORTH_WEST))
        } else {
            let base = flagged(flags, P_BASE_SIZE, pair(&mut r)?);
            let gravity = flagged(flags, P_WIN_GRAVITY, r.u32()?);
            (base, gravity)
        };
        let position = if flags & US_POSITION != 0 {
            Some((Specified::User, x, y))
        } else if flags & P_POSITION != 0 {
            Some((Specified::Program, x, y))
        } else {
            None
        };
        let size = if flags & US_SIZE != 0 {
            Some((Specified::User, width, height))
        } else if flags & P_SIZE != 0 {
            Some((Specified::Program, width, height))
        } else {
            None
        };
        Ok(WmSizeHints {
            position,
            size,
            min_size,
            max_size,
            size_increment,
            aspect,
            base_size,
            win_gravity,
        })
    }

    /// Read a `WM_SIZE_HINTS` property: `None` when it is absent.
    pub fn from_reply(reply: &GetPropertyReply) -> Result<Option<WmSizeHints>, ParseError> {
        if reply.type_ == atom::ANY {
            return Ok(None);
        }
        if reply.type_ != atom::WM_SIZE_HINTS || reply.format != 32 {
            return Err(ParseError::Invalid);
        }
        WmSizeHints::parse(&reply.value).map(Some)
    }

    /// The property's 72 bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut flags = 0;
        flags |= match self.position {
            Some((Specified::User, ..)) => US_POSITION,
            Some((Specified::Program, ..)) => P_POSITION,
            None => 0,
        };
        flags |= match self.size {
            Some((Specified::User, ..)) => US_SIZE,
            Some((Specified::Program, ..)) => P_SIZE,
            None => 0,
        };
        flags |= self.min_size.map_or(0, |_| P_MIN_SIZE);
        flags |= self.max_size.map_or(0, |_| P_MAX_SIZE);
        flags |= self.size_increment.map_or(0, |_| P_RESIZE_INC);
        flags |= self.aspect.map_or(0, |_| P_ASPECT);
        flags |= self.base_size.map_or(0, |_| P_BASE_SIZE);
        flags |= self.win_gravity.map_or(0, |_| P_WIN_GRAVITY);
        let (x, y) = self.position.map_or((0, 0), |(_, x, y)| (x, y));
        let (w, h) = self.size.map_or((0, 0), |(_, w, h)| (w, h));
        let zero = (0, 0);
        let ((an, ad), (bn, bd)) = self.aspect.unwrap_or((zero, zero));
        let mut out = Vec::with_capacity(72);
        out.extend_from_slice(&flags.to_ne_bytes());
        let pairs = [
            (x, y),
            (w, h),
            self.min_size.unwrap_or(zero),
            self.max_size.unwrap_or(zero),
            self.size_increment.unwrap_or(zero),
            (an, ad),
            (bn, bd),
            self.base_size.unwrap_or(zero),
        ];
        for (a, b) in pairs {
            out.extend_from_slice(&a.to_ne_bytes());
            out.extend_from_slice(&b.to_ne_bytes());
        }
        out.extend_from_slice(&self.win_gravity.unwrap_or(0).to_ne_bytes());
        out
    }

    /// The ChangeProperty that sets them as `property` (`WM_NORMAL_HINTS`).
    pub fn set(&self, window: Window, property: Atom) -> Request<Void> {
        change_property(
            PropMode::Replace,
            window,
            property,
            atom::WM_SIZE_HINTS,
            32,
            &self.to_bytes(),
        )
    }
}

/// `WM_HINTS`' initial state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialState {
    Normal,
    Iconic,
}

/// `WM_HINTS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WmHints {
    pub input: Option<bool>,
    pub initial_state: Option<InitialState>,
    pub icon_pixmap: Option<Pixmap>,
    pub icon_window: Option<Window>,
    pub icon_position: Option<(i32, i32)>,
    pub icon_mask: Option<Pixmap>,
    pub window_group: Option<Window>,
    pub urgent: bool,
}

impl WmHints {
    /// Parse the property's value (`format` 32 items).
    pub fn parse(value: &[u8]) -> Result<WmHints, ParseError> {
        let mut r = Reader::new(value);
        let flags = r.u32()?;
        let input = flagged(flags, HINT_INPUT, r.u32()?);
        let initial_state = flagged(flags, HINT_STATE, r.u32()?);
        let icon_pixmap = flagged(flags, HINT_ICON_PIXMAP, r.u32()?);
        let icon_window = flagged(flags, HINT_ICON_WINDOW, r.u32()?);
        let icon_position = flagged(flags, HINT_ICON_POSITION, pair(&mut r)?);
        let icon_mask = flagged(flags, HINT_ICON_MASK, r.u32()?);
        // Pre-ICCCM-1 properties stop before the window group.
        let window_group = if value.len() <= 32 {
            None
        } else {
            flagged(flags, HINT_WINDOW_GROUP, r.u32()?)
        };
        let initial_state = match initial_state {
            None => None,
            Some(1) => Some(InitialState::Normal),
            Some(3) => Some(InitialState::Iconic),
            Some(_) => return Err(ParseError::Invalid),
        };
        Ok(WmHints {
            input: input.map(|v| v != 0),
            initial_state,
            icon_pixmap,
            icon_window,
            icon_position,
            icon_mask,
            window_group,
            urgent: flags & HINT_URGENCY != 0,
        })
    }

    /// Read a `WM_HINTS` property: `None` when it is absent.
    pub fn from_reply(reply: &GetPropertyReply) -> Result<Option<WmHints>, ParseError> {
        if reply.type_ == atom::ANY {
            return Ok(None);
        }
        if reply.type_ != atom::WM_HINTS || reply.format != 32 {
            return Err(ParseError::Invalid);
        }
        WmHints::parse(&reply.value).map(Some)
    }

    /// The property's 36 bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut flags = 0;
        flags |= self.input.map_or(0, |_| HINT_INPUT);
        flags |= self.initial_state.map_or(0, |_| HINT_STATE);
        flags |= self.icon_pixmap.map_or(0, |_| HINT_ICON_PIXMAP);
        flags |= self.icon_window.map_or(0, |_| HINT_ICON_WINDOW);
        flags |= self.icon_position.map_or(0, |_| HINT_ICON_POSITION);
        flags |= self.icon_mask.map_or(0, |_| HINT_ICON_MASK);
        flags |= self.window_group.map_or(0, |_| HINT_WINDOW_GROUP);
        if self.urgent {
            flags |= HINT_URGENCY;
        }
        let state = match self.initial_state {
            Some(InitialState::Normal) => 1u32,
            Some(InitialState::Iconic) => 3,
            None => 0,
        };
        let (ix, iy) = self.icon_position.unwrap_or((0, 0));
        let words: [u32; 9] = [
            flags,
            u32::from(self.input.unwrap_or(false)),
            state,
            self.icon_pixmap.unwrap_or(0),
            self.icon_window.unwrap_or(0),
            u32::from_ne_bytes(ix.to_ne_bytes()),
            u32::from_ne_bytes(iy.to_ne_bytes()),
            self.icon_mask.unwrap_or(0),
            self.window_group.unwrap_or(0),
        ];
        words.iter().flat_map(|w| w.to_ne_bytes()).collect()
    }

    /// The ChangeProperty that sets them.
    pub fn set(&self, window: Window) -> Request<Void> {
        change_property(
            PropMode::Replace,
            window,
            atom::WM_HINTS,
            atom::WM_HINTS,
            32,
            &self.to_bytes(),
        )
    }
}
