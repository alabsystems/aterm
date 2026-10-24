// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The sequences section, the second half of a compressed block (RFC 8878 §3.1.1.3.2):
//! the sequence count, each symbol type's table mode, and the FSE bitstream.

use super::bits::BitWriter;
use super::fse::{self, Table, highbit};
use crate::decode::sequences::{
    LL_CODES, LL_DEFAULT_ACC_LOG, LL_DEFAULT_DISTRIBUTION, LL_MAX_LOG, ML_CODES,
    ML_DEFAULT_ACC_LOG, ML_DEFAULT_DISTRIBUTION, ML_MAX_LOG, OF_DEFAULT_ACC_LOG,
    OF_DEFAULT_DISTRIBUTION, OF_MAX_LOG, resolve_offset,
};

/// One sequence a match finder found: `lit_len` literals, then `match_len` bytes copied
/// from `offset` bytes back (the distance itself, not yet an `Offset_Value`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seq {
    pub(crate) lit_len: u32,
    pub(crate) match_len: u32,
    pub(crate) offset: u32,
}

/// The smallest match a sequence can carry (match length code 0, RFC 8878
/// §3.1.1.3.2.1.1).
pub(crate) const MIN_MATCH: u32 = 3;

/// `Symbol_Compression_Modes` values (RFC 8878 §3.1.1.3.2.1).
const PREDEFINED: u8 = 0;
const RLE: u8 = 1;
const COMPRESSED: u8 = 2;
const REPEAT: u8 = 3;

/// A sequence as the bitstream carries it: three codes and their extra bits.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Coded {
    ll: u8,
    of: u8,
    ml: u8,
    ll_extra: u32,
    ml_extra: u32,
    of_extra: u32,
}

/// The table a decoder holds for one symbol type, which `Repeat_Mode` reuses.
#[derive(Clone, Debug)]
pub(crate) enum Held {
    /// No block of the frame has set one.
    None,
    Predefined,
    Rle(u8),
    Fse(Box<Custom>),
}

/// A table a block described (`FSE_Compressed_Mode`).
#[derive(Clone, Debug)]
pub(crate) struct Custom {
    norm: Vec<i16>,
    table: Table,
}

/// The three symbol types, in the order the modes byte and the tables name them.
#[derive(Clone, Copy, Debug)]
enum Kind {
    LiteralLength = 0,
    Offset = 1,
    MatchLength = 2,
}

impl Kind {
    const ALL: [Self; 3] = [Self::LiteralLength, Self::Offset, Self::MatchLength];

    /// The largest accuracy log a description may use: 9 for literal and match lengths,
    /// 8 for offsets (RFC 8878 §3.1.1.3.2.1, `FSE_Compressed_Mode`).
    const fn max_log(self) -> u32 {
        (match self {
            Self::LiteralLength => LL_MAX_LOG,
            Self::Offset => OF_MAX_LOG,
            Self::MatchLength => ML_MAX_LOG,
        }) as u32
    }
}

/// The predefined distributions as encoding tables (RFC 8878 §3.1.1.3.2.2.1-3).
#[derive(Debug)]
pub(crate) struct Predefined {
    norm: [Vec<i16>; 3],
    tables: [Table; 3],
}

impl Predefined {
    pub(crate) fn new() -> Self {
        let norm = |d: &[i32]| d.iter().map(|&p| p as i16).collect::<Vec<i16>>();
        let norm = [
            norm(&LL_DEFAULT_DISTRIBUTION),
            norm(&OF_DEFAULT_DISTRIBUTION),
            norm(&ML_DEFAULT_DISTRIBUTION),
        ];
        let tables = [
            Table::new(&norm[0], u32::from(LL_DEFAULT_ACC_LOG)),
            Table::new(&norm[1], u32::from(OF_DEFAULT_ACC_LOG)),
            Table::new(&norm[2], u32::from(ML_DEFAULT_ACC_LOG)),
        ];
        Self { norm, tables }
    }

    fn norm(&self, kind: Kind) -> &[i16] {
        self.norm.get(kind as usize).map_or(&[], Vec::as_slice)
    }

