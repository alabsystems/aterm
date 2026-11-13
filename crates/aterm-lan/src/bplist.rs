// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A reader of Apple's binary property list (`bplist00`), enough for an `NSKeyedArchiver`
//! archive: the objects a [`Plist`] holds are read by index, on demand, and every offset and
//! length is checked against the bytes, so a file that is not a well-formed list reads as an
//! error, never a panic. It reads what [`crate::grant`] needs and nothing else: nulls, booleans,
//! integers, strings (ASCII and UTF-16), UIDs, arrays and dictionaries. Reals, dates, data and
//! sets read as [`Object::Other`].
//!
//! A debug build can also [`Plist::rewrite`] a list: the objects it changes or adds are encoded
//! afresh, and every other object's bytes are copied as the list holds them, so what this reader
//! cannot model (nehelper's data and 16-byte integers) survives unchanged. The lab writes
//! macOS's Local Network answer into a guest with it ([`crate::grant::allow`]); no release build
//! holds a writer.

/// One object of the list, its references still indexes into the object table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Object {
    Null,
    Bool(bool),
    Int(i64),
    String(String),
    /// A `CF$UID`: an index into an archive's `$objects`.
    Uid(u64),
    Array(Vec<usize>),
    /// Each key's and value's object index, in order.
    Dict(Vec<(usize, usize)>),
    Other,
}

/// A parsed list's trailer and offset table, over its bytes.
pub(crate) struct Plist<'a> {
    bytes: &'a [u8],
    offsets: Vec<usize>,
    reference: usize,
    top: usize,
}

/// The largest list read: nehelper's own runs to tens of kilobytes.
pub(crate) const MAX_BYTES: usize = 16 << 20;

fn bad(why: impl std::fmt::Display) -> String {
    format!("not a binary property list: {why}")
}

/// `len` big-endian bytes at `at` as an unsigned number.
fn be(bytes: &[u8], at: usize, len: usize) -> Result<u64, String> {
    if len == 0 || len > 8 {
        return Err(bad(format_args!("a {len}-byte number")));
    }
    let end = at
        .checked_add(len)
        .ok_or_else(|| bad("an offset past the end"))?;
    let field = bytes
        .get(at..end)
        .ok_or_else(|| bad(format_args!("a number at {at} past the end")))?;
    Ok(field.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)))
}

fn index(n: u64) -> Result<usize, String> {
    usize::try_from(n).map_err(|_| bad("an index past this machine's reach"))
}

