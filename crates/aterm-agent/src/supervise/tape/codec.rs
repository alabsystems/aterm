// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tape item codec ([`super::frame`], [`EffectRecord`], [`Item::Note`], [`Mark`], [`End`],
//! [`Stamp`], [`Encoder`], [`Decoder`], [`super::delta`], [`KINDS`], [`super::recover`],
//! [`ctl_request`], [`ctl_reply`]).

use std::collections::HashMap;
use std::io::ErrorKind;

use astream_effects::EffectRecord;

use super::delta;
use crate::supervise::CtlReply;

/// Where the clocks stood when a door was crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stamp {
    pub mono_ns: u64,
    pub wall_ms: i64,
}

/// One item of a tape ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// An effect the loop crossed a door for, astream's own record.
    Effect {
        record: EffectRecord,
        stamp: Option<Stamp>,
    },
    /// Something a reader of the tape wants and a replay never consumes.
    Note { kind: String, text: String },
    /// A mark in the run; it closes its frame, so it is on disk at once.
    Mark(Mark),
    /// The run's end; it closes its frame, and its frame is the one the tape is made durable after.
    End(End),
}

impl Item {
    /// An effect with no stamp.
    #[must_use]
    pub const fn effect(record: EffectRecord) -> Self {
        Self::Effect {
            record,
            stamp: None,
        }
    }

    /// An effect stamped when its door was crossed.
    #[must_use]
    pub const fn stamped(record: EffectRecord, stamp: Stamp) -> Self {
        Self::Effect {
            record,
            stamp: Some(stamp),
        }
    }

    /// The effect this item is, if it is one: what a replay feeds back.
    #[must_use]
    pub const fn record(&self) -> Option<&EffectRecord> {
        match self {
            Self::Effect { record, .. } => Some(record),
            Self::Note { .. } | Self::Mark(_) | Self::End(_) => None,
        }
    }

    /// Whether this item closes the frame it is in: a mark and the end are written at once.
    #[must_use]
    pub const fn closes_frame(&self) -> bool {
        matches!(self, Self::Mark(_) | Self::End(_))
    }
}

/// A mark in the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// A door reached inside another door's live call ran live and was taped nowhere.
    Nested { inner: String, outer: String },
    /// The run reached its cap.
    Truncated { door: u64 },
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// The loop returned; `how` is its outcome as the run said it.
    Returned { how: String },
    /// The loop was suspended for a seamless update: the successor's run opens a tape of its own.
    Suspended,
    /// The loop unwound: a panic, with its message.
    Unwound { message: String },
}

/// A frame's bytes that are not the items they claim to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecError {
    /// The offset in the frame's raw bytes.
    pub at: usize,
    pub why: &'static str,
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {} of its frame", self.why, self.at)
    }
}

const CLOCK: u8 = 1;
const RAND: u8 = 2;
const FILE: u8 = 3;
const FILE_ERR: u8 = 4;
const MONO: u8 = 5;
const EXCHANGE: u8 = 6;
const EMIT: u8 = 7;
const RAISED: u8 = 8;
const NOTE: u8 = 9;
const MARK: u8 = 10;
const END: u8 = 11;
const STAMPED: u8 = 0x80;

const PATH_VERBATIM: u8 = 0;
const PATH_ROOT: u8 = 1;
const PATH_UNDER: u8 = 2;

const MARK_NESTED: u8 = 0;
const MARK_TRUNCATED: u8 = 1;
const END_RETURNED: u8 = 0;
const END_SUSPENDED: u8 = 1;
const END_UNWOUND: u8 = 2;

