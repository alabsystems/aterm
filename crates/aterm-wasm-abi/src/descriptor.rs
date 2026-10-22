// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The descriptor: what each `#[export]` tells the glue generator, carried in
//! the module's `aterm_abi` custom section.
//!
//! `#[export]` emits one record per export as a `link_section` static in
//! that section (the linker keeps those as they are; `#[used]` would add a
//! second copy to the data segment), and the linker concatenates them.
//! `verify glue` reads the section from the pre-bind module
//! ([`decode`]), checks every record against the module's real export
//! signatures, writes the JS glue and strips the section. A module without
//! the section, or with a record of another [`VERSION`], is refused there,
//! never guessed at.
//!
//! Record layout (integers little-endian):
//!
//! ```text
//! record  = MAGIC "atab", VERSION u8, body length u32, body
//! body    = kind u8, class str, js name str, symbol str, docs str,
//!           [function kinds only] receiver u8, n u8, (name str, ty)^n, ty
//! str     = length u32, UTF-8 bytes
//! ty      = tag u8, payload (see [`tag`])
//! ```
//!
//! Types are described by the traits, not by the tokens the macro saw: the
//! macro emits `<T as Describe>::DESC` for each parameter and return type, so
//! an alias or a `Self` still describes the type the export really takes.
//! Records are assembled at compile time by [`record`] from [`Part`]s.

use std::fmt;

/// The custom section the records live in.
pub const SECTION: &str = "aterm_abi";

/// The first four bytes of every record.
pub const MAGIC: [u8; 4] = *b"atab";

/// The record format this crate writes and the generator reads. Bump it with
/// any change to the layout above, to a [`tag`], to a type's lowering
/// ([`Ty::param_words`], [`Ty::ret_word`]) or to the return slot
/// (`crate::mem`). Version 2: a throw is left in the return slot, not
/// thrown through an `aterm_abi.throw` import. Version 3: a JS object
/// parameter ([`tag::JS_OBJECT`], [`crate::js`]).
pub const VERSION: u8 = 3;

/// Bytes before a record's body: [`MAGIC`], [`VERSION`] and the body length.
pub const HEADER_LEN: usize = 9;

/// Record kinds (the body's first byte).
pub mod kind {
    /// `#[export] struct`: a JS class. Its symbol is the class's free export.
    pub const CLASS: u8 = 1;
    /// `#[export] fn` at module level.
    pub const FUNCTION: u8 = 2;
    /// A `pub fn` with `&self` or `&mut self` in an `#[export] impl`.
    pub const METHOD: u8 = 3;
    /// A `pub fn` without a receiver in an `#[export] impl`: `static`.
    pub const STATIC: u8 = 4;
    /// `#[export(getter)]`: a read-only JS property.
    pub const GETTER: u8 = 5;
    /// `#[export(constructor)]`: the JS class's `constructor`.
    pub const CONSTRUCTOR: u8 = 6;
}

/// Receivers (a function record's byte after the docs).
pub mod receiver {
    /// No `self`: a free function, a static method or a constructor.
    pub const NONE: u8 = 0;
    /// `&self`.
    pub const SHARED: u8 = 1;
    /// `&mut self`.
    pub const EXCLUSIVE: u8 = 2;
}

