// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D-Bus messages: the 12-byte fixed header, the header fields (`a(yv)`), and
//! the body, framed by the lengths the header carries.
//!
//! [`Message::encode`] writes the fields in zbus 5's order (path, interface,
//! member, error name, reply serial, destination, sender, signature), so what
//! aterm-dbus sends is byte-identical to what zbus sends for the same message
//! (`tests/recorded/zbus-5.16.0-messages.tsv`). [`Message::decode`] accepts the
//! fields in any order, ignores unknown ones as the specification requires,
//! and refuses a repeated or mistyped one and code 0, which the specification
//! names INVALID (libdbus refuses it too).

use crate::error::{Error, Result};
use crate::names;
use crate::sig::{self, SigFault, Type};
use crate::value::{ObjectPath, Signature, Value};
use crate::wire::{self, Endian, MAX_ARRAY_BYTES, Reader, Writer, pad_to};

/// The longest message the specification allows.
pub const MAX_MESSAGE: usize = 1 << 27;

/// The flag bits of the fixed header.
pub mod flags {
    /// A method call whose caller wants no reply.
    pub const NO_REPLY_EXPECTED: u8 = 0x1;
    /// Do not start a service to receive this call.
    pub const NO_AUTO_START: u8 = 0x2;
    /// The caller is prepared to wait for interactive authorization.
    pub const ALLOW_INTERACTIVE_AUTHORIZATION: u8 = 0x4;
}

/// The four message types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    MethodCall = 1,
    MethodReturn = 2,
    Error = 3,
    Signal = 4,
}

const PATH: u8 = 1;
const INTERFACE: u8 = 2;
const MEMBER: u8 = 3;
const ERROR_NAME: u8 = 4;
const REPLY_SERIAL: u8 = 5;
const DESTINATION: u8 = 6;
const SENDER: u8 = 7;
const SIGNATURE: u8 = 8;
const UNIX_FDS: u8 = 9;

/// One message. The fields are public; [`Message::encode`] holds them to the
/// specification, so a hand-built message cannot reach the wire malformed.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub endian: Endian,
    pub kind: Kind,
    pub flags: u8,
    /// Assigned by [`crate::Connection::send`]; must be nonzero to encode.
    pub serial: u32,
    pub path: Option<ObjectPath>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub body: Vec<Value>,
}

/// A name field, the rule it must meet, and what to call it in a refusal.
type NameCheck<'a> = (&'a Option<String>, fn(&str) -> bool, &'static str);

fn name(valid: fn(&str) -> bool, text: &str, what: &'static str) -> Result<String> {
    if valid(text) {
        Ok(text.to_owned())
    } else {
        Err(Error::Invalid(what))
    }
}

impl Message {
    fn new(kind: Kind) -> Self {
        Self {
            endian: Endian::NATIVE,
            kind,
            flags: 0,
            serial: 0,
            path: None,
            interface: None,
            member: None,
            error_name: None,
            reply_serial: None,
            destination: None,
            sender: None,
            body: Vec::new(),
        }
    }

    /// A call of `member` on the object at `path`.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] for a name or path the specification forbids.
    pub fn method_call(
        destination: Option<&str>,
        path: &str,
        interface: Option<&str>,
        member: &str,
    ) -> Result<Self> {
        let mut m = Self::new(Kind::MethodCall);
        m.destination = destination
            .map(|d| name(names::is_bus_name, d, "bus name"))
            .transpose()?;
        m.path = Some(ObjectPath::new(path)?);
        m.interface = interface
            .map(|i| name(names::is_interface, i, "interface name"))
            .transpose()?;
        m.member = Some(name(names::is_member, member, "member name")?);
        Ok(m)
    }

    /// A signal `interface.member` from the object at `path`, to whoever
    /// matches it.
    ///
    /// # Errors
    ///
    /// As [`Message::method_call`].
    pub fn signal(path: &str, interface: &str, member: &str) -> Result<Self> {
        let mut m = Self::new(Kind::Signal);
        m.path = Some(ObjectPath::new(path)?);
        m.interface = Some(name(names::is_interface, interface, "interface name")?);
        m.member = Some(name(names::is_member, member, "member name")?);
        Ok(m)
    }