/// THE ERROR KINDS' TABLE: a failed file read's kind as one byte.
pub(crate) const KINDS: [(u8, ErrorKind); 39] = [
    (0, ErrorKind::Other),
    (1, ErrorKind::NotFound),
    (2, ErrorKind::PermissionDenied),
    (3, ErrorKind::ConnectionRefused),
    (4, ErrorKind::ConnectionReset),
    (5, ErrorKind::HostUnreachable),
    (6, ErrorKind::NetworkUnreachable),
    (7, ErrorKind::ConnectionAborted),
    (8, ErrorKind::NotConnected),
    (9, ErrorKind::AddrInUse),
    (10, ErrorKind::AddrNotAvailable),
    (11, ErrorKind::NetworkDown),
    (12, ErrorKind::BrokenPipe),
    (13, ErrorKind::AlreadyExists),
    (14, ErrorKind::WouldBlock),
    (15, ErrorKind::NotADirectory),
    (16, ErrorKind::IsADirectory),
    (17, ErrorKind::DirectoryNotEmpty),
    (18, ErrorKind::ReadOnlyFilesystem),
    (19, ErrorKind::StaleNetworkFileHandle),
    (20, ErrorKind::InvalidInput),
    (21, ErrorKind::InvalidData),
    (22, ErrorKind::TimedOut),
    (23, ErrorKind::WriteZero),
    (24, ErrorKind::StorageFull),
    (25, ErrorKind::NotSeekable),
    (26, ErrorKind::QuotaExceeded),
    (27, ErrorKind::FileTooLarge),
    (28, ErrorKind::ResourceBusy),
    (29, ErrorKind::ExecutableFileBusy),
    (30, ErrorKind::Deadlock),
    (31, ErrorKind::CrossesDevices),
    (32, ErrorKind::TooManyLinks),
    (33, ErrorKind::InvalidFilename),
    (34, ErrorKind::ArgumentListTooLong),
    (35, ErrorKind::Interrupted),
    (36, ErrorKind::Unsupported),
    (37, ErrorKind::UnexpectedEof),
    (38, ErrorKind::OutOfMemory),
];

/// `kind`'s code ([`KINDS`]); `Other`'s for a kind the table does not hold.
pub(crate) fn kind_code(kind: ErrorKind) -> u8 {
    KINDS
        .iter()
        .find(|(_, k)| *k == kind)
        .map_or(0, |(code, _)| *code)
}

/// The kind `code` stands for ([`KINDS`]); `Other` for a code this build does not know.
pub(crate) fn kind_of(code: u8) -> ErrorKind {
    KINDS
        .iter()
        .find(|(c, _)| *c == code)
        .map_or(ErrorKind::Other, |(_, kind)| *kind)
}

/// A length as the tape writes it.
pub(crate) fn len64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Append `v` as a LEB128 varint.
pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Append `v` zigzagged, as a varint: a small step either way is one byte.
pub(crate) fn put_signed(out: &mut Vec<u8>, v: i64) {
    put_varint(out, ((v << 1) ^ (v >> 63)) as u64);
}

/// Append `bytes`, length first.
pub(crate) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(out, len64(bytes.len()));
    out.extend_from_slice(bytes);
}

/// The step from `prev` to `next`, wrapping: what a clock is stored as.
const fn step(prev: u64, next: u64) -> i64 {
    next.wrapping_sub(prev) as i64
}

/// [`step`]'s inverse.
const fn stepped(prev: u64, step: i64) -> u64 {
    prev.wrapping_add(step as u64)
}

/// A frame's raw bytes, read item by item.
#[derive(Debug)]
pub(crate) struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Input<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    /// A refusal at the current offset.
    pub(crate) const fn error(&self, why: &'static str) -> CodecError {
        CodecError { at: self.at, why }
    }

    pub(crate) fn byte(&mut self) -> Result<u8, CodecError> {
        let b = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| self.error("the frame ends inside an item"))?;
        self.at += 1;
        Ok(b)
    }

    pub(crate) fn varint(&mut self) -> Result<u64, CodecError> {
        let mut v: u64 = 0;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            let low = u64::from(b & 0x7f);
            if shift == 63 && low > 1 {
                return Err(self.error("a varint past 64 bits"));
            }
            v |= low << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(self.error("a varint past 64 bits"))
    }

    pub(crate) fn signed(&mut self) -> Result<i64, CodecError> {
        let z = self.varint()?;
        Ok(((z >> 1) as i64) ^ -((z & 1) as i64))
    }

    /// The next `n` bytes.
    pub(crate) fn take(&mut self, n: u64) -> Result<&'a [u8], CodecError> {
        let n = usize::try_from(n).map_err(|_| self.error("a length past the frame"))?;
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| self.error("a length past the frame"))?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    /// Bytes written length first ([`put_bytes`]).
    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], CodecError> {
        let n = self.varint()?;
        self.take(n)
    }

    /// Text written length first.
    pub(crate) fn text(&mut self) -> Result<String, CodecError> {
        let at = self.at;
        let bytes = self.bytes()?;
        String::from_utf8(bytes.to_vec()).map_err(|_| CodecError {
            at,
            why: "text that is not UTF-8",
        })
    }

    /// Everything left.
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let out = &self.bytes[self.at.min(self.bytes.len())..];
        self.at = self.bytes.len();
        out
    }
}