/// Type tags. A scalar is its tag alone; the others carry a payload.
pub mod tag {
    /// `()`, as a return type only.
    pub const UNIT: u8 = 0x01;
    /// `bool`.
    pub const BOOL: u8 = 0x02;
    /// `u8`.
    pub const U8: u8 = 0x03;
    /// `u16`.
    pub const U16: u8 = 0x04;
    /// `u32`.
    pub const U32: u8 = 0x05;
    /// `u64`.
    pub const U64: u8 = 0x06;
    /// `usize`.
    pub const USIZE: u8 = 0x07;
    /// `i8`.
    pub const I8: u8 = 0x08;
    /// `i16`.
    pub const I16: u8 = 0x09;
    /// `i32`.
    pub const I32: u8 = 0x0A;
    /// `i64`.
    pub const I64: u8 = 0x0B;
    /// `isize`.
    pub const ISIZE: u8 = 0x0C;
    /// `f32`.
    pub const F32: u8 = 0x0D;
    /// `f64`.
    pub const F64: u8 = 0x0E;
    /// `char`.
    pub const CHAR: u8 = 0x0F;
    /// `str`, behind a `&` parameter.
    pub const STR: u8 = 0x10;
    /// `String`.
    pub const STRING: u8 = 0x11;
    /// `[T]` behind a `&` parameter; payload: the element's tag.
    pub const SLICE: u8 = 0x12;
    /// `Vec<T>`; payload: the element's tag.
    pub const VEC: u8 = 0x13;
    /// `Option<T>`; payload: `T`.
    pub const OPTION: u8 = 0x20;
    /// `Result<T, E>`; payload: `T` then `E`.
    pub const RESULT: u8 = 0x21;
    /// An exported class; payload: name length u8, then the name.
    pub const CLASS: u8 = 0x30;
    /// A JS object the host hands in ([`crate::js`]), as a parameter only;
    /// payload: the TypeScript type's name length u8, then the name.
    pub const JS_OBJECT: u8 = 0x31;
}

/// A type's description, built at compile time: a tag and its payload in a
/// fixed buffer, so generic impls (`Option<T>`) can compose them in a `const`.
#[derive(Clone, Copy)]
pub struct TypeDesc {
    len: u8,
    bytes: [u8; TypeDesc::CAP],
}

impl TypeDesc {
    /// The longest description: a class name is at most this less a few
    /// wrapper bytes. Exceeding it is a compile-time error.
    pub const CAP: usize = 96;

    /// A scalar (or `()`, `str`, `String`): its tag alone.
    #[inline]
    #[must_use]
    pub const fn tag(tag: u8) -> Self {
        let mut bytes = [0; Self::CAP];
        bytes[0] = tag;
        Self { len: 1, bytes }
    }

    /// `tag` followed by `inner` (`Option`, `[T]`, `Vec`).
    #[inline]
    #[must_use]
    pub const fn wrap(tag: u8, inner: TypeDesc) -> Self {
        Self::pair(tag, inner, Self::empty())
    }

    /// `tag` followed by `a` then `b` (`Result`).
    #[inline]
    #[must_use]
    pub const fn pair(tag: u8, a: TypeDesc, b: TypeDesc) -> Self {
        let total = 1 + a.len as usize + b.len as usize;
        assert!(
            total <= Self::CAP,
            "aterm-wasm-abi: type description too long"
        );
        let mut bytes = [0; Self::CAP];
        bytes[0] = tag;
        let mut i = 0;
        while i < a.len as usize {
            bytes[1 + i] = a.bytes[i];
            i += 1;
        }
        let mut j = 0;
        while j < b.len as usize {
            bytes[1 + i + j] = b.bytes[j];
            j += 1;
        }
        Self {
            len: total as u8,
            bytes,
        }
    }

    /// An exported class named `name`.
    #[inline]
    #[must_use]
    pub const fn class(name: &str) -> Self {
        Self::named(tag::CLASS, name)
    }

    /// A JS object whose TypeScript type is `ts` ([`crate::js`]).
    #[inline]
    #[must_use]
    pub const fn js_object(ts: &str) -> Self {
        Self::named(tag::JS_OBJECT, ts)
    }

    /// `tag` followed by the length of `name`, then `name`.
    #[inline]
    const fn named(tag: u8, name: &str) -> Self {
        let name = name.as_bytes();
        assert!(
            name.len() + 2 <= Self::CAP,
            "aterm-wasm-abi: class name too long"
        );
        let mut bytes = [0; Self::CAP];
        bytes[0] = tag;
        bytes[1] = name.len() as u8;
        let mut i = 0;
        while i < name.len() {
            bytes[2 + i] = name[i];
            i += 1;
        }
        Self {
            len: (name.len() + 2) as u8,
            bytes,
        }
    }