impl<'a> Plist<'a> {
    /// Read `bytes`' header, trailer and offset table.
    ///
    /// # Errors
    /// Words saying why `bytes` is no binary property list this reader takes.
    pub(crate) fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err(bad(format_args!("{} bytes", bytes.len())));
        }
        if bytes.len() < 8 + 32 || !bytes.starts_with(b"bplist00") {
            return Err(bad("no bplist00 header and trailer"));
        }
        let trailer = bytes.len() - 32;
        let offset_size = usize::from(bytes[trailer + 6]);
        let reference = usize::from(bytes[trailer + 7]);
        let count = index(be(bytes, trailer + 8, 8)?)?;
        let top = index(be(bytes, trailer + 16, 8)?)?;
        let table = index(be(bytes, trailer + 24, 8)?)?;
        if !(1..=8).contains(&offset_size) || !(1..=8).contains(&reference) {
            return Err(bad("an offset or reference size outside 1 to 8"));
        }
        // CF refuses a list whose widths do not reach its own count and table, so this does.
        if !reaches(reference, count as u64) || !reaches(offset_size, table as u64) {
            return Err(bad(
                "a reference or offset size that does not reach the list's end",
            ));
        }
        let table_len = count
            .checked_mul(offset_size)
            .ok_or_else(|| bad("an offset table past the end"))?;
        if table < 8 || table.checked_add(table_len).is_none_or(|end| end > trailer) {
            return Err(bad("an offset table outside the list"));
        }
        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            let at = index(be(bytes, table + i * offset_size, offset_size)?)?;
            if at < 8 || at >= table {
                return Err(bad(format_args!("object {i} at {at}, outside the objects")));
            }
            offsets.push(at);
        }
        if top >= count {
            return Err(bad("a top object past the table"));
        }
        Ok(Self {
            bytes,
            offsets,
            reference,
            top,
        })
    }

    /// The top object's index.
    pub(crate) const fn top(&self) -> usize {
        self.top
    }

    /// A count that follows a marker: the marker's low nibble, or (`0xF`) the integer object
    /// after it. Answers the count and where the object's body starts.
    fn count(&self, at: usize, low: u8) -> Result<(usize, usize), String> {
        if low != 0x0F {
            return Ok((usize::from(low), at + 1));
        }
        let marker = *self
            .bytes
            .get(at + 1)
            .ok_or_else(|| bad("a count past the end"))?;
        if marker >> 4 != 0x1 {
            return Err(bad(format_args!("a count marked {marker:#04x}")));
        }
        let len = 1usize << (marker & 0x0F);
        if len > 8 {
            return Err(bad("a count wider than 8 bytes"));
        }
        let n = index(be(self.bytes, at + 2, len)?)?;
        Ok((n, at + 2 + len))
    }

    /// `n` references from `at`, each checked against the object table.
    fn refs(&self, at: usize, n: usize) -> Result<Vec<usize>, String> {
        if n > self.offsets.len().saturating_mul(2) {
            return Err(bad(format_args!("{n} references, more than objects")));
        }
        (0..n)
            .map(|i| {
                let r = index(be(self.bytes, at + i * self.reference, self.reference)?)?;
                if r >= self.offsets.len() {
                    return Err(bad(format_args!(
                        "a reference to object {r}, past the table"
                    )));
                }
                Ok(r)
            })
            .collect()
    }

    /// Object `i`.
    ///
    /// # Errors
    /// `i` past the table, or the object malformed.
    pub(crate) fn object(&self, i: usize) -> Result<Object, String> {
        let at = *self
            .offsets
            .get(i)
            .ok_or_else(|| bad(format_args!("object {i} past the table")))?;
        let marker = self.bytes[at];
        let low = marker & 0x0F;
        Ok(match marker >> 4 {
            0x0 => match marker {
                0x00 => Object::Null,
                0x08 => Object::Bool(false),
                0x09 => Object::Bool(true),
                _ => Object::Other,
            },
            0x1 => {
                let len = 1usize << low;
                if len > 8 {
                    return Ok(Object::Other);
                }
                let n = be(self.bytes, at + 1, len)?;
                // An 8-byte integer is signed; shorter ones are unsigned.
                Object::Int(if len == 8 {
                    i64::from_be_bytes(n.to_be_bytes())
                } else {
                    i64::try_from(n).map_err(|_| bad("an integer out of range"))?
                })
            }
            0x5 => {
                let (n, body) = self.count(at, low)?;
                let text = self
                    .bytes
                    .get(
                        body..body
                            .checked_add(n)
                            .ok_or_else(|| bad("a string past the end"))?,
                    )
                    .ok_or_else(|| bad("a string past the end"))?;
                if !text.is_ascii() {
                    return Err(bad("an ASCII string that is not ASCII"));
                }
                Object::String(String::from_utf8_lossy(text).into_owned())
            }
            0x6 => {
                let (n, body) = self.count(at, low)?;
                let end = n
                    .checked_mul(2)
                    .and_then(|len| body.checked_add(len))
                    .ok_or_else(|| bad("a string past the end"))?;
                let units: Vec<u16> = self
                    .bytes
                    .get(body..end)
                    .ok_or_else(|| bad("a string past the end"))?
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes(*pair))
                    .collect();
                Object::String(
                    String::from_utf16(&units).map_err(|_| bad("a UTF-16 string that is not"))?,
                )
            }
            0x8 => Object::Uid(be(self.bytes, at + 1, usize::from(low) + 1)?),
            0xA => {
                let (n, body) = self.count(at, low)?;
                Object::Array(self.refs(body, n)?)
            }
            0xD => {
                let (n, body) = self.count(at, low)?;
                let keys = self.refs(body, n)?;
                let values = self.refs(body + n * self.reference, n)?;
                Object::Dict(keys.into_iter().zip(values).collect())
            }
            _ => Object::Other,
        })
    }

    /// Object `i` as a string, if it is one.
    pub(crate) fn string(&self, i: usize) -> Option<String> {
        match self.object(i) {
            Ok(Object::String(text)) => Some(text),
            _ => None,
        }
    }

    fn string_equals(&self, i: usize, want: &str) -> bool {
        let Some(&at) = self.offsets.get(i) else {
            return false;
        };
        let marker = self.bytes[at];
        if marker >> 4 == 0x5 {
            let Ok((n, body)) = self.count(at, marker & 0x0F) else {
                return false;
            };
            let Some(text) = body
                .checked_add(n)
                .and_then(|end| self.bytes.get(body..end))
            else {
                return false;
            };
            return text.is_ascii() && text == want.as_bytes();
        }
        self.string(i).as_deref() == Some(want)
    }

    /// Dictionary `dict`'s value for `key`, as its object index: `None` when `dict` is no
    /// dictionary or has no such key.
    pub(crate) fn value(&self, dict: usize, key: &str) -> Option<usize> {
        let &at = self.offsets.get(dict)?;
        let marker = self.bytes[at];
        if marker >> 4 != 0xD {
            return None;
        }
        let (n, body) = self.count(at, marker & 0x0F).ok()?;
        if n > self.offsets.len().saturating_mul(2) {
            return None;
        }
        let vals_base = body.checked_add(n.checked_mul(self.reference)?)?;
        let mut found = None;
        for idx in 0..n {
            let k =
                index(be(self.bytes, body + idx * self.reference, self.reference).ok()?).ok()?;
            let v = index(be(self.bytes, vals_base + idx * self.reference, self.reference).ok()?)
                .ok()?;
            if k >= self.offsets.len() || v >= self.offsets.len() {
                return None;
            }
            if found.is_none() && self.string_equals(k, key) {
                found = Some(v);
            }
        }
        found
    }
}