    /// The reply to `call`, addressed to its sender, in its byte order (as zbus
    /// builds one).
    #[must_use]
    pub fn method_return(call: &Message) -> Self {
        let mut m = Self::new(Kind::MethodReturn);
        m.endian = call.endian;
        m.reply_serial = Some(call.serial);
        m.destination = call.sender.clone();
        m
    }

    /// The error answer `name` to `call`, its body the one string `text`.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] unless `name` is an error name.
    pub fn error(call: &Message, name_: &str, text: &str) -> Result<Self> {
        let mut m = Self::method_return(call);
        m.kind = Kind::Error;
        m.error_name = Some(name(names::is_interface, name_, "error name")?);
        m.body = vec![Value::Str(text.to_owned())];
        Ok(m)
    }

    #[must_use]
    pub fn with_body(mut self, body: Vec<Value>) -> Self {
        self.body = body;
        self
    }

    #[must_use]
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    /// The body's signature.
    #[must_use]
    pub fn signature(&self) -> String {
        sig::signature_of(&self.body.iter().map(Value::type_of).collect::<Vec<_>>())
    }

    /// Whether the sender of this message waits for an answer.
    #[must_use]
    pub fn expects_reply(&self) -> bool {
        self.kind == Kind::MethodCall && self.flags & flags::NO_REPLY_EXPECTED == 0
    }

    /// The body's first value as a string: an error's text, `Hello`'s name.
    #[must_use]
    pub fn first_str(&self) -> Option<&str> {
        self.body.first().and_then(Value::as_str)
    }

    fn check(&self) -> Result<()> {
        if self.serial == 0 {
            return Err(Error::Invalid("serial 0"));
        }
        if self.reply_serial == Some(0) {
            return Err(Error::Invalid("reply serial 0"));
        }
        let checks: [NameCheck<'_>; 5] = [
            (&self.interface, names::is_interface, "interface name"),
            (&self.member, names::is_member, "member name"),
            (&self.error_name, names::is_interface, "error name"),
            (&self.destination, names::is_bus_name, "destination"),
            (&self.sender, names::is_bus_name, "sender"),
        ];
        for (field, valid, what) in checks {
            if field.as_deref().is_some_and(|text| !valid(text)) {
                return Err(Error::Invalid(what));
            }
        }
        required(self).map_err(Error::Invalid)
    }

    /// The bytes on the wire.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] for a message the specification forbids: serial 0, a
    /// required field missing, an invalid name, a body over the limits.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.check()?;
        let signature = self.signature();
        sig::parse_many(signature.as_bytes()).map_err(SigFault::refused)?;
        let body = wire::encode_checked(&self.body, self.endian, 0)?;
        let body_len =
            u32::try_from(body.len()).map_err(|_| Error::Invalid("message longer than 128 MiB"))?;

