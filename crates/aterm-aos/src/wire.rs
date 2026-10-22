// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The canonical wire codec every Aos encoding is written in.
//!
//! # The law
//!
//! All integers are little-endian and fixed-width. A decoder accepts exactly the bytes the
//! encoder writes for some value and refuses every other byte string — the encoding is
//! *canonical*, so equal values have equal bytes and a digest over the bytes is a digest
//! over the value:
//!
//! * `bool` and `opt` tags are `0` or `1`; any other byte is [`WireFault::BadTag`].
//! * `duration` is `secs: u64, nanos: u32` with `nanos < 1_000_000_000`; a larger `nanos`
//!   is [`WireFault::NonCanonical`] (it names a duration another encoding also names).
//! * `offset` is a signed `i128` count of nanoseconds (16 bytes): time enters an Aos task as
//!   a checked offset from a declared logical epoch, never as an `Instant`.
//! * `bytes` and `str` carry a `u32` length prefix. A length of `u32::MAX` or more is written
//!   as `u32::MAX` followed by its `u64` length, and the long form is refused for any length
//!   the short form can hold, so every length has exactly one encoding. A `str` that is not
//!   UTF-8 is [`WireFault::NonCanonical`].
//! * An artifact is `kind: u8 | name: str | version: u32 | digest: [u8; 32]`; a kind byte no
//!   [`ArtifactKind`] uses is [`WireFault::BadTag`]. An identity is its five artifacts in the
//!   order task, code, contract, schema, profile, and each must carry the kind of its slot.
//! * [`Dec::finish`] refuses trailing bytes ([`WireFault::Trailing`]).
//!
//! Decoding is a pure, panic-free function of the bytes: every read is bounds-checked, every
//! length is checked against the bytes that remain before anything is copied, so a hostile
//! length can never make the decoder allocate more than its input. Every read names the
//! field it refused. Encoding cannot fail: [`Enc`] only appends.

use core::fmt;
use core::time::Duration;

use crate::ident::{ArtifactId, ArtifactKind, Digest, TaskIdentity, len_u64};

/// Why a byte string is not the canonical encoding of the value asked for. The bytes are
/// only read; nothing is decoded past the refused field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFault {
    /// The bytes ended inside `field`: it needs `need` bytes and `have` remain.
    Truncated {
        /// The field being read.
        field: &'static str,
        /// Bytes the field needs.
        need: usize,
        /// Bytes that remained.
        have: usize,
    },
    /// The value ended `extra` bytes before its bytes did.
    Trailing {
        /// Bytes left over.
        extra: usize,
    },
    /// A tag byte (`bool`, `opt`, an artifact kind, a record kind) no encoding writes.
    BadTag {
        /// The field being read.
        field: &'static str,
        /// The byte found.
        tag: u8,
    },
    /// A declared length or count past what the field may hold.
    TooLong {
        /// The field being read.
        field: &'static str,
        /// The length declared.
        len: u64,
        /// The most the field may hold.
        limit: u64,
    },
    /// Bytes the encoder never writes for any value (an over-long length form, a
    /// `nanos ≥ 10⁹`, a `str` that is not UTF-8, a value that re-encodes differently).
    NonCanonical {
        /// The field being read.
        field: &'static str,
    },
}

impl fmt::Display for WireFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireFault::Truncated { field, need, have } => write!(
                f,
                "the bytes end inside `{field}`, which needs {need} bytes where {have} remain"
            ),
            WireFault::Trailing { extra } => {
                write!(f, "{extra} bytes follow the end of the encoded value")
            }
            WireFault::BadTag { field, tag } => {
                write!(
                    f,
                    "`{field}` holds the tag {tag:#04x}, which no encoding writes"
                )
            }
            WireFault::TooLong { field, len, limit } => write!(
                f,
                "`{field}` declares {len}, more than the {limit} it may hold"
            ),
            WireFault::NonCanonical { field } => {
                write!(f, "`{field}` is not the canonical encoding of any value")
            }
        }
    }
}