    #[inline]
    const fn empty() -> Self {
        Self {
            len: 0,
            bytes: [0; Self::CAP],
        }
    }

    /// The encoded description.
    #[inline]
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        self.bytes.split_at(self.len as usize).0
    }
}

impl fmt::Debug for TypeDesc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match decode_ty(self.as_bytes()) {
            Ok(ty) => write!(f, "TypeDesc({ty:?})"),
            Err(_) => write!(f, "TypeDesc({:?})", self.as_bytes()),
        }
    }
}

/// One piece of a record: bytes the macro encoded (kind, strings, counts) or
/// a type's description.
#[derive(Clone, Copy, Debug)]
pub enum Part {
    /// Bytes copied as they are.
    Bytes(&'static [u8]),
    /// A type, through its [`crate::marshal::Describe`] impl.
    Ty(TypeDesc),
}

impl Part {
    #[inline]
    const fn bytes(&self) -> &[u8] {
        match self {
            Part::Bytes(b) => b,
            Part::Ty(t) => t.as_bytes(),
        }
    }
}

/// The length of the record [`record`] assembles from `parts`, header
/// included: the static's array length.
#[inline]
#[must_use]
pub const fn record_len(parts: &[Part]) -> usize {
    let mut n = HEADER_LEN;
    let mut i = 0;
    while i < parts.len() {
        n += parts[i].bytes().len();
        i += 1;
    }
    n
}

/// The record for `parts`: [`MAGIC`], [`VERSION`], the body length, then
/// each part's bytes. `N` must be [`record_len`]`(parts)`.
#[inline]
#[must_use]
pub const fn record<const N: usize>(parts: &[Part]) -> [u8; N] {
    assert!(N == record_len(parts), "aterm-wasm-abi: record length");
    let mut out = [0; N];
    let mut at = 0;
    while at < MAGIC.len() {
        out[at] = MAGIC[at];
        at += 1;
    }
    out[at] = VERSION;
    at += 1;
    let body = ((N - HEADER_LEN) as u32).to_le_bytes();
    let mut k = 0;
    while k < body.len() {
        out[at] = body[k];
        at += 1;
        k += 1;
    }
    let mut i = 0;
    while i < parts.len() {
        let bytes = parts[i].bytes();
        let mut j = 0;
        while j < bytes.len() {
            out[at] = bytes[j];
            at += 1;
            j += 1;
        }
        i += 1;
    }
    out
}

/// A class's handle tag: FNV-1a/32 of its name, never 0 (0 marks a freed
/// handle). Two classes of one name would share it; the generator refuses
/// a module that exports two.
#[must_use]
pub const fn class_tag(name: &str) -> u32 {
    let bytes = name.as_bytes();
    let mut h: u32 = 0x811c_9dc5;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u32;
        h = h.wrapping_mul(0x0100_0193);
        i += 1;
    }
    if h == 0 { 1 } else { h }
}

// Decoding: what the generator reads.

/// A described type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    /// `()`.
    Unit,
    /// `bool`.
    Bool,
    /// `u8`.
    U8,
    /// `u16`.
    U16,
    /// `u32`.
    U32,
    /// `u64`.
    U64,
    /// `usize`.
    Usize,
    /// `i8`.
    I8,
    /// `i16`.
    I16,
    /// `i32`.
    I32,
    /// `i64`.
    I64,
    /// `isize`.
    Isize,
    /// `f32`.
    F32,
    /// `f64`.
    F64,
    /// `char`.
    Char,
    /// `&str`.
    Str,
    /// `String`.
    String,
    /// `&[T]`.
    Slice(Box<Ty>),
    /// `Vec<T>`.
    Vec(Box<Ty>),
    /// `Option<T>`.
    Option(Box<Ty>),
    /// `Result<T, E>`.
    Result(Box<Ty>, Box<Ty>),
    /// An exported class.
    Class(String),
    /// A JS object of the named TypeScript type ([`crate::js`]).
    JsObject(String),
}