/// A path's root and what is under it.
fn rooted(path: &str) -> Option<(&str, Option<&str>)> {
    let rest = path.strip_prefix('@')?;
    if rest.starts_with('@') {
        return None;
    }
    let (root, under) = match rest.split_once('/') {
        Some((root, under)) => (root, Some(under)),
        None => (rest, None),
    };
    (!root.is_empty()).then_some((root, under))
}

/// What a file's encoder carries from one item to the next.
#[derive(Debug, Default)]
pub(crate) struct Encoder {
    names: HashMap<String, u64>,
    clock: u64,
    mono: u64,
    stamp: Stamp,
    bodies: delta::Table,
}

impl Encoder {
    /// Append `item`'s bytes to `out`.
    pub(crate) fn item(&mut self, item: &Item, out: &mut Vec<u8>) {
        match item {
            Item::Effect { record, stamp } => self.effect(record, *stamp, out),
            Item::Note { kind, text } => {
                out.push(NOTE);
                self.name(kind, out);
                put_bytes(out, text.as_bytes());
            }
            Item::Mark(Mark::Nested { inner, outer }) => {
                out.push(MARK);
                out.push(MARK_NESTED);
                self.name(inner, out);
                self.name(outer, out);
            }
            Item::Mark(Mark::Truncated { door }) => {
                out.push(MARK);
                out.push(MARK_TRUNCATED);
                put_varint(out, *door);
            }
            Item::End(End::Returned { how }) => {
                out.push(END);
                out.push(END_RETURNED);
                put_bytes(out, how.as_bytes());
            }
            Item::End(End::Suspended) => {
                out.push(END);
                out.push(END_SUSPENDED);
            }
            Item::End(End::Unwound { message }) => {
                out.push(END);
                out.push(END_UNWOUND);
                put_bytes(out, message.as_bytes());
            }
        }
    }

    fn effect(&mut self, record: &EffectRecord, stamp: Option<Stamp>, out: &mut Vec<u8>) {
        let tag = match record {
            EffectRecord::Clock(_) => CLOCK,
            EffectRecord::Rand(_) => RAND,
            EffectRecord::File { .. } => FILE,
            EffectRecord::FileErr { .. } => FILE_ERR,
            EffectRecord::Mono(_) => MONO,
            EffectRecord::Exchange { .. } => EXCHANGE,
            EffectRecord::Emit { .. } => EMIT,
            EffectRecord::Raised { .. } => RAISED,
        };
        match stamp {
            Some(stamp) => {
                out.push(tag | STAMPED);
                put_signed(out, step(self.stamp.mono_ns, stamp.mono_ns));
                put_signed(out, step(self.stamp.wall_ms as u64, stamp.wall_ms as u64));
                self.stamp = stamp;
            }
            None => out.push(tag),
        }
        match record {
            EffectRecord::Clock(ns) => {
                put_signed(out, step(self.clock, *ns));
                self.clock = *ns;
            }
            EffectRecord::Rand(word) => put_varint(out, *word),
            EffectRecord::File { path, bytes } => {
                self.path(path, out);
                put_bytes(out, bytes);
            }
            EffectRecord::FileErr { path, kind } => {
                self.path(path, out);
                out.push(kind_code(*kind));
            }
            EffectRecord::Mono(ns) => {
                put_signed(out, step(self.mono, *ns));
                self.mono = *ns;
            }
            EffectRecord::Exchange {
                door,
                request,
                reply,
            } => {
                self.name(door, out);
                put_bytes(out, request);
                self.bodies
                    .encode(delta::Key::Exchange { door, request }, reply, out);
            }
            EffectRecord::Emit { door, bytes } => {
                self.name(door, out);
                self.bodies.encode(delta::Key::Emit { door }, bytes, out);
            }
            EffectRecord::Raised { door, false_reads } => {
                self.name(door, out);
                put_varint(out, *false_reads);
            }
        }
    }

