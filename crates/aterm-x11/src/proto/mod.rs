// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The X11 wire codec: requests encoded to bytes, replies, events and errors
//! parsed from them. No connection, no I/O, no platform: every function here
//! is pure, so the codec is held to x11rb-protocol 0.13.2's own encodings on
//! any host (`tests/recorded/`).
//!
//! Byte order is the client's native one, as libxcb and x11rb choose at
//! connection setup; a request's bytes are what goes on that wire. A request
//! too long for the 16-bit length field gets a length of 0 here, as
//! x11rb-protocol writes it, and the transport rewrites it in the
//! BIG-REQUESTS form ([`big_request`]).
//!
//! Every parser takes bytes another process wrote (the X server, or a client
//! through `SendEvent` and properties). It returns [`ParseError`] on a short or
//! inconsistent packet. Reads use checked slice bounds; fixed-width
//! protocol counts may reserve bounded initial vector capacity before a
//! later field read refuses a short packet.

use std::fmt;
use std::marker::PhantomData;

pub mod event;
pub mod randr;
pub mod shm;
pub mod sync;
pub mod xinput;
pub mod xproto;

/// A window id.
pub type Window = u32;
/// An atom.
pub type Atom = u32;
/// A server timestamp in milliseconds, or [`CURRENT_TIME`].
pub type Timestamp = u32;
/// A graphics context id.
pub type Gcontext = u32;
/// A window or pixmap id.
pub type Drawable = u32;
/// A visual id.
pub type Visualid = u32;
/// A colormap id.
pub type Colormap = u32;
/// A pixmap id.
pub type Pixmap = u32;
/// A cursor id.
pub type Cursor = u32;
/// A keycode.
pub type Keycode = u8;

/// The null resource or atom (`None` in the protocol).
pub const NONE: u32 = 0;
/// The timestamp that means "now" to the server.
pub const CURRENT_TIME: Timestamp = 0;
/// `CopyFromParent` for a depth, class or visual.
pub const COPY_FROM_PARENT: u32 = 0;

/// The atoms every server predefines (protocol appendix B).
pub mod atom {
    use super::Atom;

    /// `AnyPropertyType` in GetProperty.
    pub const ANY: Atom = 0;
    pub const PRIMARY: Atom = 1;
    pub const SECONDARY: Atom = 2;
    pub const ATOM: Atom = 4;
    pub const CARDINAL: Atom = 6;
    pub const INTEGER: Atom = 19;
    pub const PIXMAP: Atom = 20;
    pub const RESOURCE_MANAGER: Atom = 23;
    pub const STRING: Atom = 31;
    pub const WINDOW: Atom = 33;
    pub const WM_HINTS: Atom = 35;
    pub const WM_ICON_NAME: Atom = 37;
    pub const WM_NAME: Atom = 39;
    pub const WM_NORMAL_HINTS: Atom = 40;
    pub const WM_SIZE_HINTS: Atom = 41;
    pub const WM_CLASS: Atom = 67;
    pub const WM_TRANSIENT_FOR: Atom = 68;
}

/// Why a packet did not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The packet ends before a field it must hold.
    Short,
    /// A field holds a value the protocol does not allow there (the wrong
    /// packet type, a length that disagrees with another field).
    Invalid,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ParseError::Short => "X11 packet too short",
            ParseError::Invalid => "X11 packet holds an invalid value",
        })
    }
}

impl std::error::Error for ParseError {}

/// The marker for a request the server does not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Void {}

/// A reply the codec parses.
pub trait Reply: Sized {
    /// Parse one whole reply packet (32 bytes plus `4 * length`).
    fn parse(bytes: &[u8]) -> Result<Self, ParseError>;
}

/// One encoded request. `R` is its reply type, or [`Void`].
pub struct Request<R> {
    bytes: Vec<u8>,
    reply: PhantomData<fn() -> R>,
}

impl<R> Request<R> {
    /// The request as it goes on the wire, the length field included.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Take the bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl<R> fmt::Debug for Request<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("opcode", &self.bytes.first())
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// Rewrite `bytes` (a whole request, length field 0 when it is longer than
/// 16 bits can say) in the BIG-REQUESTS form: a zero length field, then the
/// 32-bit length in units of 4 bytes, which counts the extra word. A request
/// that fits is returned unchanged. `None` when the request is not a whole
/// number of words or too long even for 32 bits.
pub fn big_request(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(4) {
        return None;
    }
    let words = bytes.len() / 4;
    if words <= usize::from(u16::MAX) {
        return Some(bytes.to_vec());
    }
    let big = u32::try_from(words.checked_add(1)?).ok()?;
    let mut out = Vec::with_capacity(bytes.len().checked_add(4)?);
    out.extend_from_slice(&bytes[..2]);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&big.to_ne_bytes());
    out.extend_from_slice(&bytes[4..]);
    Some(out)
}

/// Bytes of padding that bring `len` to a multiple of four.
pub(crate) fn pad4(len: usize) -> usize {
    (4 - (len % 4)) % 4
}

/// A request under construction: opcode, data byte, length placeholder.
pub(crate) struct Writer(Vec<u8>);

impl Writer {
    /// A core request (`data` is the request's one-byte field) or an
    /// extension request (`opcode` the major opcode, `data` the minor).
    pub(crate) fn new(opcode: u8, data: u8) -> Writer {
        let mut b = Vec::with_capacity(32);
        b.extend_from_slice(&[opcode, data, 0, 0]);
        Writer(b)
    }