/// A wasm value type, as the module's type section spells a signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValType {
    /// `i32`: every pointer, length, tag and 32-bit-or-narrower integer.
    I32,
    /// `i64`: `u64`, `i64`; a JS `BigInt`.
    I64,
    /// `f32`.
    F32,
    /// `f64`.
    F64,
}

impl Ty {
    /// Whether this is a scalar: one word, no memory.
    #[must_use]
    pub fn is_scalar(&self) -> bool {
        matches!(
            self,
            Ty::Bool
                | Ty::U8
                | Ty::U16
                | Ty::U32
                | Ty::U64
                | Ty::Usize
                | Ty::I8
                | Ty::I16
                | Ty::I32
                | Ty::I64
                | Ty::Isize
                | Ty::F32
                | Ty::F64
                | Ty::Char
        )
    }

    /// Whether this can be a `&[T]` or `Vec<T>` element: a number with a JS
    /// typed array.
    #[must_use]
    pub fn is_element(&self) -> bool {
        matches!(
            self,
            Ty::U8
                | Ty::U16
                | Ty::U32
                | Ty::U64
                | Ty::I8
                | Ty::I16
                | Ty::I32
                | Ty::I64
                | Ty::F32
                | Ty::F64
        )
    }

    /// A scalar's one word (`None` for the others).
    #[must_use]
    pub fn scalar_word(&self) -> Option<ValType> {
        Some(match self {
            Ty::U64 | Ty::I64 => ValType::I64,
            Ty::F32 => ValType::F32,
            Ty::F64 => ValType::F64,
            t if t.is_scalar() => ValType::I32,
            _ => return None,
        })
    }

    /// The two words a parameter of this type crosses as, or `None` when it
    /// cannot be a parameter. The second word of a scalar is unused (0).
    #[must_use]
    pub fn param_words(&self) -> Option<[ValType; 2]> {
        if let Some(w) = self.scalar_word() {
            return Some([w, ValType::I32]);
        }
        match self {
            Ty::Str | Ty::String => Some([ValType::I32, ValType::I32]),
            Ty::Slice(e) | Ty::Vec(e) if e.is_element() => Some([ValType::I32, ValType::I32]),
            // The object's index in the glue's table; the second word is 0.
            Ty::JsObject(_) => Some([ValType::I32, ValType::I32]),
            Ty::Option(inner) => match &**inner {
                t if t.is_scalar() => Some([ValType::I32, t.scalar_word()?]),
                Ty::Str | Ty::String => Some([ValType::I32, ValType::I32]),
                Ty::Slice(e) | Ty::Vec(e) if e.is_element() => Some([ValType::I32, ValType::I32]),
                _ => None,
            },
            _ => None,
        }
    }

    /// The word a return of this type crosses as: `Some(None)` for `()`,
    /// `None` when it cannot be returned. Whether the glue also reads the
    /// return slot is [`Ty::ret_uses_slot`].
    #[must_use]
    pub fn ret_word(&self) -> Option<Option<ValType>> {
        if let Some(w) = self.scalar_word() {
            return Some(Some(w));
        }
        match self {
            Ty::Unit => Some(None),
            Ty::String | Ty::Class(_) => Some(Some(ValType::I32)),
            Ty::Vec(e) if e.is_element() => Some(Some(ValType::I32)),
            Ty::Option(inner) => match &**inner {
                t if t.is_scalar() => Some(t.scalar_word()),
                Ty::String | Ty::Class(_) => Some(Some(ValType::I32)),
                Ty::Vec(e) if e.is_element() => Some(Some(ValType::I32)),
                _ => None,
            },
            Ty::Result(ok, err) if **err == Ty::String && !matches!(**ok, Ty::Result(..)) => {
                ok.ret_word()
            }
            _ => None,
        }
    }