    /// A name: its number once the file has spelled it.
    fn name(&mut self, name: &str, out: &mut Vec<u8>) {
        if let Some(id) = self.names.get(name) {
            put_varint(out, id.saturating_add(1));
            return;
        }
        put_varint(out, 0);
        put_bytes(out, name.as_bytes());
        let id = len64(self.names.len());
        self.names.insert(name.to_string(), id);
    }

    fn path(&mut self, path: &str, out: &mut Vec<u8>) {
        match rooted(path) {
            Some((root, None)) => {
                out.push(PATH_ROOT);
                self.name(root, out);
            }
            Some((root, Some(under))) => {
                out.push(PATH_UNDER);
                self.name(root, out);
                put_bytes(out, under.as_bytes());
            }
            None => {
                out.push(PATH_VERBATIM);
                put_bytes(out, path.as_bytes());
            }
        }
    }
}

/// What a file's decoder carries from one item to the next: the [`Encoder`]'s state.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    names: Vec<String>,
    /// The bytes of every name in `names`.
    spelled: usize,
    clock: u64,
    mono: u64,
    stamp: Stamp,
    bodies: delta::Table,
}

impl Decoder {
    /// The bytes of every name the file has spelled so far.
    pub(crate) const fn spelled(&self) -> usize {
        self.spelled
    }

    /// The next item of `input`.
    pub(crate) fn item(&mut self, input: &mut Input<'_>) -> Result<Item, CodecError> {
        let at = input.at;
        let tag = input.byte()?;
        let stamp = if tag & STAMPED == 0 {
            None
        } else {
            let mono_ns = stepped(self.stamp.mono_ns, input.signed()?);
            let wall_ms = stepped(self.stamp.wall_ms as u64, input.signed()?) as i64;
            self.stamp = Stamp { mono_ns, wall_ms };
            Some(self.stamp)
        };
        let record = match tag & !STAMPED {
            CLOCK => {
                self.clock = stepped(self.clock, input.signed()?);
                EffectRecord::Clock(self.clock)
            }
            RAND => EffectRecord::Rand(input.varint()?),
            FILE => EffectRecord::File {
                path: self.path(input)?,
                bytes: input.bytes()?.to_vec(),
            },
            FILE_ERR => EffectRecord::FileErr {
                path: self.path(input)?,
                kind: kind_of(input.byte()?),
            },
            MONO => {
                self.mono = stepped(self.mono, input.signed()?);
                EffectRecord::Mono(self.mono)
            }
            EXCHANGE => {
                let door = self.name(input)?;
                let request = input.bytes()?.to_vec();
                let reply = self.bodies.decode(
                    delta::Key::Exchange {
                        door: &door,
                        request: &request,
                    },
                    input,
                )?;
                EffectRecord::Exchange {
                    door,
                    request,
                    reply,
                }
            }
            EMIT => {
                let door = self.name(input)?;
                let bytes = self
                    .bodies
                    .decode(delta::Key::Emit { door: &door }, input)?;
                EffectRecord::Emit { door, bytes }
            }
            RAISED => EffectRecord::Raised {
                door: self.name(input)?,
                false_reads: input.varint()?,
            },
            other => {
                if stamp.is_some() {
                    return Err(CodecError {
                        at,
                        why: "a stamp on an item that is no effect",
                    });
                }
                return self.aterm_item(other, at, input);
            }
        };
        Ok(Item::Effect { record, stamp })
    }