    pub(crate) fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }

    pub(crate) fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_ne_bytes());
        self
    }

    pub(crate) fn i16(&mut self, v: i16) -> &mut Self {
        self.0.extend_from_slice(&v.to_ne_bytes());
        self
    }

    pub(crate) fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_ne_bytes());
        self
    }

    pub(crate) fn pad(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }

    pub(crate) fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }

    /// Pad to a word, write the length in words (0 when it does not fit 16
    /// bits: see [`big_request`]).
    pub(crate) fn finish<R>(&mut self) -> Request<R> {
        let mut bytes = std::mem::take(&mut self.0);
        bytes.resize(bytes.len() + pad4(bytes.len()), 0);
        let len = u16::try_from(bytes.len() / 4).unwrap_or(0);
        bytes[2..4].copy_from_slice(&len.to_ne_bytes());
        Request {
            bytes,
            reply: PhantomData,
        }
    }
}

/// A cursor over a packet another process wrote.
pub(crate) struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, pos: 0 }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let end = self.pos.checked_add(n).ok_or(ParseError::Short)?;
        let s = self.b.get(self.pos..end).ok_or(ParseError::Short)?;
        self.pos = end;
        Ok(s)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        self.take(n).map(drop)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn bool(&mut self) -> Result<bool, ParseError> {
        Ok(self.u8()? != 0)
    }

    pub(crate) fn u16(&mut self) -> Result<u16, ParseError> {
        let s = self.take(2)?;
        Ok(u16::from_ne_bytes([s[0], s[1]]))
    }

    pub(crate) fn i16(&mut self) -> Result<i16, ParseError> {
        let s = self.take(2)?;
        Ok(i16::from_ne_bytes([s[0], s[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, ParseError> {
        let s = self.take(4)?;
        Ok(u32::from_ne_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub(crate) fn i32(&mut self) -> Result<i32, ParseError> {
        let s = self.take(4)?;
        Ok(i32::from_ne_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// `n` 32-bit values.
    pub(crate) fn u32s(&mut self, n: usize) -> Result<Vec<u32>, ParseError> {
        let bytes = self.take(n.checked_mul(4).ok_or(ParseError::Short)?)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_ne_bytes(*c))
            .collect())
    }
}

/// The common head of a reply: checks the packet type (1) and that the packet
/// holds the `4 * length` bytes past 32 its length field promises. Returns
/// the reader positioned after the length field, and the data byte.
pub(crate) fn reply_head(bytes: &[u8]) -> Result<(Reader<'_>, u8, u32), ParseError> {
    let mut r = Reader::new(bytes);
    let kind = r.u8()?;
    let data = r.u8()?;
    let _sequence = r.u16()?;
    let length = r.u32()?;
    if kind != 1 {
        return Err(ParseError::Invalid);
    }
    let total = usize::try_from(length)
        .ok()
        .and_then(|l| l.checked_mul(4))
        .and_then(|l| l.checked_add(32))
        .ok_or(ParseError::Short)?;
    if bytes.len() < total {
        return Err(ParseError::Short);
    }
    Ok((r, data, length))
}

/// A request's value list: the mask and the values in ascending bit order,
/// each four bytes on the wire whatever its type (CreateWindow,
/// ChangeWindowAttributes, ConfigureWindow, CreateGC).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValueList {
    values: Vec<(u32, u32)>,
}

impl ValueList {
    /// No values.
    pub fn new() -> ValueList {
        ValueList::default()
    }

    /// Set the value of the field whose mask bit is `bit` (one bit set).
    /// A second set of one bit replaces the first.
    pub fn set(mut self, bit: u32, value: u32) -> ValueList {
        match self.values.binary_search_by_key(&bit, |&(b, _)| b) {
            Ok(i) => self.values[i].1 = value,
            Err(i) => self.values.insert(i, (bit, value)),
        }
        self
    }

    /// The mask: every bit set.
    pub fn mask(&self) -> u32 {
        self.values.iter().fold(0, |m, &(b, _)| m | b)
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        for &(_, v) in &self.values {
            w.u32(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_request_takes_the_big_form() {
        let words = usize::from(u16::MAX) + 1;
        let mut req = vec![0u8; words * 4];
        req[0] = 72;
        req[1] = 2;
        let big = big_request(&req).expect("a whole number of words");
        assert_eq!(big.len(), req.len() + 4);
        assert_eq!(&big[..4], &[72, 2, 0, 0]);
        assert_eq!(
            u32::from_ne_bytes([big[4], big[5], big[6], big[7]]),
            u32::try_from(words + 1).unwrap()
        );
        let small = vec![1u8; 8];
        assert_eq!(big_request(&small).as_deref(), Some(&small[..]));
        assert_eq!(big_request(&[1, 2, 3]), None);
    }

    #[test]
    fn a_reply_shorter_than_its_length_field_is_refused() {
        let mut reply = [0u8; 36];
        reply[0] = 1;
        reply[4..8].copy_from_slice(&2u32.to_ne_bytes());
        assert_eq!(reply_head(&reply).err(), Some(ParseError::Short));
        reply[4..8].copy_from_slice(&1u32.to_ne_bytes());
        assert!(reply_head(&reply).is_ok());
        reply[0] = 0;
        assert_eq!(reply_head(&reply).err(), Some(ParseError::Invalid));
        assert_eq!(reply_head(&reply[..7]).err(), Some(ParseError::Short));
    }

    #[test]
    fn a_value_list_orders_its_bits() {
        let list = ValueList::new().set(8, 3).set(1, 9).set(8, 4);
        assert_eq!(list.mask(), 9);
        let mut w = Writer::new(0, 0);
        list.write(&mut w);
        let req: Request<Void> = w.finish();
        let want: Vec<u8> = [9u32, 4].iter().flat_map(|v| v.to_ne_bytes()).collect();
        assert_eq!(&req.bytes()[4..], &want[..]);
    }
}
