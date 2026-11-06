// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A reader of Apple's binary property list (`bplist00`), enough for an `NSKeyedArchiver`
//! archive: the objects a [`Plist`] holds are read by index, on demand, and every offset and
//! length is checked against the bytes, so a file that is not a well-formed list reads as an
//! error, never a panic. It reads what [`crate::grant`] needs and nothing else: nulls, booleans,
//! integers, strings (ASCII and UTF-16), UIDs, arrays and dictionaries. Reals, dates, data and
//! sets read as [`Object::Other`].

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

    /// Dictionary `dict`'s value for `key`, as its object index: `None` when `dict` is no
    /// dictionary or has no such key.
    pub(crate) fn value(&self, dict: usize, key: &str) -> Option<usize> {
        let Ok(Object::Dict(pairs)) = self.object(dict) else {
            return None;
        };
        pairs
            .into_iter()
            .find(|(k, _)| self.string(*k).as_deref() == Some(key))
            .map(|(_, v)| v)
    }
}

/// A test-side writer of the same format, so fixtures say what they hold.
#[cfg(test)]
pub(crate) mod write {
    use super::Object;

    /// `objects` as a `bplist00`, the top object `top`; every reference and offset 2 bytes wide.
    pub(crate) fn list(objects: &[Object], top: usize) -> Vec<u8> {
        let mut out = b"bplist00".to_vec();
        let mut offsets = Vec::new();
        let count = |out: &mut Vec<u8>, marker: u8, n: usize| {
            if n < 15 {
                out.push(marker | u8::try_from(n).unwrap());
            } else {
                out.push(marker | 0x0F);
                out.push(0x11);
                out.extend(u16::try_from(n).unwrap().to_be_bytes());
            }
        };
        for object in objects {
            offsets.push(out.len());
            match object {
                Object::Null => out.push(0x00),
                Object::Bool(b) => out.push(if *b { 0x09 } else { 0x08 }),
                Object::Int(n) => {
                    out.push(0x13);
                    out.extend(n.to_be_bytes());
                }
                Object::String(text) if text.is_ascii() => {
                    count(&mut out, 0x50, text.len());
                    out.extend(text.as_bytes());
                }
                Object::String(text) => {
                    let units: Vec<u16> = text.encode_utf16().collect();
                    count(&mut out, 0x60, units.len());
                    for unit in units {
                        out.extend(unit.to_be_bytes());
                    }
                }
                Object::Uid(n) => {
                    out.push(0x81);
                    out.extend(u16::try_from(*n).unwrap().to_be_bytes());
                }
                Object::Array(refs) => {
                    count(&mut out, 0xA0, refs.len());
                    for r in refs {
                        out.extend(u16::try_from(*r).unwrap().to_be_bytes());
                    }
                }
                Object::Dict(pairs) => {
                    count(&mut out, 0xD0, pairs.len());
                    for (k, _) in pairs {
                        out.extend(u16::try_from(*k).unwrap().to_be_bytes());
                    }
                    for (_, v) in pairs {
                        out.extend(u16::try_from(*v).unwrap().to_be_bytes());
                    }
                }
                Object::Other => out.push(0x33),
            }
        }
        let table = out.len();
        for offset in &offsets {
            out.extend(u16::try_from(*offset).unwrap().to_be_bytes());
        }
        out.extend([0, 0, 0, 0, 0, 0, 2, 2]);
        out.extend(u64::try_from(objects.len()).unwrap().to_be_bytes());
        out.extend(u64::try_from(top).unwrap().to_be_bytes());
        out.extend(u64::try_from(table).unwrap().to_be_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::write::list;
    use super::*;

    #[test]
    fn objects_read_back_as_written() {
        let objects = vec![
            Object::Dict(vec![(1, 2), (3, 4)]),
            Object::String("$objects".into()),
            Object::Array(vec![5, 6, 7, 8]),
            Object::String("long key past fifteen bytes".into()),
            Object::Uid(9),
            Object::Null,
            Object::Bool(true),
            Object::Int(-3),
            Object::String("naïve".into()),
            Object::Bool(false),
        ];
        let bytes = list(&objects, 0);
        let plist = Plist::parse(&bytes).unwrap();
        assert_eq!(plist.top(), 0);
        for (i, object) in objects.iter().enumerate() {
            assert_eq!(&plist.object(i).unwrap(), object, "object {i}");
        }
        assert_eq!(plist.value(0, "$objects"), Some(2));
        assert_eq!(plist.value(0, "long key past fifteen bytes"), Some(4));
        assert_eq!(plist.value(0, "absent"), None);
        assert_eq!(plist.value(2, "$objects"), None, "an array has no keys");
    }

    /// Truncations, a bad header and references past the table all read as errors, never a
    /// panic: every prefix of a list, and a list whose reference points past its table.
    #[test]
    fn malformed_lists_are_errors_not_panics() {
        let bytes = list(
            &[
                Object::Dict(vec![(1, 2)]),
                Object::String("k".into()),
                Object::Array(vec![1]),
            ],
            0,
        );
        assert!(Plist::parse(&bytes).is_ok());
        for cut in 0..bytes.len() {
            if let Ok(plist) = Plist::parse(&bytes[..cut]) {
                for i in 0..3 {
                    let _ = plist.object(i);
                }
            }
        }
        assert!(Plist::parse(b"bplist01").is_err());
        let mut wrong = bytes.clone();
        wrong[..8].copy_from_slice(b"xplist00");
        assert!(Plist::parse(&wrong).is_err());
        // The array's one reference (2 bytes after its marker) aimed past the table.
        let array_at = Plist::parse(&bytes).unwrap().offsets[2];
        let mut past = bytes;
        assert_eq!(past[array_at], 0xA1);
        past[array_at + 1..array_at + 3].copy_from_slice(&40u16.to_be_bytes());
        let plist = Plist::parse(&past).unwrap();
        assert!(plist.object(2).is_err());
    }
}
