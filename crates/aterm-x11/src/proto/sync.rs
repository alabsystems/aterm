// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SYNC 3.1's counter requests: the window system's `_NET_WM_SYNC_REQUEST`
//! counter (EWMH), which a window manager waits on before it shows a resized
//! window again. Written against the X Synchronization Extension Protocol
//! 3.1 (xcbproto `sync.xml`); bytes held to x11rb-protocol 0.13.2's
//! (`tests/recorded/`).
//!
//! An INT64 travels as its high half (INT32) and then its low half (CARD32),
//! each in the connection's byte order; here it is an `i64`.
//!
//! Every request takes the extension's major opcode, which the server assigns
//! (QueryExtension `SYNC`). The protocol asks a client to send Initialize
//! before any other of its requests.

use super::{ParseError, Reader, Reply, Request, Void, Writer, reply_head};

/// The extension's name for QueryExtension.
pub const NAME: &[u8] = b"SYNC";

/// A counter id (an XID this client generates).
pub type Counter = u32;

/// Write `value` as an INT64: high half, then low half.
fn int64(w: &mut Writer, value: i64) -> &mut Writer {
    let bits = value.cast_unsigned();
    w.u32((bits >> 32) as u32).u32(bits as u32)
}

/// Read an INT64: high half, then low half.
fn read_int64(r: &mut Reader<'_>) -> Result<i64, ParseError> {
    let hi = r.i32()?;
    let lo = r.u32()?;
    Ok((i64::from(hi) << 32) | i64::from(lo))
}

/// Initialize (0): the version this client speaks; the reply is the server's.
pub fn initialize(
    major: u8,
    desired_major_version: u8,
    desired_minor_version: u8,
) -> Request<InitializeReply> {
    Writer::new(major, 0)
        .u8(desired_major_version)
        .u8(desired_minor_version)
        .pad(2)
        .finish()
}

/// CreateCounter (2): counter `id` holding `initial_value`.
pub fn create_counter(major: u8, id: Counter, initial_value: i64) -> Request<Void> {
    let mut w = Writer::new(major, 2);
    w.u32(id);
    int64(&mut w, initial_value).finish()
}

/// SetCounter (3): `counter` now holds `value`.
pub fn set_counter(major: u8, counter: Counter, value: i64) -> Request<Void> {
    let mut w = Writer::new(major, 3);
    w.u32(counter);
    int64(&mut w, value).finish()
}

/// QueryCounter (5).
pub fn query_counter(major: u8, counter: Counter) -> Request<QueryCounterReply> {
    Writer::new(major, 5).u32(counter).finish()
}

/// DestroyCounter (6).
pub fn destroy_counter(major: u8, counter: Counter) -> Request<Void> {
    Writer::new(major, 6).u32(counter).finish()
}

/// Initialize's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitializeReply {
    pub major_version: u8,
    pub minor_version: u8,
}

impl Reply for InitializeReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(InitializeReply {
            major_version: r.u8()?,
            minor_version: r.u8()?,
        })
    }
}

/// QueryCounter's reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryCounterReply {
    pub counter_value: i64,
}

impl Reply for QueryCounterReply {
    fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let (mut r, _, _) = reply_head(bytes)?;
        Ok(QueryCounterReply {
            counter_value: read_int64(&mut r)?,
        })
    }
}