/// The rewriter: object spans and encodings ([`Plist::rewrite`]).
#[cfg(any(test, debug_assertions))]
impl Plist<'_> {
    /// How many objects the list holds.
    pub(crate) fn len(&self) -> usize {
        self.offsets.len()
    }

    /// Object `i`'s bytes as the list holds them: its marker, any count, and its body.
    fn span(&self, i: usize) -> Result<&[u8], String> {
        let at = *self
            .offsets
            .get(i)
            .ok_or_else(|| bad(format_args!("object {i} past the table")))?;
        let marker = self.bytes[at];
        let low = marker & 0x0F;
        let body = |n: usize, width: usize| -> Result<usize, String> {
            let (count, start) = self.count(at, low)?;
            count
                .checked_mul(width)
                .and_then(|len| len.checked_mul(n))
                .and_then(|len| start.checked_add(len))
                .ok_or_else(|| bad("an object past the end"))
        };
        let end = match marker >> 4 {
            0x0 => at + 1,
            0x1 | 0x2 => at + 1 + (1usize << low),
            0x3 => at + 1 + 8,
            0x4 | 0x5 => body(1, 1)?,
            0x6 => body(1, 2)?,
            0x8 => at + 1 + usize::from(low) + 1,
            0xA | 0xC => body(1, self.reference)?,
            0xD => body(2, self.reference)?,
            other => return Err(bad(format_args!("object {i} marked {other:#x}"))),
        };
        self.bytes
            .get(at..end)
            .ok_or_else(|| bad(format_args!("object {i} past the end")))
    }

    /// This list with each `replaced` object (by index) encoded afresh in its place and
    /// `appended` after the last, at indexes from [`Plist::len`]; every other object's bytes
    /// are copied, and references keep this list's width, so the top object and every index
    /// stay as they were.
    ///
    /// # Errors
    /// A replaced index past the table, an object this writer cannot encode
    /// ([`Object::Other`]), or more objects than this list's reference width reaches.
    pub(crate) fn rewrite(
        &self,
        replaced: &std::collections::BTreeMap<usize, Object>,
        appended: &[Object],
    ) -> Result<Vec<u8>, String> {
        let old = self.len();
        if let Some(&past) = replaced.keys().find(|&&i| i >= old) {
            return Err(bad(format_args!(
                "a replacement of object {past}, past the table"
            )));
        }
        let total = old + appended.len();
        if !reaches(self.reference, total as u64) {
            return Err(bad(format_args!(
                "{total} objects, past {}-byte references",
                self.reference
            )));
        }
        let mut out = b"bplist00".to_vec();
        let mut offsets = Vec::with_capacity(total);
        for i in 0..total {
            offsets.push(out.len());
            let fresh = if i < old {
                replaced.get(&i)
            } else {
                appended.get(i - old)
            };
            match fresh {
                Some(object) => encode(&mut out, object, self.reference)?,
                None => out.extend_from_slice(self.span(i)?),
            }
        }
        let table = out.len();
        let offset_size = byte_count(table as u64);
        for at in offsets {
            push_be(&mut out, at as u64, offset_size);
        }
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        out.push(u8::try_from(offset_size).map_err(|_| bad("an offset size"))?);
        out.push(u8::try_from(self.reference).map_err(|_| bad("a reference size"))?);
        push_be(&mut out, total as u64, 8);
        push_be(&mut out, self.top as u64, 8);
        push_be(&mut out, table as u64, 8);
        Ok(out)
    }
}