    fn table(&self, kind: Kind) -> Option<&Table> {
        self.tables.get(kind as usize)
    }
}

/// `v`'s code among `codes` for every `v < N`: the last code whose base is at most
/// `v + offset`.
const fn small_codes<const N: usize>(codes: &[(u32, u8)], offset: u32) -> [u8; N] {
    let mut out = [0u8; N];
    let mut v = 0;
    while v < N {
        let mut c = 0;
        while c + 1 < codes.len() && codes[c + 1].0 <= v as u32 + offset {
            c += 1;
        }
        out[v] = c as u8;
        v += 1;
    }
    out
}

const LL_SMALL: [u8; 64] = small_codes::<64>(&LL_CODES, 0);
const ML_SMALL: [u8; 128] = small_codes::<128>(&ML_CODES, MIN_MATCH);

/// The literal length code of `ll` (RFC 8878 §3.1.1.3.2.1.1): codes 25 up each double.
#[inline]
pub(crate) const fn ll_code(ll: u32) -> u8 {
    if ll < 64 {
        LL_SMALL[ll as usize]
    } else {
        (highbit(ll) + 19) as u8
    }
}

/// The match length code of `ml` (RFC 8878 §3.1.1.3.2.1.1): codes 43 up each double.
#[inline]
pub(crate) const fn ml_code(ml: u32) -> u8 {
    let base = ml - MIN_MATCH;
    if base < 128 {
        ML_SMALL[base as usize]
    } else {
        (highbit(base) + 36) as u8
    }
}

/// The `Offset_Value` that names `distance` after `lit_len` literals, given the repeat
/// offsets (RFC 8878 §3.1.1.5): 1 to 3 for a repeat offset (shifted by one when no literal
/// precedes the match), else the distance plus 3.
#[inline]
const fn offset_value(distance: u32, lit_len: u32, reps: &[u32; 3]) -> u32 {
    let [r0, r1, r2] = *reps;
    if lit_len > 0 {
        if distance == r0 {
            1
        } else if distance == r1 {
            2
        } else if distance == r2 {
            3
        } else {
            distance + 3
        }
    } else if distance == r1 {
        1
    } else if distance == r2 {
        2
    } else if r0 > 1 && distance == r0 - 1 {
        3
    } else {
        distance + 3
    }
}

/// The bitstream coder of one symbol type: a table's states, or nothing (RLE mode reads
/// no state bits).
#[derive(Clone, Copy)]
enum Coder<'a> {
    Rle,
    Fse(&'a Table),
}

impl Coder<'_> {
    #[inline]
    fn init(self, symbol: u8) -> u32 {
        match self {
            Self::Rle => 0,
            Self::Fse(t) => t.init(symbol),
        }
    }

    #[inline]
    fn encode(self, w: &mut BitWriter<'_>, state: &mut u32, symbol: u8) {
        if let Self::Fse(t) = self {
            t.encode(w, state, symbol);
        }
    }

    #[inline]
    fn flush(self, w: &mut BitWriter<'_>, state: u32) {
        if let Self::Fse(t) = self {
            t.flush(w, state);
        }
    }
}