    /// Whether a return of this type leaves a second value in the return slot
    /// (`String`/`Vec` lengths, `Option<scalar>` presence).
    #[must_use]
    pub fn ret_uses_slot(&self) -> bool {
        match self {
            Ty::String | Ty::Vec(_) => true,
            Ty::Option(inner) => inner.is_scalar() || matches!(**inner, Ty::String | Ty::Vec(_)),
            Ty::Result(ok, _) => ok.ret_uses_slot(),
            _ => false,
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Unit => f.write_str("()"),
            Ty::Bool => f.write_str("bool"),
            Ty::U8 => f.write_str("u8"),
            Ty::U16 => f.write_str("u16"),
            Ty::U32 => f.write_str("u32"),
            Ty::U64 => f.write_str("u64"),
            Ty::Usize => f.write_str("usize"),
            Ty::I8 => f.write_str("i8"),
            Ty::I16 => f.write_str("i16"),
            Ty::I32 => f.write_str("i32"),
            Ty::I64 => f.write_str("i64"),
            Ty::Isize => f.write_str("isize"),
            Ty::F32 => f.write_str("f32"),
            Ty::F64 => f.write_str("f64"),
            Ty::Char => f.write_str("char"),
            Ty::Str => f.write_str("&str"),
            Ty::String => f.write_str("String"),
            Ty::Slice(e) => write!(f, "&[{e}]"),
            Ty::Vec(e) => write!(f, "Vec<{e}>"),
            Ty::Option(t) => write!(f, "Option<{t}>"),
            Ty::Result(t, e) => write!(f, "Result<{t}, {e}>"),
            Ty::Class(c) | Ty::JsObject(c) => f.write_str(c),
        }
    }
}

/// One decoded record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// [`kind`].
    pub kind: u8,
    /// The class a member belongs to (empty for a free function); for a
    /// [`kind::CLASS`] record, the class itself.
    pub class: String,
    /// The JS name.
    pub js_name: String,
    /// The wasm export the glue calls (for a class, its free export).
    pub symbol: String,
    /// The Rust doc comment, one line per `///`.
    pub docs: String,
    /// [`receiver`] (always [`receiver::NONE`] for a class).
    pub receiver: u8,
    /// Parameter names and types: each name the Rust binding's, a raw
    /// identifier's without its `r#` (`argN` for `_`).
    pub params: Vec<(String, Ty)>,
    /// The return type ([`Ty::Unit`] for a class).
    pub ret: Ty,
}