impl std::error::Error for WireFault {}

/// The canonical encoder: an append-only byte buffer. Every method appends one field and
/// cannot fail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Enc {
    buf: Vec<u8>,
}

impl Enc {
    /// An empty encoder.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Enc { buf: Vec::new() }
    }

    /// An empty encoder with room for `n` bytes.
    #[inline]
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Enc {
            buf: Vec::with_capacity(n),
        }
    }

    /// Bytes written so far.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing has been written.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The bytes written so far.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    /// Fixed bytes with no length prefix (a magic). The reader must know their length.
    #[inline]
    pub fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// One byte.
    #[inline]
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// A `u16`, LE.
    #[inline]
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// A `u32`, LE.
    #[inline]
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// A `u64`, LE.
    #[inline]
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// An `i32`, LE two's complement.
    #[inline]
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// An `i64`, LE two's complement.
    #[inline]
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// A `bool` as `0` or `1`.
    #[inline]
    pub fn bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    /// An `f32` as its bit pattern (`to_bits`), so `-0.0` and every NaN payload round-trip.
    #[inline]
    pub fn f32_bits(&mut self, v: f32) {
        self.u32(v.to_bits());
    }

    /// A [`Duration`] as `secs: u64, nanos: u32`.
    #[inline]
    pub fn duration(&mut self, d: Duration) {
        self.u64(d.as_secs());
        self.u32(d.subsec_nanos());
    }

    /// A signed nanosecond offset from a declared epoch, as 16 bytes LE.
    #[inline]
    pub fn offset(&mut self, ns: i128) {
        self.buf.extend_from_slice(&ns.to_le_bytes());
    }

    /// A length prefix: `u32` below `u32::MAX`, else `u32::MAX` then the `u64` length.
    #[inline]
    fn len_prefix(&mut self, n: usize) {
        match u32::try_from(n) {
            Ok(short) if short < u32::MAX => self.u32(short),
            _ => {
                self.u32(u32::MAX);
                self.u64(len_u64(n));
            }
        }
    }

    /// Length-prefixed bytes.
    #[inline]
    pub fn bytes(&mut self, b: &[u8]) {
        self.len_prefix(b.len());
        self.buf.extend_from_slice(b);
    }

    /// A length-prefixed UTF-8 string.
    #[inline]
    pub fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    /// An option: tag `0`, or tag `1` then the value as `f` writes it.
    #[inline]
    pub fn opt<T>(&mut self, v: Option<T>, f: impl FnOnce(&mut Enc, T)) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                f(self, x);
            }
        }
    }

    /// A digest's 32 bytes.
    #[inline]
    pub fn digest(&mut self, d: &Digest) {
        self.buf.extend_from_slice(&d.0);
    }

    /// An artifact: `kind | name | version | digest`.
    pub fn artifact(&mut self, a: &ArtifactId) {
        self.u8(a.kind.byte());
        self.str(&a.name);
        self.u32(a.version);
        self.digest(&a.digest);
    }

    /// An identity: its five artifacts, task, code, contract, schema, profile.
    pub fn identity(&mut self, id: &TaskIdentity) {
        self.artifact(&id.task);
        self.artifact(&id.code);
        self.artifact(&id.contract);
        self.artifact(&id.schema);
        self.artifact(&id.profile);
    }

    /// The bytes written.
    #[inline]
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// The canonical decoder over a borrowed byte string. Every read names its field; a refused
/// read leaves the decoder where it was.
#[derive(Clone, Debug)]
pub struct Dec<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Dec<'a> {
    /// A decoder at the start of `bytes`.
    #[inline]
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Dec { bytes, pos: 0 }
    }

    /// Bytes not yet read.
    #[inline]
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    /// Bytes read so far.
    #[inline]
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// The next `n` bytes, or `Truncated` naming `field`.
    #[inline]
    pub fn raw(&mut self, field: &'static str, n: usize) -> Result<&'a [u8], WireFault> {
        let have = self.remaining();
        let end = self.pos.checked_add(n).filter(|_| n <= have);
        match end.and_then(|end| self.bytes.get(self.pos..end).map(|s| (s, end))) {
            Some((s, end)) => {
                self.pos = end;
                Ok(s)
            }
            None => Err(WireFault::Truncated {
                field,
                need: n,
                have,
            }),
        }
    }

    #[inline]
    fn array<const N: usize>(&mut self, field: &'static str) -> Result<[u8; N], WireFault> {
        let s = self.raw(field, N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(s);
        Ok(out)
    }

    /// One byte.
    #[inline]
    pub fn u8(&mut self, field: &'static str) -> Result<u8, WireFault> {
        Ok(self.array::<1>(field)?[0])
    }

    /// A `u16`, LE.
    #[inline]
    pub fn u16(&mut self, field: &'static str) -> Result<u16, WireFault> {
        Ok(u16::from_le_bytes(self.array(field)?))
    }

    /// A `u32`, LE.
    #[inline]
    pub fn u32(&mut self, field: &'static str) -> Result<u32, WireFault> {
        Ok(u32::from_le_bytes(self.array(field)?))
    }

    /// A `u64`, LE.
    #[inline]
    pub fn u64(&mut self, field: &'static str) -> Result<u64, WireFault> {
        Ok(u64::from_le_bytes(self.array(field)?))
    }

    /// An `i32`, LE two's complement.
    #[inline]
    pub fn i32(&mut self, field: &'static str) -> Result<i32, WireFault> {
        Ok(i32::from_le_bytes(self.array(field)?))
    }

    /// An `i64`, LE two's complement.
    #[inline]
    pub fn i64(&mut self, field: &'static str) -> Result<i64, WireFault> {
        Ok(i64::from_le_bytes(self.array(field)?))
    }

    /// A `bool`: `0` or `1`, else `BadTag`.
    #[inline]
    pub fn bool(&mut self, field: &'static str) -> Result<bool, WireFault> {
        let at = self.pos;
        match self.u8(field)? {
            0 => Ok(false),
            1 => Ok(true),
            tag => {
                self.pos = at;
                Err(WireFault::BadTag { field, tag })
            }
        }
    }

    /// An `f32` from its bit pattern.
    #[inline]
    pub fn f32_bits(&mut self, field: &'static str) -> Result<f32, WireFault> {
        Ok(f32::from_bits(self.u32(field)?))
    }

    /// A [`Duration`]: `secs: u64, nanos: u32` with `nanos < 1_000_000_000`.
    #[inline]
    pub fn duration(&mut self, field: &'static str) -> Result<Duration, WireFault> {
        let at = self.pos;
        let secs = self.u64(field)?;
        let nanos = match self.u32(field) {
            Ok(n) => n,
            Err(e) => {
                self.pos = at;
                return Err(e);
            }
        };
        if nanos >= 1_000_000_000 {
            self.pos = at;
            return Err(WireFault::NonCanonical { field });
        }
        Ok(Duration::new(secs, nanos))
    }

    /// A signed nanosecond offset (16 bytes LE).
    #[inline]
    pub fn offset(&mut self, field: &'static str) -> Result<i128, WireFault> {
        Ok(i128::from_le_bytes(self.array(field)?))
    }

    /// A length prefix in its one canonical form.
    #[inline]
    fn len_prefix(&mut self, field: &'static str) -> Result<u64, WireFault> {
        let short = self.u32(field)?;
        if short < u32::MAX {
            return Ok(u64::from(short));
        }
        let long = self.u64(field)?;
        if long < u64::from(u32::MAX) {
            return Err(WireFault::NonCanonical { field });
        }
        Ok(long)
    }

    /// Length-prefixed bytes, borrowed from the input.
    #[inline]
    pub fn bytes(&mut self, field: &'static str) -> Result<&'a [u8], WireFault> {
        let at = self.pos;
        let r = self.len_prefix(field).and_then(|len| {
            let n = usize::try_from(len).map_err(|_| WireFault::TooLong {
                field,
                len,
                limit: len_u64(usize::MAX),
            })?;
            self.raw(field, n)
        });
        if r.is_err() {
            self.pos = at;
        }
        r
    }

    /// A length-prefixed UTF-8 string, borrowed from the input.
    #[inline]
    pub fn str(&mut self, field: &'static str) -> Result<&'a str, WireFault> {
        let at = self.pos;
        let b = self.bytes(field)?;
        core::str::from_utf8(b).map_err(|_| {
            self.pos = at;
            WireFault::NonCanonical { field }
        })
    }

    /// An option: tag `0` is `None`; tag `1` is `Some` of what `f` reads; any other tag is
    /// `BadTag`.
    #[inline]
    pub fn opt<T>(
        &mut self,
        field: &'static str,
        f: impl FnOnce(&mut Dec<'a>) -> Result<T, WireFault>,
    ) -> Result<Option<T>, WireFault> {
        let at = self.pos;
        let r = match self.u8(field)? {
            0 => Ok(None),
            1 => f(self).map(Some),
            tag => Err(WireFault::BadTag { field, tag }),
        };
        if r.is_err() {
            self.pos = at;
        }
        r
    }

    /// A count of `limit` or fewer items, as `u64`: `TooLong` past `limit`. Read a count
    /// with this before allocating for the items.
    #[inline]
    pub fn count(&mut self, field: &'static str, limit: u64) -> Result<usize, WireFault> {
        let at = self.pos;
        let n = self.u64(field)?;
        let ok = if n > limit {
            None
        } else {
            usize::try_from(n).ok()
        };
        ok.ok_or_else(|| {
            self.pos = at;
            WireFault::TooLong {
                field,
                len: n,
                limit,
            }
        })
    }

    /// A digest's 32 bytes.
    #[inline]
    pub fn digest(&mut self, field: &'static str) -> Result<Digest, WireFault> {
        Ok(Digest(self.array(field)?))
    }

    /// An artifact of any kind.
    #[inline]
    pub fn artifact(&mut self, field: &'static str) -> Result<ArtifactId, WireFault> {
        self.artifact_of(field, None)
    }

    fn artifact_of(
        &mut self,
        field: &'static str,
        want: Option<ArtifactKind>,
    ) -> Result<ArtifactId, WireFault> {
        let at = self.pos;
        let r = (|| {
            let tag = self.u8(field)?;
            let kind = ArtifactKind::from_byte(tag)
                .filter(|k| want.is_none_or(|w| w == *k))
                .ok_or(WireFault::BadTag { field, tag })?;
            let name = self.str(field)?.to_owned();
            let version = self.u32(field)?;
            let digest = self.digest(field)?;
            Ok(ArtifactId {
                kind,
                name,
                version,
                digest,
            })
        })();
        if r.is_err() {
            self.pos = at;
        }
        r
    }

    /// An identity: five artifacts, each of its slot's kind (else `BadTag` naming the slot).
    pub fn identity(&mut self) -> Result<TaskIdentity, WireFault> {
        let at = self.pos;
        let r = (|| {
            Ok(TaskIdentity {
                task: self.artifact_of("identity.task", Some(ArtifactKind::Task))?,
                code: self.artifact_of("identity.code", Some(ArtifactKind::Code))?,
                contract: self.artifact_of("identity.contract", Some(ArtifactKind::Contract))?,
                schema: self.artifact_of("identity.schema", Some(ArtifactKind::Schema))?,
                profile: self.artifact_of("identity.profile", Some(ArtifactKind::Profile))?,
            })
        })();
        if r.is_err() {
            self.pos = at;
        }
        r
    }

    /// Ends the value: `Trailing` if any byte is left.
    #[inline]
    pub const fn finish(self) -> Result<(), WireFault> {
        match self.remaining() {
            0 => Ok(()),
            extra => Err(WireFault::Trailing { extra }),
        }
    }
}