/// Writes the sequences section of `seqs` to `out` (RFC 8878 §3.1.1.3.2), naming each
/// distance through the repeat offsets `reps` and choosing for each symbol type the mode
/// that costs least given the table the decoder holds (`held`, in literal length, offset,
/// match length order). `reps` and `held` end as a decoder's would.
pub(crate) fn write(
    seqs: &[Seq],
    reps: &mut [u32; 3],
    held: &mut [Held; 3],
    predefined: &Predefined,
    coded: &mut Vec<Coded>,
    out: &mut Vec<u8>,
) {
    let n = seqs.len();
    if n < 128 {
        out.push(n as u8);
    } else if n < 0x7F00 {
        out.push(((n >> 8) + 128) as u8);
        out.push(n as u8);
    } else {
        out.push(255);
        out.extend_from_slice(&((n - 0x7F00) as u16).to_le_bytes());
    }
    if n == 0 {
        return;
    }

    coded.clear();
    let mut counts = [[0u32; 53]; 3];
    for s in seqs {
        let ov = offset_value(s.offset, s.lit_len, reps);
        let resolved = resolve_offset(ov, s.lit_len as usize, reps);
        debug_assert_eq!(resolved, s.offset, "a decoder resolves the same distance");
        let ll = ll_code(s.lit_len);
        let ml = ml_code(s.match_len);
        let of = highbit(ov) as u8;
        let c = Coded {
            ll,
            of,
            ml,
            ll_extra: s.lit_len - LL_CODES.get(usize::from(ll)).map_or(0, |c| c.0),
            ml_extra: s.match_len - ML_CODES.get(usize::from(ml)).map_or(0, |c| c.0),
            of_extra: ov - (1 << of),
        };
        for (kind, code) in [(0, c.ll), (1, c.of), (2, c.ml)] {
            if let Some(slot) = counts
                .get_mut(kind)
                .and_then(|k| k.get_mut(usize::from(code)))
            {
                *slot += 1;
            }
        }
        coded.push(c);
    }

    let modes_at = out.len();
    out.push(0);
    let mut modes = 0u8;
    for kind in Kind::ALL {
        let k = kind as usize;
        let (Some(counts), Some(slot)) = (counts.get(k), held.get_mut(k)) else {
            continue;
        };
        let (mode, next) = choose(kind, counts, n, slot, predefined, out);
        modes |= mode << (6 - 2 * k);
        if let Some(next) = next {
            *slot = next;
        }
    }
    if let Some(m) = out.get_mut(modes_at) {
        *m = modes;
    }

    let coder = |kind: Kind| match held.get(kind as usize) {
        Some(Held::Fse(c)) => Coder::Fse(&c.table),
        Some(Held::Predefined) => predefined.table(kind).map_or(Coder::Rle, Coder::Fse),
        _ => Coder::Rle,
    };
    let (ll, of, ml) = (
        coder(Kind::LiteralLength),
        coder(Kind::Offset),
        coder(Kind::MatchLength),
    );
    let mut w = BitWriter::new(out);
    let Some((last, rest)) = coded.split_last() else {
        return;
    };
    let ll_bits = |c: &Coded| u32::from(LL_CODES.get(usize::from(c.ll)).map_or(0, |c| c.1));
    let ml_bits = |c: &Coded| u32::from(ML_CODES.get(usize::from(c.ml)).map_or(0, |c| c.1));
    // Written back to front, so a decoder reads the states first, then each sequence's
    // offset, match length and literal length bits, then the next states' bits in
    // literal length, match length, offset order (RFC 8878 §3.1.1.3.2.1.2).
    let mut ml_state = ml.init(last.ml);
    let mut of_state = of.init(last.of);
    let mut ll_state = ll.init(last.ll);
    w.add(u64::from(last.ll_extra), ll_bits(last));
    w.add(u64::from(last.ml_extra), ml_bits(last));
    w.add(u64::from(last.of_extra), u32::from(last.of));
    for c in rest.iter().rev() {
        of.encode(&mut w, &mut of_state, c.of);
        ml.encode(&mut w, &mut ml_state, c.ml);
        ll.encode(&mut w, &mut ll_state, c.ll);
        w.add(u64::from(c.ll_extra), ll_bits(c));
        w.add(u64::from(c.ml_extra), ml_bits(c));
        w.add(u64::from(c.of_extra), u32::from(c.of));
    }
    ml.flush(&mut w, ml_state);
    of.flush(&mut w, of_state);
    ll.flush(&mut w, ll_state);
    w.close();
}