    /// One of aterm's own items (a note, a mark, the end).
    fn aterm_item(
        &mut self,
        tag: u8,
        at: usize,
        input: &mut Input<'_>,
    ) -> Result<Item, CodecError> {
        Ok(match tag {
            NOTE => Item::Note {
                kind: self.name(input)?,
                text: input.text()?,
            },
            MARK => match input.byte()? {
                MARK_NESTED => Item::Mark(Mark::Nested {
                    inner: self.name(input)?,
                    outer: self.name(input)?,
                }),
                MARK_TRUNCATED => Item::Mark(Mark::Truncated {
                    door: input.varint()?,
                }),
                _ => return Err(input.error("a mark this build does not know")),
            },
            END => match input.byte()? {
                END_RETURNED => Item::End(End::Returned { how: input.text()? }),
                END_SUSPENDED => Item::End(End::Suspended),
                END_UNWOUND => Item::End(End::Unwound {
                    message: input.text()?,
                }),
                _ => return Err(input.error("an end this build does not know")),
            },
            _ => {
                return Err(CodecError {
                    at,
                    why: "an item this build does not know",
                });
            }
        })
    }

    fn name(&mut self, input: &mut Input<'_>) -> Result<String, CodecError> {
        match input.varint()? {
            0 => {
                let name = input.text()?;
                self.spelled += name.len();
                self.names.push(name.clone());
                Ok(name)
            }
            n => usize::try_from(n - 1)
                .ok()
                .and_then(|id| self.names.get(id))
                .cloned()
                .ok_or_else(|| input.error("a name the file never spelled")),
        }
    }

    fn path(&mut self, input: &mut Input<'_>) -> Result<String, CodecError> {
        match input.byte()? {
            PATH_VERBATIM => input.text(),
            PATH_ROOT => Ok(format!("@{}", self.name(input)?)),
            PATH_UNDER => {
                let root = self.name(input)?;
                Ok(format!("@{root}/{}", input.text()?))
            }
            _ => Err(input.error("a path form this build does not know")),
        }
    }
}

/// THE CONTROL LANE'S REQUEST, as an `Exchange`'s request bytes.
#[must_use]
pub fn ctl_request(args: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    put_varint(&mut out, len64(args.len()));
    for arg in args {
        put_bytes(&mut out, arg.as_bytes());
    }
    out
}

/// [`ctl_request`]'s words, read back.
///
/// # Errors
/// Bytes [`ctl_request`] did not write.
pub fn ctl_args(request: &[u8]) -> Result<Vec<String>, CodecError> {
    let mut input = Input::new(request);
    let count = input.varint()?;
    let mut args = Vec::new();
    for _ in 0..count {
        args.push(input.text()?);
    }
    if !input.is_empty() {
        return Err(input.error("bytes after the request's last word"));
    }
    Ok(args)
}

const REPLY_OK: u8 = 0;
const REPLY_ERR: u8 = 1;

/// Control lane `Exchange` reply bytes ([`super::delta`]).
#[must_use]
pub fn ctl_reply(reply: &Result<CtlReply, String>) -> Vec<u8> {
    let mut out = Vec::new();
    match reply {
        Ok(CtlReply {
            code,
            stdout,
            stderr,
        }) => {
            out.push(REPLY_OK);
            put_signed(&mut out, i64::from(*code));
            put_bytes(&mut out, stderr.as_bytes());
            out.extend_from_slice(stdout.as_bytes());
        }
        Err(why) => {
            out.push(REPLY_ERR);
            out.extend_from_slice(why.as_bytes());
        }
    }
    out
}

/// [`ctl_reply`]'s reply, read back.
///
/// # Errors
/// Bytes [`ctl_reply`] did not write.
pub fn ctl_reply_of(bytes: &[u8]) -> Result<Result<CtlReply, String>, CodecError> {
    let mut input = Input::new(bytes);
    let text = |input: &mut Input<'_>, bytes: &[u8]| {
        String::from_utf8(bytes.to_vec()).map_err(|_| input.error("text that is not UTF-8"))
    };
    match input.byte()? {
        REPLY_OK => {
            let code = i32::try_from(input.signed()?)
                .map_err(|_| input.error("an exit code past 32 bits"))?;
            let stderr = input.text()?;
            let rest = input.rest();
            let stdout = text(&mut input, rest)?;
            Ok(Ok(CtlReply {
                code,
                stdout,
                stderr,
            }))
        }
        REPLY_ERR => {
            let rest = input.rest();
            Ok(Err(text(&mut input, rest)?))
        }
        _ => Err(input.error("a reply form this build does not know")),
    }
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
