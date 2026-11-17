// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict bounded binary codec for the pure boot-cost document.

use super::*;

const MAGIC: &[u8; 8] = b"ATBOOT\0\x01";

/// Encode a valid bounded document. No native state or persistence status is included.
///
/// # Errors
/// Rejects invalid fields, inconsistent target references, or a document over the byte cap.
pub fn encode(book: &Book) -> Result<Vec<u8>, Error> {
    validate_book(book)?;
    let mut w = Writer(Vec::new());
    w.bytes(MAGIC)?;
    w.u8(u8::try_from(book.boots.len()).map_err(|_| Error::Bounds)?)?;
    for b in &book.boots {
        w.string(&b.boot_id, MAX_BOOT_ID_BYTES)?;
        w.u64(b.first_seen_ns)?;
        w.u16(b.coverage.0)?;
        w.u8(u8::try_from(b.observations.len()).map_err(|_| Error::Bounds)?)?;
        for target in b.targets {
            w.u8(target.unwrap_or(u8::MAX))?;
        }
        for o in &b.observations {
            w.u64(o.at_ns)?;
            w.optional_u64(o.before_ns)?;
            w.optional_u64(o.after_ns)?;
            w.u8(u8::from(o.process.is_some()))?;
            if let Some(p) = o.process {
                w.u32(p.pid)?;
                w.u64(p.start)?;
                w.u8(u8::from(p.version.is_some()))?;
                if let Some(v) = p.version {
                    w.bytes(&v.to_le_bytes())?;
                }
            }
            w.u64(o.counter_epoch)?;
            w.raw(o.process_cpu)?;
            w.optional_u64(o.coalition_id)?;
            w.raw(o.coalition_cpu)?;
            w.u64(o.historical_reported.own_ns)?;
            w.u64(o.historical_reported.tabs_ns)?;
            w.u8(u8::from(o.historical_reported.incomplete))?;
            w.u8(u8::from(o.wakes.is_some()))?;
            if let Some(wakes) = &o.wakes {
                w.u64(wakes.epoch)?;
                w.u16(u16::try_from(wakes.variants.len()).map_err(|_| Error::Bounds)?)?;
                for variant in &wakes.variants {
                    w.string(&variant.kind, MAX_WAKE_KIND_BYTES)?;
                    w.u64(variant.count)?;
                }
            }
            w.u16(o.coverage.0)?;
        }
    }
    Ok(w.0)
}