/// The cheapest mode for one symbol type's `counts` over `n` sequences, writing its
/// table (an RLE byte or a description) to `out`; the table the decoder then holds, when
/// it changes.
fn choose(
    kind: Kind,
    counts: &[u32],
    n: usize,
    held: &Held,
    predefined: &Predefined,
    out: &mut Vec<u8>,
) -> (u8, Option<Held>) {
    let present = counts.iter().filter(|&&c| c > 0).count();
    let max_code = counts.iter().rposition(|&c| c > 0).unwrap_or(0);
    let used = counts.get(..=max_code).unwrap_or_default();
    let default_log = predefined.table(kind).map_or(0, Table::log);

    let repeat = match held {
        Held::None => None,
        Held::Predefined => fse::cost_bits(used, predefined.norm(kind), default_log),
        Held::Rle(code) => (present == 1 && usize::from(*code) == max_code).then_some(0.0),
        Held::Fse(c) => fse::cost_bits(used, &c.norm, c.table.log()),
    };
    let basic = fse::cost_bits(used, predefined.norm(kind), default_log);
    let rle = (present == 1).then_some(8.0);
    let custom = (present > 1).then(|| {
        let log = fse::optimal_log(kind.max_log(), n, max_code as u32);
        let mut norm = Vec::new();
        fse::normalize(used, log, &mut norm);
        let mut desc = Vec::new();
        fse::write_description(&norm, log, &mut desc);
        let bits = fse::cost_bits(used, &norm, log).unwrap_or(f64::INFINITY);
        (desc.len() as f64 * 8.0 + bits, norm, log, desc)
    });

    let mut best = (f64::INFINITY, REPEAT);
    for (cost, mode) in [
        (repeat, REPEAT),
        (basic, PREDEFINED),
        (rle, RLE),
        (custom.as_ref().map(|c| c.0), COMPRESSED),
    ] {
        if let Some(cost) = cost
            && cost < best.0
        {
            best = (cost, mode);
        }
    }
    match best.1 {
        PREDEFINED => (PREDEFINED, Some(Held::Predefined)),
        RLE => {
            out.push(max_code as u8);
            (RLE, Some(Held::Rle(max_code as u8)))
        }
        COMPRESSED => {
            let Some((_, norm, log, desc)) = custom else {
                return (PREDEFINED, Some(Held::Predefined));
            };
            out.extend_from_slice(&desc);
            let table = Table::new(&norm, log);
            (
                COMPRESSED,
                Some(Held::Fse(Box::new(Custom { norm, table }))),
            )
        }
        _ => (REPEAT, None),
    }
}

#[cfg(test)]
mod tests {
    use super::{LL_CODES, MIN_MATCH, ML_CODES, ll_code, ml_code, offset_value};
    use crate::decode::sequences::resolve_offset;

    /// The shortcut code functions agree with the code tables at every length a block can
    /// hold, and each value sits inside its code's range.
    #[test]
    fn length_codes_cover_every_value() {
        // A block holds 128 KiB: a sequence carries fewer literals than that (it ends in a
        // match), and a match up to the whole block.
        for v in 0..(1u32 << 17) - MIN_MATCH {
            let c = ll_code(v);
            let (base, bits) = LL_CODES[usize::from(c)];
            assert!(
                base <= v && v - base < 1 << bits,
                "literal length {v} as code {c}"
            );
            let ml = v + MIN_MATCH;
            let c = ml_code(ml);
            let (base, bits) = ML_CODES[usize::from(c)];
            assert!(
                base <= ml && ml - base < 1 << bits,
                "match length {ml} as code {c}"
            );
        }
    }

    /// Every distance, after literals or not, resolves through a decoder's repeat-offset
    /// rules to itself, the history moving as the decoder's does.
    #[test]
    fn offset_values_resolve_to_their_distance() {
        let mut reps: [u32; 3] = [1, 4, 8];
        let mut x = 99u64;
        for i in 0..20_000u32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let distance = match x % 5 {
                0 => reps[0],
                1 => reps[1],
                2 => reps[2],
                3 => reps[0].saturating_sub(1).max(1),
                _ => 1 + (x >> 8) as u32 % 100_000,
            };
            let lit_len = (i % 3) * ((x >> 40) as u32 % 4);
            let ov = offset_value(distance, lit_len, &reps);
            assert_eq!(resolve_offset(ov, lit_len as usize, &mut reps), distance);
        }
    }

    /// The negative control: an offset value resolved with the literal length it was not
    /// made for names another distance.
    #[test]
    fn a_repeat_code_under_the_other_literal_rule_differs() {
        let reps = [10, 20, 30];
        let ov = offset_value(20, 0, &reps);
        assert_eq!(ov, 1);
        assert_ne!(resolve_offset(ov, 5, &mut reps.clone()), 20);
    }
}
