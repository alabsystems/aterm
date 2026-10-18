// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! RandR 1.5's requests the window system's monitor handling sends: the
//! version, notification, the screen's resources (CRTCs, outputs, modes), an
//! output's and a CRTC's state, the primary output, and a CRTC's
//! configuration (fullscreen video modes). Written against randrproto's
//! protocol description (xcbproto `randr.xml`); bytes held to
//! x11rb-protocol 0.13.2's (`tests/recorded/`).

use super::{ParseError, Reader, Reply, Request, Timestamp, Void, Window, Writer, reply_head};

/// The extension's name for QueryExtension.
pub const NAME: &[u8] = b"RANDR";

/// A CRTC id.
pub type Crtc = u32;
/// An output id.
pub type Output = u32;
/// A mode id.
pub type Mode = u32;

/// SelectInput's notification bits.
pub mod notify_mask {
    pub const SCREEN_CHANGE: u16 = 1 << 0;
    pub const CRTC_CHANGE: u16 = 1 << 1;
    pub const OUTPUT_CHANGE: u16 = 1 << 2;
    pub const OUTPUT_PROPERTY: u16 = 1 << 3;
}

/// Rotation bits.
pub mod rotation {
    pub const ROTATE_0: u16 = 1 << 0;
}

/// QueryVersion (0).
pub fn query_version(
    major: u8,
    major_version: u32,
    minor_version: u32,
) -> Request<QueryVersionReply> {
    Writer::new(major, 0)
        .u32(major_version)
        .u32(minor_version)
        .finish()
}

/// SelectInput (4).
pub fn select_input(major: u8, window: Window, enable: u16) -> Request<Void> {
    Writer::new(major, 4)
        .u32(window)
        .u16(enable)
        .pad(2)
        .finish()
}

/// GetScreenResources (8): probes the outputs, which may be slow.
pub fn get_screen_resources(major: u8, window: Window) -> Request<ScreenResourcesReply> {
    Writer::new(major, 8).u32(window).finish()
}

/// GetOutputInfo (9).
pub fn get_output_info(
    major: u8,
    output: Output,
    config_timestamp: Timestamp,
) -> Request<GetOutputInfoReply> {
    Writer::new(major, 9)
        .u32(output)
        .u32(config_timestamp)
        .finish()
}

/// GetCrtcInfo (20).
pub fn get_crtc_info(
    major: u8,
    crtc: Crtc,
    config_timestamp: Timestamp,
) -> Request<GetCrtcInfoReply> {
    Writer::new(major, 20)
        .u32(crtc)
        .u32(config_timestamp)
        .finish()
}

/// SetCrtcConfig (21).
#[allow(
    clippy::too_many_arguments,
    reason = "the request's own fields, in wire order"
)]
pub fn set_crtc_config(
    major: u8,
    crtc: Crtc,
    timestamp: Timestamp,
    config_timestamp: Timestamp,
    x: i16,
    y: i16,
    mode: Mode,
    rotation: u16,
    outputs: &[Output],
) -> Request<SetCrtcConfigReply> {
    let mut w = Writer::new(major, 21);
    w.u32(crtc)
        .u32(timestamp)
        .u32(config_timestamp)
        .i16(x)
        .i16(y)
        .u32(mode)
        .u16(rotation)
        .pad(2);
    for &o in outputs {
        w.u32(o);
    }
    w.finish()
}

/// GetScreenResourcesCurrent (25): the resources as last probed, at once.
pub fn get_screen_resources_current(major: u8, window: Window) -> Request<ScreenResourcesReply> {
    Writer::new(major, 25).u32(window).finish()
}

/// GetOutputPrimary (31).
pub fn get_output_primary(major: u8, window: Window) -> Request<GetOutputPrimaryReply> {
    Writer::new(major, 31).u32(window).finish()
}

/// QueryVersion's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryVersionReply {
    pub major_version: u32,
    pub minor_version: u32,
}

impl Reply for QueryVersionReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(QueryVersionReply {
            major_version: r.u32()?,
            minor_version: r.u32()?,
        })
    }
}

/// One mode of the screen's resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeInfo {
    pub id: Mode,
    pub width: u16,
    pub height: u16,
    pub dot_clock: u32,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub name_len: u16,
    pub mode_flags: u32,
}

/// GetScreenResources' and GetScreenResourcesCurrent's reply (one layout).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenResourcesReply {
    pub timestamp: Timestamp,
    pub config_timestamp: Timestamp,
    pub crtcs: Vec<Crtc>,
    pub outputs: Vec<Output>,
    pub modes: Vec<ModeInfo>,
    /// The modes' names, back to back; each mode's `name_len` bytes in order.
    pub names: Vec<u8>,
}

