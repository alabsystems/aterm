// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! X11 events and errors: 32-byte packets sent by the server.

use super::{Atom, ParseError, Reader, Timestamp, Window};

/// The error packet's code.
pub const ERROR: u8 = 0;
/// The reply packet's code.
pub const REPLY: u8 = 1;
/// PropertyNotify.
pub const PROPERTY_NOTIFY: u8 = 28;
/// SelectionClear.
pub const SELECTION_CLEAR: u8 = 29;
/// SelectionRequest.
pub const SELECTION_REQUEST: u8 = 30;
/// SelectionNotify.
pub const SELECTION_NOTIFY: u8 = 31;
/// ClientMessage.
pub const CLIENT_MESSAGE: u8 = 33;
/// GenericEvent (XGE): longer than 32 bytes.
pub const GENERIC_EVENT: u8 = 35;

/// PropertyNotify's states.
pub mod property {
    pub const NEW_VALUE: u8 = 0;
    pub const DELETE: u8 = 1;
}

/// An error the server sent for a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XError {
    pub error_code: u8,
    pub sequence: u16,
    pub bad_value: u32,
    pub minor_opcode: u16,
    pub major_opcode: u8,
}

impl XError {
    /// Parse an error packet (code 0).
    pub fn parse(bytes: &[u8]) -> Result<XError, ParseError> {
        let mut r = Reader::new(bytes);
        if r.u8()? != ERROR {
            return Err(ParseError::Invalid);
        }
        let error_code = r.u8()?;
        let sequence = r.u16()?;
        let bad_value = r.u32()?;
        let minor_opcode = r.u16()?;
        let major_opcode = r.u8()?;
        r.skip(21)?;
        Ok(XError {
            error_code,
            sequence,
            bad_value,
            minor_opcode,
            major_opcode,
        })
    }
}

/// SelectionRequest: a client asks the owner to convert a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRequest {
    pub sequence: u16,
    pub time: Timestamp,
    pub owner: Window,
    pub requestor: Window,
    pub selection: Atom,
    pub target: Atom,
    pub property: Atom,
}

/// SelectionNotify: the owner's answer (property `NONE` on refusal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionNotify {
    pub sequence: u16,
    pub time: Timestamp,
    pub requestor: Window,
    pub selection: Atom,
    pub target: Atom,
    pub property: Atom,
}

impl SelectionNotify {
    /// The 32-byte packet, for SendEvent.
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0] = SELECTION_NOTIFY;
        b[2..4].copy_from_slice(&self.sequence.to_ne_bytes());
        b[4..8].copy_from_slice(&self.time.to_ne_bytes());
        b[8..12].copy_from_slice(&self.requestor.to_ne_bytes());
        b[12..16].copy_from_slice(&self.selection.to_ne_bytes());
        b[16..20].copy_from_slice(&self.target.to_ne_bytes());
        b[20..24].copy_from_slice(&self.property.to_ne_bytes());
        b
    }
}

/// SelectionClear: another client took a selection this one owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionClear {
    pub sequence: u16,
    pub time: Timestamp,
    pub owner: Window,
    pub selection: Atom,
}

/// PropertyNotify: a property changed or went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropertyNotify {
    pub sequence: u16,
    pub window: Window,
    pub atom: Atom,
    pub time: Timestamp,
    pub state: u8,
}

/// ClientMessage: 20 bytes of data in `format`-bit items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientMessage {
    pub format: u8,
    pub sequence: u16,
    pub window: Window,
    pub type_: Atom,
    pub data: [u8; 20],
}

impl ClientMessage {
    /// A format-32 message of five values in this client's byte order.
    pub fn new32(window: Window, type_: Atom, values: [u32; 5]) -> ClientMessage {
        let mut data = [0u8; 20];
        for (chunk, v) in data.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            *chunk = v.to_ne_bytes();
        }
        ClientMessage {
            format: 32,
            sequence: 0,
            window,
            type_,
            data,
        }
    }

    /// The data as five 32-bit values.
    pub fn data32(&self) -> [u32; 5] {
        let mut out = [0u32; 5];
        for (v, c) in out.iter_mut().zip(self.data.as_chunks::<4>().0) {
            *v = u32::from_ne_bytes(*c);
        }
        out
    }

    /// The 32-byte packet, for SendEvent.
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0] = CLIENT_MESSAGE;
        b[1] = self.format;
        b[2..4].copy_from_slice(&self.sequence.to_ne_bytes());
        b[4..8].copy_from_slice(&self.window.to_ne_bytes());
        b[8..12].copy_from_slice(&self.type_.to_ne_bytes());
        b[12..32].copy_from_slice(&self.data);
        b
    }
}

/// One packet from the event queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An error for a request whose error was not checked.
    Error(XError),
    SelectionRequest(SelectionRequest),
    SelectionNotify(SelectionNotify),
    SelectionClear(SelectionClear),
    PropertyNotify(PropertyNotify),
    ClientMessage(ClientMessage),
    /// Any other event: its code (the SendEvent bit cleared) and packet.
    Other {
        code: u8,
        bytes: Vec<u8>,
    },
}

impl Event {
    /// Parse one event packet: 32 bytes, or more for a GenericEvent.
    pub fn parse(bytes: &[u8]) -> Result<Event, ParseError> {
        let mut r = Reader::new(bytes);
        let code = r.u8()? & 0x7f;
        if bytes.len() < 32 {
            return Err(ParseError::Short);
        }
        Ok(match code {
            ERROR => Event::Error(XError::parse(bytes)?),
            SELECTION_REQUEST => {
                r.skip(1)?;
                Event::SelectionRequest(SelectionRequest {
                    sequence: r.u16()?,
                    time: r.u32()?,
                    owner: r.u32()?,
                    requestor: r.u32()?,
                    selection: r.u32()?,
                    target: r.u32()?,
                    property: r.u32()?,
                })
            }
            SELECTION_NOTIFY => {
                r.skip(1)?;
                Event::SelectionNotify(SelectionNotify {
                    sequence: r.u16()?,
                    time: r.u32()?,
                    requestor: r.u32()?,
                    selection: r.u32()?,
                    target: r.u32()?,
                    property: r.u32()?,
                })
            }
            SELECTION_CLEAR => {
                r.skip(1)?;
                Event::SelectionClear(SelectionClear {
                    sequence: r.u16()?,
                    time: r.u32()?,
                    owner: r.u32()?,
                    selection: r.u32()?,
                })
            }
            PROPERTY_NOTIFY => {
                r.skip(1)?;
                Event::PropertyNotify(PropertyNotify {
                    sequence: r.u16()?,
                    window: r.u32()?,
                    atom: r.u32()?,
                    time: r.u32()?,
                    state: r.u8()?,
                })
            }
            CLIENT_MESSAGE => {
                let format = r.u8()?;
                let sequence = r.u16()?;
                let window = r.u32()?;
                let type_ = r.u32()?;
                let mut data = [0u8; 20];
                data.copy_from_slice(r.take(20)?);
                Event::ClientMessage(ClientMessage {
                    format,
                    sequence,
                    window,
                    type_,
                    data,
                })
            }
            code => Event::Other {
                code,
                bytes: bytes.to_vec(),
            },
        })
    }
}