        let mut w = Writer::new(self.endian, 0);
        w.buf
            .extend_from_slice(&[self.endian.tag(), self.kind as u8, self.flags, 1]);
        w.u32(body_len);
        w.u32(self.serial);
        w.u32(0);
        w.align(8);
        let start = w.buf.len();
        let field = |w: &mut Writer, code: u8, value: Value| -> Result<()> {
            w.align(8);
            w.buf.push(code);
            w.value(&Value::variant(value))
        };
        if let Some(path) = &self.path {
            field(&mut w, PATH, Value::Path(path.clone()))?;
        }
        let strings = [
            (INTERFACE, &self.interface),
            (MEMBER, &self.member),
            (ERROR_NAME, &self.error_name),
        ];
        for (code, text) in strings {
            if let Some(text) = text {
                field(&mut w, code, Value::Str(text.clone()))?;
            }
        }
        if let Some(serial) = self.reply_serial {
            field(&mut w, REPLY_SERIAL, Value::U32(serial))?;
        }
        for (code, text) in [(DESTINATION, &self.destination), (SENDER, &self.sender)] {
            if let Some(text) = text {
                field(&mut w, code, Value::Str(text.clone()))?;
            }
        }
        if !signature.is_empty() {
            field(&mut w, SIGNATURE, Value::Sig(Signature::checked(signature)))?;
        }
        let fields_len = w.buf.len().saturating_sub(start);
        w.patch_u32(12, fields_len as u32);
        w.align(8);
        w.buf.extend_from_slice(&body);
        if w.buf.len() > MAX_MESSAGE {
            return Err(Error::Invalid("message longer than 128 MiB"));
        }
        Ok(w.buf)
    }

    /// Parses exactly one message.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] for bytes that break the specification,
    /// [`Error::Unsupported`] for an unknown message type or attached unix
    /// descriptors.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let total = frame_len(bytes)?.ok_or(Error::malformed(
            "shorter than a message header",
            bytes.len(),
        ))?;
        if total != bytes.len() {
            return Err(Error::malformed("length disagrees with the header", 4));
        }
        let head = |i: usize| bytes.get(i).copied().unwrap_or(0);
        let endian = Endian::from_tag(head(0)).ok_or(Error::malformed("unknown byte order", 0))?;
        if head(3) != 1 {
            return Err(Error::malformed("protocol version other than 1", 3));
        }
        let kind = match head(1) {
            1 => Kind::MethodCall,
            2 => Kind::MethodReturn,
            3 => Kind::Error,
            4 => Kind::Signal,
            _ => return Err(Error::Unsupported("message type")),
        };
        let mut m = Self::new(kind);
        m.endian = endian;
        m.flags = head(2);
        let mut r = Reader::new(bytes, endian, 0);
        r.pos = 4;
        r.u32()?;
        m.serial = r.u32()?;
        if m.serial == 0 {
            return Err(Error::malformed("serial 0", 8));
        }
        let fields = r.array(8, |r| {
            r.align(8)?;
            let at = r.at();
            let code = r.u8()?;
            Ok((at, code, r.value(&Type::Variant)?))
        })?;
        let mut signature = None;
        let mut seen = [false; UNIX_FDS as usize + 1];
        for (at, code, value) in fields {
            let Value::Variant(value) = value else {
                continue;
            };
            if code == 0 {
                return Err(Error::malformed("header field code 0 (INVALID)", at));
            }
            if let Some(seen) = seen.get_mut(usize::from(code)) {
                if *seen {
                    return Err(Error::malformed("header field repeated", at));
                }
                *seen = true;
            }
            let wrong = Error::malformed("header field of the wrong type or form", at);
            let text = |valid: fn(&str) -> bool| match &*value {
                Value::Str(s) if valid(s) => Ok(Some(s.clone())),
                _ => Err(Error::malformed(
                    "header field of the wrong type or form",
                    at,
                )),
            };
            match (code, &*value) {
                (PATH, Value::Path(path)) => m.path = Some(path.clone()),
                (INTERFACE, _) => m.interface = text(names::is_interface)?,
                (MEMBER, _) => m.member = text(names::is_member)?,
                (ERROR_NAME, _) => m.error_name = text(names::is_interface)?,
                (DESTINATION, _) => m.destination = text(names::is_bus_name)?,
                (SENDER, _) => m.sender = text(names::is_bus_name)?,
                (REPLY_SERIAL, Value::U32(serial)) if *serial != 0 => {
                    m.reply_serial = Some(*serial)
                }
                (SIGNATURE, Value::Sig(sig)) => signature = Some(sig.clone()),
                (UNIX_FDS, Value::U32(0)) => {}
                (UNIX_FDS, Value::U32(_)) => {
                    return Err(Error::Unsupported("unix descriptors attached to a message"));
                }
                (PATH | REPLY_SERIAL | SIGNATURE | UNIX_FDS, _) => return Err(wrong),
                _ => {}
            }
        }
        r.align(8)?;
        if let Err(what) = required(&m) {
            return Err(Error::malformed(what, 1));
        }
        let types = match &signature {
            Some(sig) => sig::parse_many(sig.as_str().as_bytes()).map_err(|f| f.on_wire(r.pos))?,
            None => Vec::new(),
        };
        let body = bytes.get(r.pos..).unwrap_or_default();
        m.body = wire::decode_checked(body, &types, endian, r.pos)?;
        Ok(m)
    }
}

/// The fields each message type must carry.
fn required(m: &Message) -> std::result::Result<(), &'static str> {
    let missing = match m.kind {
        Kind::MethodCall if m.path.is_none() || m.member.is_none() => {
            "method call without a path or member"
        }
        Kind::Signal if m.path.is_none() || m.interface.is_none() || m.member.is_none() => {
            "signal without a path, interface or member"
        }
        Kind::Error if m.error_name.is_none() || m.reply_serial.is_none() => {
            "error without a name or reply serial"
        }
        Kind::MethodReturn if m.reply_serial.is_none() => "method return without a reply serial",
        _ => return Ok(()),
    };
    Err(missing)
}