/// Decode one complete document. Counts and string lengths are checked before allocation;
/// malformed, contradictory and trailing data is rejected without restoring a partial book.
///
/// # Errors
/// Rejects unsupported versions, truncated or trailing bytes, invalid tags, UTF-8 or fields,
/// inconsistent target references, and documents over the fixed bounds.
pub fn decode(bytes: &[u8]) -> Result<Book, Error> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::TooLarge);
    }
    let mut r = Reader { bytes, at: 0 };
    if r.bytes(MAGIC.len())? != MAGIC {
        return Err(Error::Format);
    }
    let count = usize::from(r.u8()?);
    if count > MAX_BOOTS {
        return Err(Error::Bounds);
    }
    let mut book = Book {
        boots: Vec::with_capacity(count),
    };
    for _ in 0..count {
        let boot_id = r.string(MAX_BOOT_ID_BYTES)?;
        let first_seen_ns = r.u64()?;
        let coverage = Coverage(r.u16()?);
        let count = usize::from(r.u8()?);
        if count > 2 {
            return Err(Error::Bounds);
        }
        let mut targets = [None; 2];
        for slot in &mut targets {
            *slot = match r.u8()? {
                u8::MAX => None,
                index @ 0..=1 => Some(index),
                _ => return Err(Error::Invalid("target index")),
            };
        }
        let mut observations = Vec::with_capacity(count);
        for _ in 0..count {
            let at_ns = r.u64()?;
            let before_ns = r.optional_u64()?;
            let after_ns = r.optional_u64()?;
            let process = if r.boolean()? {
                let pid = r.u32()?;
                let start = r.u64()?;
                let version = if r.boolean()? {
                    Some(i32::from_le_bytes(r.array()?))
                } else {
                    None
                };
                Some(ProcessKey {
                    pid,
                    start,
                    version,
                })
            } else {
                None
            };
            let counter_epoch = r.u64()?;
            let process_cpu = r.raw()?;
            let coalition_id = r.optional_u64()?;
            let coalition_cpu = r.raw()?;
            let historical_reported = HistoricalReported {
                own_ns: r.u64()?,
                tabs_ns: r.u64()?,
                incomplete: r.boolean()?,
            };
            let wakes = if r.boolean()? {
                let epoch = r.u64()?;
                let count = usize::from(r.u16()?);
                if count > MAX_VARIANTS {
                    return Err(Error::Bounds);
                }
                let mut variants = Vec::with_capacity(count);
                for _ in 0..count {
                    variants.push(WakeCount {
                        kind: r.string(MAX_WAKE_KIND_BYTES)?,
                        count: r.u64()?,
                    });
                }
                Some(WakeCounts { epoch, variants })
            } else {
                None
            };
            let coverage = Coverage(r.u16()?);
            observations.push(Observation {
                at_ns,
                before_ns,
                after_ns,
                process,
                counter_epoch,
                process_cpu,
                coalition_id,
                coalition_cpu,
                historical_reported,
                wakes,
                coverage,
            });
        }
        book.boots.push(Boot {
            boot_id,
            first_seen_ns,
            observations,
            targets,
            coverage,
        });
    }
    if r.at != bytes.len() {
        return Err(Error::Trailing);
    }
    validate_book(&book)?;
    Ok(book)
}

struct Writer(Vec<u8>);
impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > MAX_BYTES)
        {
            return Err(Error::TooLarge);
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn u8(&mut self, n: u8) -> Result<(), Error> {
        self.bytes(&[n])
    }
    fn u16(&mut self, n: u16) -> Result<(), Error> {
        self.bytes(&n.to_le_bytes())
    }
    fn u32(&mut self, n: u32) -> Result<(), Error> {
        self.bytes(&n.to_le_bytes())
    }
    fn u64(&mut self, n: u64) -> Result<(), Error> {
        self.bytes(&n.to_le_bytes())
    }
    fn optional_u64(&mut self, n: Option<u64>) -> Result<(), Error> {
        self.u8(u8::from(n.is_some()))?;
        if let Some(n) = n {
            self.u64(n)?;
        }
        Ok(())
    }
    fn raw(&mut self, n: RawCounters) -> Result<(), Error> {
        self.optional_u64(n.own_ns)?;
        self.optional_u64(n.inherited_ns)
    }
    fn string(&mut self, s: &str, max: usize) -> Result<(), Error> {
        if !valid_label(s, max) {
            return Err(Error::Bounds);
        }
        self.u16(u16::try_from(s.len()).map_err(|_| Error::Bounds)?)?;
        self.bytes(s.as_bytes())
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Bounds)?;
        let slice = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(slice)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        self.bytes(N)?.try_into().map_err(|_| Error::Truncated)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn boolean(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Invalid("tag")),
        }
    }
    fn optional_u64(&mut self) -> Result<Option<u64>, Error> {
        if self.boolean()? {
            Ok(Some(self.u64()?))
        } else {
            Ok(None)
        }
    }
    fn raw(&mut self) -> Result<RawCounters, Error> {
        Ok(RawCounters {
            own_ns: self.optional_u64()?,
            inherited_ns: self.optional_u64()?,
        })
    }
    fn string(&mut self, max: usize) -> Result<String, Error> {
        let n = usize::from(self.u16()?);
        if n == 0 || n > max {
            return Err(Error::Bounds);
        }
        let s = std::str::from_utf8(self.bytes(n)?).map_err(|_| Error::Utf8)?;
        if !valid_label(s, max) {
            return Err(Error::Invalid("label"));
        }
        Ok(s.to_owned())
    }
}
