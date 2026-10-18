// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! XInput 2's requests the window system's input handling sends: the
//! version, event selection per device, and a pointer query. Written against
//! inputproto's XI2 protocol description (xcbproto `xinput.xml`); bytes held
//! to x11rb-protocol 0.13.2's (`tests/recorded/`). XI2's events arrive
//! through Xlib (`XGetEventData`), not through this codec.

use super::{ParseError, Reply, Request, Void, Window, Writer, reply_head};

/// The extension's name for QueryExtension.
pub const NAME: &[u8] = b"XInputExtension";

/// A device id; [`ALL_DEVICES`] and [`ALL_MASTER_DEVICES`] are special.
pub type DeviceId = u16;
/// `XIAllDevices`.
pub const ALL_DEVICES: DeviceId = 0;
/// `XIAllMasterDevices`.
pub const ALL_MASTER_DEVICES: DeviceId = 1;

/// A 16.16 fixed-point coordinate.
pub type Fp1616 = i32;

/// The value of a [`Fp1616`].
pub fn fp1616_to_f64(v: Fp1616) -> f64 {
    f64::from(v) / 65536.0
}

/// XI2 event-mask bits (one bit per event type).
pub mod event_mask {
    pub const DEVICE_CHANGED: u32 = 1 << 1;
    pub const KEY_PRESS: u32 = 1 << 2;
    pub const KEY_RELEASE: u32 = 1 << 3;
    pub const BUTTON_PRESS: u32 = 1 << 4;
    pub const BUTTON_RELEASE: u32 = 1 << 5;
    pub const MOTION: u32 = 1 << 6;
    pub const ENTER: u32 = 1 << 7;
    pub const LEAVE: u32 = 1 << 8;
    pub const FOCUS_IN: u32 = 1 << 9;
    pub const FOCUS_OUT: u32 = 1 << 10;
    pub const HIERARCHY: u32 = 1 << 11;
    pub const PROPERTY: u32 = 1 << 12;
    pub const RAW_KEY_PRESS: u32 = 1 << 13;
    pub const RAW_KEY_RELEASE: u32 = 1 << 14;
    pub const RAW_BUTTON_PRESS: u32 = 1 << 15;
    pub const RAW_BUTTON_RELEASE: u32 = 1 << 16;
    pub const RAW_MOTION: u32 = 1 << 17;
    pub const TOUCH_BEGIN: u32 = 1 << 18;
    pub const TOUCH_UPDATE: u32 = 1 << 19;
    pub const TOUCH_END: u32 = 1 << 20;
}

/// One device's event mask for XISelectEvents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventMask {
    pub deviceid: DeviceId,
    /// The mask, 32 bits per word.
    pub mask: Vec<u32>,
}

/// XIQueryPointer (40).
pub fn xi_query_pointer(
    major: u8,
    window: Window,
    deviceid: DeviceId,
) -> Request<XIQueryPointerReply> {
    Writer::new(major, 40)
        .u32(window)
        .u16(deviceid)
        .pad(2)
        .finish()
}

/// XISelectEvents (46).
pub fn xi_select_events(major: u8, window: Window, masks: &[EventMask]) -> Request<Void> {
    let mut w = Writer::new(major, 46);
    w.u32(window)
        .u16(u16::try_from(masks.len()).unwrap_or(u16::MAX))
        .pad(2);
    for m in masks {
        w.u16(m.deviceid)
            .u16(u16::try_from(m.mask.len()).unwrap_or(u16::MAX));
        for &word in &m.mask {
            w.u32(word);
        }
    }
    w.finish()
}

/// XIQueryVersion (47).
pub fn xi_query_version(
    major: u8,
    major_version: u16,
    minor_version: u16,
) -> Request<XIQueryVersionReply> {
    Writer::new(major, 47)
        .u16(major_version)
        .u16(minor_version)
        .finish()
}

/// XIQueryVersion's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XIQueryVersionReply {
    pub major_version: u16,
    pub minor_version: u16,
}

impl Reply for XIQueryVersionReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(XIQueryVersionReply {
            major_version: r.u16()?,
            minor_version: r.u16()?,
        })
    }
}

/// Modifier state: base, latched, locked, effective.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierInfo {
    pub base: u32,
    pub latched: u32,
    pub locked: u32,
    pub effective: u32,
}

/// Group state: base, latched, locked, effective.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub base: u8,
    pub latched: u8,
    pub locked: u8,
    pub effective: u8,
}

/// XIQueryPointer's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XIQueryPointerReply {
    pub root: Window,
    pub child: Window,
    pub root_x: Fp1616,
    pub root_y: Fp1616,
    pub win_x: Fp1616,
    pub win_y: Fp1616,
    pub same_screen: bool,
    pub mods: ModifierInfo,
    pub group: GroupInfo,
    /// The pressed buttons, one bit each, 32 per word.
    pub buttons: Vec<u32>,
}

impl Reply for XIQueryPointerReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        let root = r.u32()?;
        let child = r.u32()?;
        let root_x = r.i32()?;
        let root_y = r.i32()?;
        let win_x = r.i32()?;
        let win_y = r.i32()?;
        let same_screen = r.bool()?;
        r.skip(1)?;
        let buttons_len = r.u16()?;
        let mods = ModifierInfo {
            base: r.u32()?,
            latched: r.u32()?,
            locked: r.u32()?,
            effective: r.u32()?,
        };
        let group = GroupInfo {
            base: r.u8()?,
            latched: r.u8()?,
            locked: r.u8()?,
            effective: r.u8()?,
        };
        Ok(XIQueryPointerReply {
            root,
            child,
            root_x,
            root_y,
            win_x,
            win_y,
            same_screen,
            mods,
            group,
            buttons: r.u32s(usize::from(buttons_len))?,
        })
    }
}
