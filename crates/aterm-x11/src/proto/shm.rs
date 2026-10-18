// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MIT-SHM 1.2: a shared-memory segment attached by descriptor (AttachFd) and
//! an image drawn from it with no pixel bytes on the wire (PutImage), which is
//! what the X11 CPU presenter draws through. Written against the extension's
//! protocol description (xcbproto `shm.xml`); bytes held to x11rb-protocol
//! 0.13.2's (`tests/recorded/`).
//!
//! Every request takes the extension's major opcode, which the server assigns
//! (QueryExtension `MIT-SHM`).

use super::{Drawable, Gcontext, ParseError, Reply, Request, Void, Writer, reply_head};

/// The extension's name for QueryExtension.
pub const NAME: &[u8] = b"MIT-SHM";

/// A segment id (an XID this client generates).
pub type Seg = u32;

/// QueryVersion (0).
pub fn query_version(major: u8) -> Request<QueryVersionReply> {
    Writer::new(major, 0).finish()
}

/// Detach (2).
pub fn detach(major: u8, shmseg: Seg) -> Request<Void> {
    Writer::new(major, 2).u32(shmseg).finish()
}

/// PutImage (3): draw `src_width` x `src_height` pixels at (`src_x`,
/// `src_y`) of the `total_width` x `total_height` image at `offset` in the
/// segment, to (`dst_x`, `dst_y`) of `drawable`. With `send_event` the server
/// answers with a Completion event once it has read the segment.
#[allow(
    clippy::too_many_arguments,
    reason = "the request's own fields, in wire order"
)]
pub fn put_image(
    major: u8,
    drawable: Drawable,
    gc: Gcontext,
    total_width: u16,
    total_height: u16,
    src_x: u16,
    src_y: u16,
    src_width: u16,
    src_height: u16,
    dst_x: i16,
    dst_y: i16,
    depth: u8,
    format: u8,
    send_event: bool,
    shmseg: Seg,
    offset: u32,
) -> Request<Void> {
    Writer::new(major, 3)
        .u32(drawable)
        .u32(gc)
        .u16(total_width)
        .u16(total_height)
        .u16(src_x)
        .u16(src_y)
        .u16(src_width)
        .u16(src_height)
        .i16(dst_x)
        .i16(dst_y)
        .u8(depth)
        .u8(format)
        .u8(u8::from(send_event))
        .pad(1)
        .u32(shmseg)
        .u32(offset)
        .finish()
}

/// AttachFd (6): the descriptor itself travels beside these bytes
/// (SCM_RIGHTS), which the transport sends with them.
pub fn attach_fd(major: u8, shmseg: Seg, read_only: bool) -> Request<Void> {
    Writer::new(major, 6)
        .u32(shmseg)
        .u8(u8::from(read_only))
        .pad(3)
        .finish()
}

/// QueryVersion's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryVersionReply {
    pub shared_pixmaps: bool,
    pub major_version: u16,
    pub minor_version: u16,
    pub uid: u16,
    pub gid: u16,
    pub pixmap_format: u8,
}

impl Reply for QueryVersionReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, shared_pixmaps, _) = reply_head(bytes)?;
        Ok(QueryVersionReply {
            shared_pixmaps: shared_pixmaps != 0,
            major_version: r.u16()?,
            minor_version: r.u16()?,
            uid: r.u16()?,
            gid: r.u16()?,
            pixmap_format: r.u8()?,
        })
    }
}