/// The header's length: the 16 bytes up to the fields' content, the fields
/// (at most [`MAX_ARRAY_BYTES`], 2^26, spelled as a literal in the contract),
/// and the padding to the body's 8-byte boundary.
#[cfg_attr(trust_verify, trust::requires(fields <= 67108864))]
#[cfg_attr(trust_verify, trust::ensures(result >= 16 && result % 8 == 0))]
fn header_len(fields: usize) -> usize {
    pad_to(16 + fields, 8)
}

/// How many bytes the message starting at `head` occupies, once its first 16
/// bytes are in: `None` until then.
///
/// # Errors
///
/// [`Error::Malformed`] for an unknown byte order or lengths past the
/// specification's limits: the stream cannot be resynchronised after that.
pub fn frame_len(head: &[u8]) -> Result<Option<usize>> {
    let Some(fixed) = head.first_chunk::<16>() else {
        return Ok(None);
    };
    let endian = Endian::from_tag(fixed[0]).ok_or(Error::malformed("unknown byte order", 0))?;
    let word = |b: [u8; 4]| match endian {
        Endian::Little => u32::from_le_bytes(b),
        Endian::Big => u32::from_be_bytes(b),
    };
    let fields = word([fixed[12], fixed[13], fixed[14], fixed[15]]) as usize;
    if fields > MAX_ARRAY_BYTES {
        return Err(Error::malformed("header fields longer than 64 MiB", 12));
    }
    let total =
        header_len(fields).saturating_add(word([fixed[4], fixed[5], fixed[6], fixed[7]]) as usize);
    if total > MAX_MESSAGE {
        return Err(Error::malformed("message longer than 128 MiB", 4));
    }
    Ok(Some(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> Message {
        let mut m = Message::method_call(Some(":1.9"), "/a", Some("org.a.B"), "C").unwrap();
        m.serial = 7;
        m
    }

    fn refused(bytes: &[u8]) -> Error {
        Message::decode(bytes).expect_err("decoded")
    }

    #[test]
    fn a_message_round_trips() {
        let m = call().with_body(vec![Value::from("x"), Value::U32(3)]);
        let bytes = m.encode().unwrap();
        assert_eq!(frame_len(&bytes).unwrap(), Some(bytes.len()));
        assert_eq!(frame_len(&bytes[..15]).unwrap(), None);
        assert_eq!(Message::decode(&bytes).unwrap(), m);
    }

    #[test]
    fn encode_holds_the_specification() {
        let mut m = call();
        m.serial = 0;
        assert!(matches!(m.encode(), Err(Error::Invalid("serial 0"))));
        let mut m = call();
        m.member = None;
        assert!(matches!(m.encode(), Err(Error::Invalid(_))));
        let mut m = call();
        m.sender = Some("not a name".into());
        assert!(matches!(m.encode(), Err(Error::Invalid("sender"))));
        assert!(Message::method_call(None, "/a/", None, "C").is_err());
        assert!(Message::signal("/a", "noDots", "C").is_err());
        assert!(Message::error(&call(), "bad", "x").is_err());
    }

    #[test]
    fn decode_refuses_a_broken_header() {
        let good = call().encode().unwrap();
        let mut bad = good.clone();
        bad[0] = b'x';
        assert!(matches!(
            refused(&bad),
            Error::Malformed {
                what: "unknown byte order",
                ..
            }
        ));
        let mut bad = good.clone();
        bad[3] = 2;
        assert!(matches!(
            refused(&bad),
            Error::Malformed {
                what: "protocol version other than 1",
                ..
            }
        ));
        let mut bad = good.clone();
        bad[1] = 9;
        assert!(matches!(refused(&bad), Error::Unsupported(_)));
        let mut bad = good.clone();
        bad[8..12].copy_from_slice(&[0; 4]);
        assert!(matches!(
            refused(&bad),
            Error::Malformed {
                what: "serial 0",
                ..
            }
        ));
        assert!(matches!(
            refused(&good[..good.len() - 1]),
            Error::Malformed { .. }
        ));
        let mut long = good.clone();
        long.push(0);
        assert!(matches!(
            refused(&long),
            Error::Malformed {
                what: "length disagrees with the header",
                ..
            }
        ));
    }

    #[test]
    fn decode_refuses_repeated_mistyped_and_missing_fields() {
        // A hand-built header: fields as (code, signature, value) through the
        // writer, so only the field set differs from a valid message.
        let build = |fields: &[(u8, Value)], kind: Kind| {
            let mut w = Writer::new(Endian::Little, 0);
            w.buf.extend_from_slice(&[b'l', kind as u8, 0, 1]);
            w.u32(0);
            w.u32(5);
            w.u32(0);
            w.align(8);
            for (code, value) in fields {
                w.align(8);
                w.buf.push(*code);
                w.value(&Value::variant(value.clone())).unwrap();
            }
            let len = w.buf.len() - 16;
            w.patch_u32(12, len as u32);
            w.align(8);
            w.buf
        };
        let path = || Value::path("/a").unwrap();
        let ok = build(
            &[(PATH, path()), (MEMBER, "M".into()), (42, Value::U64(1))],
            Kind::MethodCall,
        );
        assert_eq!(
            Message::decode(&ok).unwrap().member.as_deref(),
            Some("M"),
            "an unknown field is ignored"
        );
        let twice = build(
            &[(PATH, path()), (MEMBER, "M".into()), (MEMBER, "N".into())],
            Kind::MethodCall,
        );
        assert!(matches!(
            refused(&twice),
            Error::Malformed {
                what: "header field repeated",
                ..
            }
        ));
        let mistyped = build(
            &[(PATH, "/a".into()), (MEMBER, "M".into())],
            Kind::MethodCall,
        );
        assert!(matches!(
            refused(&mistyped),
            Error::Malformed {
                what: "header field of the wrong type or form",
                ..
            }
        ));
        let bad_name = build(&[(PATH, path()), (MEMBER, "M.N".into())], Kind::MethodCall);
        assert!(matches!(refused(&bad_name), Error::Malformed { .. }));
        let no_member = build(&[(PATH, path())], Kind::MethodCall);
        assert!(matches!(
            refused(&no_member),
            Error::Malformed {
                what: "method call without a path or member",
                ..
            }
        ));
        let invalid = build(
            &[(REPLY_SERIAL, Value::U32(1)), (0, Value::U32(1))],
            Kind::MethodReturn,
        );
        assert!(matches!(
            refused(&invalid),
            Error::Malformed {
                what: "header field code 0 (INVALID)",
                ..
            }
        ));
        let no_reply_serial = build(&[], Kind::MethodReturn);
        assert!(matches!(refused(&no_reply_serial), Error::Malformed { .. }));
        let zero_reply = build(&[(REPLY_SERIAL, Value::U32(0))], Kind::MethodReturn);
        assert!(matches!(refused(&zero_reply), Error::Malformed { .. }));
        let fds = build(
            &[(REPLY_SERIAL, Value::U32(1)), (UNIX_FDS, Value::U32(1))],
            Kind::MethodReturn,
        );
        assert!(matches!(refused(&fds), Error::Unsupported(_)));
        let no_fds = build(
            &[(REPLY_SERIAL, Value::U32(1)), (UNIX_FDS, Value::U32(0))],
            Kind::MethodReturn,
        );
        assert!(Message::decode(&no_fds).is_ok());
        // A body the signature field does not describe.
        let mut body = build(
            &[
                (REPLY_SERIAL, Value::U32(1)),
                (SIGNATURE, Value::Sig(Signature::new("u").unwrap())),
            ],
            Kind::MethodReturn,
        );
        assert!(matches!(refused(&body), Error::Malformed { .. }));
        body[4] = 4;
        body.extend_from_slice(&[1, 0, 0, 0]);
        assert_eq!(Message::decode(&body).unwrap().body, vec![Value::U32(1)]);
    }

    #[test]
    fn frame_len_refuses_lengths_past_the_limits() {
        let mut head = call().encode().unwrap();
        head[12..16].copy_from_slice(&(1u32 << 27).to_le_bytes());
        assert!(frame_len(&head).is_err());
        let mut head = call().encode().unwrap();
        head[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(frame_len(&head).is_err());
    }
}
