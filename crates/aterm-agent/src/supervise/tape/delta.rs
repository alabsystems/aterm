// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PER-KEY DELTA (the replay plan's §6.3): a body the loop sees again and
//! again, stored against the last one it saw under the same key.
//!
//! A supervising loop reads the same screen every step — a `text --json
//! tail=40` reply of three or four kilobytes, every twenty seconds, for hours —
//! and between two reads usually nothing changed, or only the rows at the
//! bottom did. Compression inside one frame cannot see that (one screen alone
//! compresses about 2×, measured; twelve in a row 18.8×), and a frame holds
//! only what arrived since the last act or wait. So before the frame's LZ4,
//! each reply door's reply is stored against the previous reply to the SAME
//! request at the same door, and each output against the previous output at
//! the same door:
//! * `Same`: an exact repeat, one byte;
//! * `Splice`: the common prefix and suffix kept, only the middle stored;
//! * `Full`: the body itself.
//!
//! Only a body longer than [`DELTA_MIN`] is stored against another (a short
//! one is cheaper whole). The table remembers the last body of at most
//! [`KEYS`] keys, least recently used out first, and only bodies of at most
//! [`BODY_MAX`]: a bigger one is still stored against the base its key holds,
//! if any (its middle is in the frame, so the base is all it borrows), but is
//! never remembered — its key is forgotten, and the next body under it is
//! whole. Keys are
//! remembered by their xxh64, so a long request costs eight bytes; two keys
//! with one hash share a slot, which costs only compression — the decoder
//! computes the same hash from the same key and finds the same base. The
//! table starts empty with each file, and both sides update it in the same
//! order, which is why a tape decodes from its start.

use super::codec::{CodecError, Input, len64, put_bytes, put_varint};

/// A body at most this long is stored whole.
pub(crate) const DELTA_MIN: usize = 256;

/// The keys the table remembers.
pub(crate) const KEYS: usize = 64;

/// The longest body the table remembers.
pub(crate) const BODY_MAX: usize = 64 * 1024;

/// A splice keeps at least this much of the base, or the body goes whole.
const SPLICE_MIN: usize = 16;

const FULL: u8 = 0;
const SAME: u8 = 1;
const SPLICE: u8 = 2;

/// What a body is stored against: the previous reply to the same request at
/// the same reply door, or the previous output at the same output door.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Key<'a> {
    Exchange { door: &'a str, request: &'a [u8] },
    Emit { door: &'a str },
}

impl Key<'_> {
    /// The key's slot: an xxh64 over its kind and each part length first, so
    /// no two keys' bytes run together.
    fn slot(self) -> u64 {
        let mut bytes = Vec::new();
        match self {
            Key::Exchange { door, request } => {
                bytes.push(0);
                put_bytes(&mut bytes, door.as_bytes());
                put_bytes(&mut bytes, request);
            }
            Key::Emit { door } => {
                bytes.push(1);
                put_bytes(&mut bytes, door.as_bytes());
            }
        }
        aterm_zstd::xxh64::xxh64(0, &bytes)
    }
}

/// The last body under each remembered key, most recently used last.
#[derive(Debug, Default)]
pub(crate) struct Table {
    slots: Vec<(u64, Vec<u8>)>,
}

impl Table {
    /// The base under `slot`, made the most recently used.
    fn base(&mut self, slot: u64) -> Option<&[u8]> {
        let at = self.slots.iter().position(|(s, _)| *s == slot)?;
        let entry = self.slots.remove(at);
        self.slots.push(entry);
        self.slots.last().map(|(_, body)| body.as_slice())
    }

    /// `body` becomes the base under `slot` — or, too long to remember, the
    /// slot is forgotten.
    fn remember(&mut self, slot: u64, body: &[u8]) {
        self.slots.retain(|(s, _)| *s != slot);
        if body.len() > BODY_MAX {
            return;
        }
        self.slots.push((slot, body.to_vec()));
        if self.slots.len() > KEYS {
            self.slots.remove(0);
        }
    }

    /// Append `body`, stored under `key` ([module docs](self)).
    pub(crate) fn encode(&mut self, key: Key<'_>, body: &[u8], out: &mut Vec<u8>) {
        let slot = key.slot();
        let base = if body.len() > DELTA_MIN {
            self.base(slot)
        } else {
            None
        };
        match base {
            Some(base) if base == body => out.push(SAME),
            Some(base) => {
                let prefix = common_prefix(base, body);
                let suffix = common_suffix(&base[prefix..], &body[prefix..]);
                if prefix + suffix >= SPLICE_MIN {
                    out.push(SPLICE);
                    put_varint(out, len64(prefix));
                    put_varint(out, len64(suffix));
                    put_bytes(out, &body[prefix..body.len() - suffix]);
                } else {
                    out.push(FULL);
                    put_bytes(out, body);
                }
            }
            None => {
                out.push(FULL);
                put_bytes(out, body);
            }
        }
        self.remember(slot, body);
    }

    /// The next body of `input`, stored under `key`.
    pub(crate) fn decode(
        &mut self,
        key: Key<'_>,
        input: &mut Input<'_>,
    ) -> Result<Vec<u8>, CodecError> {
        let slot = key.slot();
        let body = match input.byte()? {
            FULL => input.bytes()?.to_vec(),
            SAME => self
                .base(slot)
                .ok_or_else(|| input.error("a repeat of a body the file never stored"))?
                .to_vec(),
            SPLICE => {
                let prefix = input.varint()?;
                let suffix = input.varint()?;
                let middle = input.bytes()?;
                let base = self
                    .base(slot)
                    .ok_or_else(|| input.error("a splice of a body the file never stored"))?;
                let kept = usize::try_from(prefix)
                    .ok()
                    .zip(usize::try_from(suffix).ok())
                    .filter(|(p, s)| p.checked_add(*s).is_some_and(|k| k <= base.len()));
                let Some((prefix, suffix)) = kept else {
                    return Err(input.error("a splice that keeps more than its base holds"));
                };
                let mut body = Vec::with_capacity(prefix + middle.len() + suffix);
                body.extend_from_slice(&base[..prefix]);
                body.extend_from_slice(middle);
                body.extend_from_slice(&base[base.len() - suffix..]);
                body
            }
            _ => return Err(input.error("a body form this build does not know")),
        };
        self.remember(slot, &body);
        Ok(body)
    }
}

/// How many leading bytes `a` and `b` share.
fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// How many trailing bytes `a` and `b` share.
fn common_suffix(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count()
}