fn mode_info(r: &mut Reader<'_>) -> Result<ModeInfo, ParseError> {
    Ok(ModeInfo {
        id: r.u32()?,
        width: r.u16()?,
        height: r.u16()?,
        dot_clock: r.u32()?,
        hsync_start: r.u16()?,
        hsync_end: r.u16()?,
        htotal: r.u16()?,
        hskew: r.u16()?,
        vsync_start: r.u16()?,
        vsync_end: r.u16()?,
        vtotal: r.u16()?,
        name_len: r.u16()?,
        mode_flags: r.u32()?,
    })
}

impl Reply for ScreenResourcesReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        let timestamp = r.u32()?;
        let config_timestamp = r.u32()?;
        let num_crtcs = r.u16()?;
        let num_outputs = r.u16()?;
        let num_modes = r.u16()?;
        let names_len = r.u16()?;
        r.skip(8)?;
        let crtcs = r.u32s(usize::from(num_crtcs))?;
        let outputs = r.u32s(usize::from(num_outputs))?;
        let mut modes = Vec::with_capacity(usize::from(num_modes).min(256));
        for _ in 0..num_modes {
            modes.push(mode_info(&mut r)?);
        }
        let names = r.take(usize::from(names_len))?.to_vec();
        Ok(ScreenResourcesReply {
            timestamp,
            config_timestamp,
            crtcs,
            outputs,
            modes,
            names,
        })
    }
}

/// GetOutputInfo's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetOutputInfoReply {
    pub status: u8,
    pub timestamp: Timestamp,
    pub crtc: Crtc,
    pub mm_width: u32,
    pub mm_height: u32,
    pub connection: u8,
    pub subpixel_order: u8,
    pub num_preferred: u16,
    pub crtcs: Vec<Crtc>,
    pub modes: Vec<Mode>,
    pub clones: Vec<Output>,
    pub name: Vec<u8>,
}

impl Reply for GetOutputInfoReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, status, _) = reply_head(bytes)?;
        let timestamp = r.u32()?;
        let crtc = r.u32()?;
        let mm_width = r.u32()?;
        let mm_height = r.u32()?;
        let connection = r.u8()?;
        let subpixel_order = r.u8()?;
        let num_crtcs = r.u16()?;
        let num_modes = r.u16()?;
        let num_preferred = r.u16()?;
        let num_clones = r.u16()?;
        let name_len = r.u16()?;
        Ok(GetOutputInfoReply {
            status,
            timestamp,
            crtc,
            mm_width,
            mm_height,
            connection,
            subpixel_order,
            num_preferred,
            crtcs: r.u32s(usize::from(num_crtcs))?,
            modes: r.u32s(usize::from(num_modes))?,
            clones: r.u32s(usize::from(num_clones))?,
            name: r.take(usize::from(name_len))?.to_vec(),
        })
    }
}

/// GetCrtcInfo's reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetCrtcInfoReply {
    pub status: u8,
    pub timestamp: Timestamp,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub mode: Mode,
    pub rotation: u16,
    pub rotations: u16,
    pub outputs: Vec<Output>,
    pub possible: Vec<Output>,
}

impl Reply for GetCrtcInfoReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, status, _) = reply_head(bytes)?;
        let timestamp = r.u32()?;
        let x = r.i16()?;
        let y = r.i16()?;
        let width = r.u16()?;
        let height = r.u16()?;
        let mode = r.u32()?;
        let rotation = r.u16()?;
        let rotations = r.u16()?;
        let num_outputs = r.u16()?;
        let num_possible = r.u16()?;
        Ok(GetCrtcInfoReply {
            status,
            timestamp,
            x,
            y,
            width,
            height,
            mode,
            rotation,
            rotations,
            outputs: r.u32s(usize::from(num_outputs))?,
            possible: r.u32s(usize::from(num_possible))?,
        })
    }
}

/// SetCrtcConfig's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetCrtcConfigReply {
    pub status: u8,
    pub timestamp: Timestamp,
}

impl Reply for SetCrtcConfigReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, status, _) = reply_head(bytes)?;
        Ok(SetCrtcConfigReply {
            status,
            timestamp: r.u32()?,
        })
    }
}

/// GetOutputPrimary's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GetOutputPrimaryReply {
    pub output: Output,
}

impl Reply for GetOutputPrimaryReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(GetOutputPrimaryReply { output: r.u32()? })
    }
}