/// Whether `width`-byte numbers are wide enough for a list of `n` objects or a table at `n`:
/// CF's rule, every number below `n` and `n` itself below 2^(8 × width).
const fn reaches(width: usize, n: u64) -> bool {
    width >= 8 || n >> (8 * width) == 0
}

/// The width CF gives a number up to `n`: 1, 2, 4 or 8 bytes (`_byteCount`).
#[cfg(any(test, debug_assertions))]
const fn byte_count(n: u64) -> usize {
    if n >> 8 == 0 {
        1
    } else if n >> 16 == 0 {
        2
    } else if n >> 32 == 0 {
        4
    } else {
        8
    }
}

/// `n`'s low `width` bytes, big-endian.
#[cfg(any(test, debug_assertions))]
fn push_be(out: &mut Vec<u8>, n: u64, width: usize) {
    out.extend_from_slice(&n.to_be_bytes()[8 - width..]);
}

/// A marker with its count: in its low nibble, or `0xF` and an integer object after it.
#[cfg(any(test, debug_assertions))]
fn marker_count(out: &mut Vec<u8>, marker: u8, n: usize) {
    match u8::try_from(n) {
        Ok(small) if small < 15 => out.push(marker | small),
        _ => {
            out.push(marker | 0x0F);
            int(out, n as u64);
        }
    }
}

/// An unsigned integer object, as narrow as it fits (1, 2, 4 or 8 bytes).
#[cfg(any(test, debug_assertions))]
fn int(out: &mut Vec<u8>, n: u64) {
    let (marker, width) = match n {
        0..=0xFF => (0x10, 1),
        0x100..=0xFFFF => (0x11, 2),
        0x1_0000..=0xFFFF_FFFF => (0x12, 4),
        _ => (0x13, 8),
    };
    out.push(marker);
    push_be(out, n, width);
}

/// `object`'s bytes, its references `reference` bytes wide.
#[cfg(any(test, debug_assertions))]
fn encode(out: &mut Vec<u8>, object: &Object, reference: usize) -> Result<(), String> {
    let refs = |out: &mut Vec<u8>, refs: &[usize]| -> Result<(), String> {
        for &r in refs {
            if reference < 8 && (r as u64) >> (8 * reference) != 0 {
                return Err(bad(format_args!(
                    "a reference to object {r}, past {reference} bytes"
                )));
            }
            push_be(out, r as u64, reference);
        }
        Ok(())
    };
    match object {
        Object::Null => out.push(0x00),
        Object::Bool(b) => out.push(if *b { 0x09 } else { 0x08 }),
        Object::Int(n) => match u64::try_from(*n) {
            Ok(n) => int(out, n),
            Err(_) => {
                out.push(0x13);
                out.extend_from_slice(&n.to_be_bytes());
            }
        },
        Object::String(text) if text.is_ascii() => {
            marker_count(out, 0x50, text.len());
            out.extend_from_slice(text.as_bytes());
        }
        Object::String(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            marker_count(out, 0x60, units.len());
            for unit in units {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }
        Object::Uid(n) => {
            let width = byte_count(*n);
            out.push(0x80 | u8::try_from(width - 1).map_err(|_| bad("a UID width"))?);
            push_be(out, *n, width);
        }
        Object::Array(items) => {
            marker_count(out, 0xA0, items.len());
            refs(out, items)?;
        }
        Object::Dict(pairs) => {
            marker_count(out, 0xD0, pairs.len());
            let keys: Vec<usize> = pairs.iter().map(|(k, _)| *k).collect();
            let values: Vec<usize> = pairs.iter().map(|(_, v)| *v).collect();
            refs(out, &keys)?;
            refs(out, &values)?;
        }
        Object::Other => return Err(bad("an object this writer cannot encode")),
    }
    Ok(())
}

/// A test-side writer of the same format, so fixtures say what they hold.
#[cfg(test)]
#[path = "bplist_write_tests.rs"]
pub(crate) mod write;

#[cfg(test)]
#[path = "bplist_tests.rs"]
mod tests;