/// Why a descriptor section was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// A record does not start with [`MAGIC`].
    Magic {
        /// The record's offset in the section.
        at: usize,
    },
    /// A record has another format version.
    Version {
        /// The record's offset in the section.
        at: usize,
        /// Its version byte.
        found: u8,
    },
    /// The section ends inside a record.
    Truncated {
        /// The record's offset in the section.
        at: usize,
    },
    /// A record is malformed.
    Malformed {
        /// The record's offset in the section.
        at: usize,
        /// What is wrong with it.
        why: String,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Magic { at } => {
                write!(f, "no record magic at byte {at} of the {SECTION} section")
            }
            DecodeError::Version { at, found } => write!(
                f,
                "the record at byte {at} is descriptor version {found}; this reader takes \
                 version {VERSION} only (rebuild the module and the glue from one checkout)"
            ),
            DecodeError::Truncated { at } => {
                write!(
                    f,
                    "the {SECTION} section ends inside the record at byte {at}"
                )
            }
            DecodeError::Malformed { at, why } => {
                write!(f, "the record at byte {at} is malformed: {why}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Every record in a section's bytes, in order. An empty section is no
/// records; the caller decides whether that is an error.
///
/// # Errors
/// [`DecodeError`] for a foreign, truncated or malformed record, or one of
/// another [`VERSION`].
pub fn decode(section: &[u8]) -> Result<Vec<Record>, DecodeError> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < section.len() {
        let rest = &section[at..];
        if rest.len() < HEADER_LEN {
            return Err(DecodeError::Truncated { at });
        }
        if rest[..4] != MAGIC {
            return Err(DecodeError::Magic { at });
        }
        if rest[4] != VERSION {
            return Err(DecodeError::Version { at, found: rest[4] });
        }
        let len = u32::from_le_bytes([rest[5], rest[6], rest[7], rest[8]]) as usize;
        let body = rest
            .get(HEADER_LEN..HEADER_LEN + len)
            .ok_or(DecodeError::Truncated { at })?;
        let mut r = Reader {
            bytes: body,
            pos: 0,
        };
        let record = read_record(&mut r).map_err(|why| DecodeError::Malformed { at, why })?;
        if r.pos != body.len() {
            return Err(DecodeError::Malformed {
                at,
                why: format!("{} trailing bytes", body.len() - r.pos),
            });
        }
        out.push(record);
        at += HEADER_LEN + len;
    }
    Ok(out)
}

/// One type description on its own.
///
/// # Errors
/// A message for a truncated or unknown description, or trailing bytes.
pub fn decode_ty(bytes: &[u8]) -> Result<Ty, String> {
    let mut r = Reader { bytes, pos: 0 };
    let ty = r.ty(0)?;
    if r.pos == bytes.len() {
        Ok(ty)
    } else {
        Err("trailing bytes after the type".to_string())
    }
}

fn read_record(r: &mut Reader<'_>) -> Result<Record, String> {
    let kind = r.u8()?;
    if !(kind::CLASS..=kind::CONSTRUCTOR).contains(&kind) {
        return Err(format!("unknown record kind {kind}"));
    }
    let class = r.str()?;
    let js_name = r.str()?;
    let symbol = r.str()?;
    let docs = r.str()?;
    if kind == kind::CLASS {
        return Ok(Record {
            kind,
            class,
            js_name,
            symbol,
            docs,
            receiver: receiver::NONE,
            params: Vec::new(),
            ret: Ty::Unit,
        });
    }
    let receiver = r.u8()?;
    if receiver > receiver::EXCLUSIVE {
        return Err(format!("unknown receiver {receiver}"));
    }
    let n = r.u8()?;
    let mut params = Vec::with_capacity(usize::from(n));
    for _ in 0..n {
        let name = r.str()?;
        params.push((name, r.ty(0)?));
    }
    let ret = r.ty(0)?;
    Ok(Record {
        kind,
        class,
        js_name,
        symbol,
        docs,
        receiver,
        params,
        ret,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.bytes.get(self.pos).ok_or("ends early")?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.pos.checked_add(n).ok_or("length overflows")?;
        let s = self.bytes.get(self.pos..end).ok_or("ends early")?;
        self.pos = end;
        Ok(s)
    }

    fn str(&mut self) -> Result<String, String> {
        let len = self.take(4)?;
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let s = self.take(len)?;
        String::from_utf8(s.to_vec()).map_err(|_| "a string is not UTF-8".to_string())
    }

    /// A name of up to 255 bytes after its length byte; `what` names it in
    /// an error.
    fn name(&mut self, what: &str) -> Result<String, String> {
        let n = self.u8()?;
        let name = self.take(usize::from(n))?;
        String::from_utf8(name.to_vec()).map_err(|_| format!("{what} is not UTF-8"))
    }

    /// A type nested `depth` deep. A real description is at most
    /// [`TypeDesc::CAP`] bytes, so nesting deeper than that is refused
    /// before it can exhaust the stack.
    fn ty(&mut self, depth: usize) -> Result<Ty, String> {
        if depth >= TypeDesc::CAP {
            return Err(format!("a type nests deeper than {} levels", TypeDesc::CAP));
        }
        let t = self.u8()?;
        let d = depth + 1;
        Ok(match t {
            tag::UNIT => Ty::Unit,
            tag::BOOL => Ty::Bool,
            tag::U8 => Ty::U8,
            tag::U16 => Ty::U16,
            tag::U32 => Ty::U32,
            tag::U64 => Ty::U64,
            tag::USIZE => Ty::Usize,
            tag::I8 => Ty::I8,
            tag::I16 => Ty::I16,
            tag::I32 => Ty::I32,
            tag::I64 => Ty::I64,
            tag::ISIZE => Ty::Isize,
            tag::F32 => Ty::F32,
            tag::F64 => Ty::F64,
            tag::CHAR => Ty::Char,
            tag::STR => Ty::Str,
            tag::STRING => Ty::String,
            tag::SLICE => Ty::Slice(Box::new(self.ty(d)?)),
            tag::VEC => Ty::Vec(Box::new(self.ty(d)?)),
            tag::OPTION => Ty::Option(Box::new(self.ty(d)?)),
            tag::RESULT => {
                let ok = self.ty(d)?;
                Ty::Result(Box::new(ok), Box::new(self.ty(d)?))
            }
            tag::CLASS => Ty::Class(self.name("a class name")?),
            tag::JS_OBJECT => Ty::JsObject(self.name("a JS type name")?),
            other => return Err(format!("unknown type tag {other:#04x}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Vec<u8> {
        let mut v = (text.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(text.as_bytes());
        v
    }

    /// A function record's body, as the macro encodes the leading part.
    fn head(kind: u8, class: &str, js: &str, symbol: &str, docs: &str, recv: u8, n: u8) -> Vec<u8> {
        let mut v = vec![kind];
        for t in [class, js, symbol, docs] {
            v.extend(s(t));
        }
        v.push(recv);
        v.push(n);
        v
    }

    #[test]
    fn composite_descriptions_nest_their_parts() {
        let opt = TypeDesc::wrap(tag::OPTION, TypeDesc::tag(tag::U32));
        assert_eq!(opt.as_bytes(), &[tag::OPTION, tag::U32]);
        let res = TypeDesc::pair(
            tag::RESULT,
            TypeDesc::class("LinkHit"),
            TypeDesc::tag(tag::STRING),
        );
        assert_eq!(
            decode_ty(res.as_bytes()),
            Ok(Ty::Result(
                Box::new(Ty::Class("LinkHit".into())),
                Box::new(Ty::String)
            ))
        );
        assert_eq!(
            decode_ty(TypeDesc::wrap(tag::VEC, TypeDesc::tag(tag::F64)).as_bytes()),
            Ok(Ty::Vec(Box::new(Ty::F64)))
        );
    }

    /// A record assembled at compile time decodes to what went in, and two
    /// concatenated records (what the linker makes of the section) decode in
    /// order.
    #[test]
    fn records_round_trip_through_the_section() {
        // kind, class "Item", js name "Item", symbol "item_free", no docs.
        const PARTS: &[Part] = &[
            Part::Bytes(&[kind::CLASS, 4, 0, 0, 0]),
            Part::Bytes(b"Item"),
            Part::Bytes(&[4, 0, 0, 0]),
            Part::Bytes(b"Item"),
            Part::Bytes(&[9, 0, 0, 0]),
            Part::Bytes(b"item_free"),
            Part::Bytes(&[0, 0, 0, 0]),
        ];
        const CLASS: [u8; record_len(PARTS)] = record(PARTS);
        let mut section = CLASS.to_vec();
        let mut body = head(
            kind::METHOD,
            "Item",
            "len",
            "item_len",
            " Doc.",
            receiver::SHARED,
            1,
        );
        body.extend(s("extra"));
        body.extend(TypeDesc::wrap(tag::OPTION, TypeDesc::tag(tag::U64)).as_bytes());
        body.extend(TypeDesc::tag(tag::USIZE).as_bytes());
        let mut fun = MAGIC.to_vec();
        fun.push(VERSION);
        fun.extend((body.len() as u32).to_le_bytes());
        fun.extend(&body);
        section.extend(&fun);

        let records = decode(&section).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, kind::CLASS);
        assert_eq!(records[0].class, "Item");
        assert_eq!(records[0].symbol, "item_free");
        assert_eq!(
            records[1],
            Record {
                kind: kind::METHOD,
                class: "Item".into(),
                js_name: "len".into(),
                symbol: "item_len".into(),
                docs: " Doc.".into(),
                receiver: receiver::SHARED,
                params: vec![("extra".into(), Ty::Option(Box::new(Ty::U64)))],
                ret: Ty::Usize,
            }
        );
    }

    /// NEGATIVE CONTROLS: what the generator must refuse is refused here.
    #[test]
    fn foreign_old_and_cut_sections_are_refused() {
        let body = head(kind::FUNCTION, "", "f", "f", "", receiver::NONE, 0);
        let mut rec = MAGIC.to_vec();
        rec.push(VERSION);
        rec.extend(((body.len() + 1) as u32).to_le_bytes());
        rec.extend(&body);
        rec.push(tag::UNIT);
        assert!(decode(&rec).is_ok());

        let mut other = rec.clone();
        other[4] = VERSION + 1;
        assert_eq!(
            decode(&other),
            Err(DecodeError::Version {
                at: 0,
                found: VERSION + 1
            })
        );
        let mut foreign = rec.clone();
        foreign[0] = b'x';
        assert_eq!(decode(&foreign), Err(DecodeError::Magic { at: 0 }));
        assert_eq!(
            decode(&rec[..rec.len() - 1]),
            Err(DecodeError::Truncated { at: 0 })
        );
        let mut bad_tag = rec.clone();
        *bad_tag.last_mut().unwrap() = 0x7f;
        assert!(matches!(
            decode(&bad_tag),
            Err(DecodeError::Malformed { .. })
        ));

        // A crafted run of OPTION tags is refused, not a stack overflow.
        let mut deep = vec![tag::OPTION; 1 << 20];
        deep.push(tag::U8);
        assert!(decode_ty(&deep).unwrap_err().contains("nests deeper"));
        let mut real = vec![tag::OPTION; TypeDesc::CAP - 1];
        real.push(tag::U8);
        assert!(decode_ty(&real).is_ok(), "as deep as a description can be");
    }

    #[test]
    fn class_tags_are_never_the_freed_mark() {
        assert_ne!(class_tag("AtermTerminal"), 0);
        assert_ne!(class_tag("AtermTerminal"), class_tag("LinkHit"));
        assert_eq!(class_tag(""), 0x811c_9dc5);
    }

    /// The lowering table the generator checks signatures against.
    #[test]
    fn the_lowering_table_covers_each_shape_once() {
        use ValType::*;
        assert_eq!(Ty::U64.param_words(), Some([I64, I32]));
        assert_eq!(Ty::Str.param_words(), Some([I32, I32]));
        assert_eq!(
            Ty::Option(Box::new(Ty::F64)).param_words(),
            Some([I32, F64])
        );
        assert_eq!(
            Ty::Option(Box::new(Ty::Option(Box::new(Ty::U8)))).param_words(),
            None
        );
        assert_eq!(Ty::Class("C".into()).param_words(), None);
        assert_eq!(
            Ty::JsObject("HTMLCanvasElement".into()).param_words(),
            Some([I32, I32])
        );
        assert_eq!(Ty::JsObject("HTMLCanvasElement".into()).ret_word(), None);
        assert_eq!(
            decode_ty(TypeDesc::js_object("OffscreenCanvas").as_bytes()),
            Ok(Ty::JsObject("OffscreenCanvas".into()))
        );
        assert_eq!(Ty::Unit.ret_word(), Some(None));
        assert_eq!(Ty::Str.ret_word(), None);
        let res = |ok: Ty| Ty::Result(Box::new(ok), Box::new(Ty::String));
        assert_eq!(res(Ty::Unit).ret_word(), Some(None));
        assert_eq!(
            res(Ty::Option(Box::new(Ty::U64))).ret_word(),
            Some(Some(I64))
        );
        assert!(res(Ty::Option(Box::new(Ty::U64))).ret_uses_slot());
        assert_eq!(
            Ty::Result(Box::new(Ty::Unit), Box::new(Ty::U32)).ret_word(),
            None
        );
        assert_eq!(Ty::Vec(Box::new(Ty::Bool)).ret_word(), None);
    }
}
