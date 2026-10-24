// Copyright (c) 2019 Moritz Borcherding
// SPDX-License-Identifier: MIT
//
// Derived from ruzstd 0.8.3 src/blocks/sequence_section.rs,
// src/decoding/sequence_section_decoder.rs and
// src/decoding/sequence_execution.rs, modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

//! The sequences section, the second half of a compressed block (RFC 8878 §3.1.1.3.2), decoded and
//! executed in one pass: each sequence copies some literals, then a match from earlier output.

use super::bits::BitReaderReversed;
use super::error::DecodeError;
use super::fse::FseTable;
use super::window::{BlockBuf, Window};

pub(crate) const MAX_LITERAL_LENGTH_CODE: u8 = 35;
pub(crate) const MAX_MATCH_LENGTH_CODE: u8 = 52;
const MAX_OFFSET_CODE: u8 = 31;

/// Accuracy limits of transmitted tables (RFC 8878 §3.1.1.3.2.1, `FSE_Compressed_Mode`).
pub(crate) const LL_MAX_LOG: u8 = 9;
pub(crate) const ML_MAX_LOG: u8 = 9;
pub(crate) const OF_MAX_LOG: u8 = 8;

/// The predefined distributions (RFC 8878 §3.1.1.3.2.2.1-3).
pub(crate) const LL_DEFAULT_ACC_LOG: u8 = 6;
pub(crate) const LL_DEFAULT_DISTRIBUTION: [i32; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
pub(crate) const ML_DEFAULT_ACC_LOG: u8 = 6;
pub(crate) const ML_DEFAULT_DISTRIBUTION: [i32; 53] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
pub(crate) const OF_DEFAULT_ACC_LOG: u8 = 5;
pub(crate) const OF_DEFAULT_DISTRIBUTION: [i32; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

/// Literal length codes: (base, extra bits) (RFC 8878 §3.1.1.3.2.1.1).
pub(crate) const LL_CODES: [(u32, u8); 36] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (16, 1),
    (18, 1),
    (20, 1),
    (22, 1),
    (24, 2),
    (28, 2),
    (32, 3),
    (40, 3),
    (48, 4),
    (64, 6),
    (128, 7),
    (256, 8),
    (512, 9),
    (1024, 10),
    (2048, 11),
    (4096, 12),
    (8192, 13),
    (16384, 14),
    (32768, 15),
    (65536, 16),
];

/// Match length codes: (base, extra bits) (RFC 8878 §3.1.1.3.2.1.1).
pub(crate) const ML_CODES: [(u32, u8); 53] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (16, 0),
    (17, 0),
    (18, 0),
    (19, 0),
    (20, 0),
    (21, 0),
    (22, 0),
    (23, 0),
    (24, 0),
    (25, 0),
    (26, 0),
    (27, 0),
    (28, 0),
    (29, 0),
    (30, 0),
    (31, 0),
    (32, 0),
    (33, 0),
    (34, 0),
    (35, 1),
    (37, 1),
    (39, 1),
    (41, 1),
    (43, 2),
    (47, 2),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 5),
    (131, 7),
    (259, 8),
    (515, 9),
    (1027, 10),
    (2051, 11),
    (4099, 12),
    (8195, 13),
    (16387, 14),
    (32771, 15),
    (65539, 16),
];

/// The repeat offsets a frame starts with (RFC 8878 §3.1.2.5).
pub(crate) const INITIAL_OFFSETS: [u32; 3] = [1, 4, 8];

/// A parsed `Sequences_Section_Header`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Header {
    pub(crate) num_sequences: usize,
    /// `Symbol_Compression_Modes`; absent when there are no sequences.
    modes: u8,
}

/// Parses the header at the front of `source`; returns it and its length.
pub(crate) fn parse_header(source: &[u8]) -> Result<(Header, usize), DecodeError> {
    let short = || DecodeError::Corrupt("sequences section header runs past the block");
    let b = |i: usize| source.get(i).copied().ok_or_else(short);
    let b0 = b(0)?;
    let (num_sequences, len) = match b0 {
        0 => {
            return Ok((
                Header {
                    num_sequences: 0,
                    modes: 0,
                },
                1,
            ));
        }
        1..=127 => (usize::from(b0), 1),
        128..=254 => ((usize::from(b0 - 128) << 8) + usize::from(b(1)?), 2),
        255 => (usize::from(b(1)?) + (usize::from(b(2)?) << 8) + 0x7F00, 3),
    };
    if num_sequences == 0 {
        return Ok((
            Header {
                num_sequences: 0,
                modes: 0,
            },
            len,
        ));
    }
    let modes = b(len)?;
    Ok((
        Header {
            num_sequences,
            modes,
        },
        len + 1,
    ))
}

/// Which code table a symbol type's codes index (RFC 8878 §3.1.1.3.2.1.1).
#[derive(Clone, Copy, Debug)]
enum Codes {
    LiteralLength,
    MatchLength,
    Offset,
}

impl Codes {
    /// A code's (base value, extra bits); `None` past the last code.
    fn lookup(self, code: u8) -> Option<(u32, u8)> {
        match self {
            Self::LiteralLength => LL_CODES.get(usize::from(code)).copied(),
            Self::MatchLength => ML_CODES.get(usize::from(code)).copied(),
            Self::Offset => (code <= MAX_OFFSET_CODE).then(|| (1u32 << code, code)),
        }
    }
}

/// One state of a sequence decoding table.
#[derive(Clone, Copy, Debug, Default)]
struct SeqEntry {
    /// The code's base value (an offset code's `1 << code`).
    base_value: u32,
    /// The next state is `next_base` plus `nb_bits` read.
    next_base: u16,
    nb_bits: u8,
    /// Bits read and added to `base_value`.
    extra_bits: u8,
}

/// A decoding table for one symbol type.
#[derive(Clone, Debug, Default)]
struct SeqTable {
    entries: Vec<SeqEntry>,
    accuracy_log: u8,
    /// False until a block sets the table (Repeat mode needs one).
    built: bool,
}

impl SeqTable {
    /// Flattens a built FSE table over `codes`.
    fn set_fse(&mut self, fse: &FseTable, codes: Codes) -> Result<(), DecodeError> {
        self.built = false;
        self.entries.clear();
        for (symbol, nb_bits, base_line) in fse.states() {
            let Some((base_value, extra_bits)) = codes.lookup(symbol) else {
                return Err(DecodeError::Corrupt("sequence code out of range"));
            };
            self.entries.push(SeqEntry {
                base_value,
                next_base: base_line as u16,
                nb_bits,
                extra_bits,
            });
        }
        self.accuracy_log = fse.accuracy_log();
        self.built = true;
        Ok(())
    }

    /// RLE mode: every symbol is `code`, and no state bits are read.
    fn rle(&mut self, code: u8, codes: Codes) -> Result<(), DecodeError> {
        self.built = false;
        let Some((base_value, extra_bits)) = codes.lookup(code) else {
            return Err(DecodeError::Corrupt("RLE sequence code out of range"));
        };
        self.entries.clear();
        self.entries.push(SeqEntry {
            base_value,
            next_base: 0,
            nb_bits: 0,
            extra_bits,
        });
        self.accuracy_log = 0;
        self.built = true;
        Ok(())
    }
}

/// One symbol type: its table, the predefined table (built once), and a
/// scratch FSE table for descriptions read from the stream.
struct Kind {
    table: SeqTable,
    predefined: SeqTable,
    fse: FseTable,
    codes: Codes,
    max_log: u8,
}

impl Kind {
    fn new(codes: Codes, max_symbol: u8, max_log: u8, default_log: u8, default: &[i32]) -> Self {
        let mut fse = FseTable::new(max_symbol);
        let mut predefined = SeqTable::default();
        if fse.build_from_probabilities(default_log, default).is_ok() {
            let _ = predefined.set_fse(&fse, codes);
        }
        Self {
            table: SeqTable::default(),
            predefined,
            fse,
            codes,
            max_log,
        }
    }

    /// Applies `mode`, reading a description or RLE byte from the front of
    /// `source`. Returns the bytes read.
    fn update(&mut self, mode: u8, source: &[u8]) -> Result<usize, DecodeError> {
        match mode {
            0 => {
                if !self.predefined.built {
                    return Err(DecodeError::Corrupt("predefined sequence table missing"));
                }
                self.table.clone_from(&self.predefined);
                Ok(0)
            }
            1 => {
                let Some(&code) = source.first() else {
                    return Err(DecodeError::Corrupt("RLE sequence table lacks its byte"));
                };
                self.table.rle(code, self.codes)?;
                Ok(1)
            }
            2 => {
                self.table.built = false;
                let read = self.fse.build_decoder(source, self.max_log)?;
                self.table.set_fse(&self.fse, self.codes)?;
                Ok(read)
            }
            _ => {
                if self.table.built {
                    Ok(0)
                } else {
                    Err(DecodeError::Corrupt("Repeat mode with no table to repeat"))
                }
            }
        }
    }
}

/// The three tables, kept across a frame's blocks for Repeat mode.
pub(crate) struct Tables {
    ll: Kind,
    of: Kind,
    ml: Kind,
}

impl Tables {
    pub(crate) fn new() -> Self {
        Self {
            ll: Kind::new(
                Codes::LiteralLength,
                MAX_LITERAL_LENGTH_CODE,
                LL_MAX_LOG,
                LL_DEFAULT_ACC_LOG,
                &LL_DEFAULT_DISTRIBUTION,
            ),
            of: Kind::new(
                Codes::Offset,
                MAX_OFFSET_CODE,
                OF_MAX_LOG,
                OF_DEFAULT_ACC_LOG,
                &OF_DEFAULT_DISTRIBUTION,
            ),
            ml: Kind::new(
                Codes::MatchLength,
                MAX_MATCH_LENGTH_CODE,
                ML_MAX_LOG,
                ML_DEFAULT_ACC_LOG,
                &ML_DEFAULT_DISTRIBUTION,
            ),
        }
    }

    /// Forgets every table (a new frame).
    pub(crate) fn reset(&mut self) {
        for kind in [&mut self.ll, &mut self.of, &mut self.ml] {
            kind.table.built = false;
        }
    }

    /// Applies the block's modes, reading any table descriptions from the
    /// front of `source` (in the order literal lengths, offsets, match
    /// lengths). Returns the bytes read.
    fn update(&mut self, modes: u8, source: &[u8]) -> Result<usize, DecodeError> {
        if modes & 3 != 0 {
            return Err(DecodeError::Corrupt("sequences modes set reserved bits"));
        }
        let mut read = 0;
        for (kind, mode) in [
            (&mut self.ll, modes >> 6),
            (&mut self.of, (modes >> 4) & 3),
            (&mut self.ml, (modes >> 2) & 3),
        ] {
            read += kind.update(mode, source.get(read..).unwrap_or_default())?;
        }
        Ok(read)
    }
}

/// What executing sequences writes to and reads from.
pub(crate) struct Exec<'a> {
    /// The block's output so far.
    pub(crate) out: &'a mut BlockBuf,
    pub(crate) history: &'a Window,
    pub(crate) offsets: &'a mut [u32; 3],
}

/// A decoder's current state in one table.
struct State<'t> {
    entries: &'t [SeqEntry],
    entry: SeqEntry,
}

impl<'t> State<'t> {
    /// Reads the first state: `accuracy_log` bits.
    fn init(table: &'t SeqTable, br: &mut BitReaderReversed<'_>) -> Result<Self, DecodeError> {
        if !table.built {
            return Err(DecodeError::Corrupt("Repeat mode with no table to repeat"));
        }
        let mut state = Self {
            entries: &table.entries,
            entry: SeqEntry::default(),
        };
        state.go(br.get_bits(table.accuracy_log));
        Ok(state)
    }

    #[inline(always)]
    fn update(&mut self, br: &mut BitReaderReversed<'_>) {
        let add = br.get_bits(self.entry.nb_bits);
        self.go(u64::from(self.entry.next_base) + add);
    }

    /// A built table holds every state its entries can reach, so the
    /// fallback is never taken; it keeps the lookup panic-free.
    #[inline(always)]
    fn go(&mut self, state: u64) {
        self.entry = usize::try_from(state)
            .ok()
            .and_then(|i| self.entries.get(i))
            .copied()
            .unwrap_or_default();
    }
}

/// Decodes `header.num_sequences` sequences from `source` (table descriptions, then the bitstream)
/// and executes each against the first `lit_count` bytes of `literals` (which runs [`SLACK`] bytes
/// past them); the literals left over follow the last sequence.
///
/// [`SLACK`]: super::window::SLACK
pub(crate) fn execute(
    header: &Header,
    source: &[u8],
    tables: &mut Tables,
    literals: &[u8],
    lit_count: usize,
    exec: &mut Exec<'_>,
) -> Result<(), DecodeError> {
    let table_bytes = tables.update(header.modes, source)?;
    let stream = source.get(table_bytes..).unwrap_or_default();
    let mut br = BitReaderReversed::new(stream);
    br.skip_padding()?;

    let mut ll = State::init(&tables.ll.table, &mut br)?;
    let mut of = State::init(&tables.of.table, &mut br)?;
    let mut ml = State::init(&tables.ml.table, &mut br)?;

    let mut lit_pos: usize = 0;
    for i in 0..header.num_sequences {
        let (lle, mle, ofe) = (ll.entry, ml.entry, of.entry);
        let (of_add, ml_add, ll_add) =
            br.get_bits_triple(ofe.extra_bits, mle.extra_bits, lle.extra_bits);
        let offset_value = ofe.base_value | of_add as u32;
        let lit_len = u64::from(lle.base_value) + ll_add;
        let match_len = u64::from(mle.base_value) + ml_add;

        if i + 1 < header.num_sequences {
            ll.update(&mut br);
            ml.update(&mut br);
            of.update(&mut br);
        }

        let (Ok(lit_len), Ok(match_len)) = (usize::try_from(lit_len), usize::try_from(match_len))
        else {
            return Err(DecodeError::Corrupt("sequence lengths overflow"));
        };
        if lit_len > lit_count - lit_pos {
            return Err(DecodeError::Corrupt(
                "sequence copies more literals than the block has",
            ));
        }
        if !exec.out.has_room(lit_len.saturating_add(match_len)) {
            return Err(DecodeError::Corrupt("block decodes past its maximum size"));
        }
        let offset = resolve_offset(offset_value, lit_len, exec.offsets) as usize;
        if offset == 0 {
            return Err(DecodeError::Corrupt("match offset of zero"));
        }
        if offset > exec.history.max_offset() {
            return Err(DecodeError::Corrupt(
                "match offset exceeds the frame's window",
            ));
        }
        exec.out.copy_literals(literals, lit_pos, lit_len)?;
        lit_pos += lit_len;
        exec.out.copy_match(exec.history, offset, match_len)?;
    }

    if br.bits_remaining() != 0 {
        return Err(DecodeError::Corrupt(
            "sequences bitstream length does not match its sequences",
        ));
    }
    exec.out
        .push(literals.get(lit_pos..lit_count).unwrap_or_default())?;
    Ok(())
}

/// Turns an `Offset_Value` into a match offset, updating the repeat offsets
/// (RFC 8878 §3.1.2.5). Values 1 to 3 name a repeat offset (shifted by one
/// when the literal length is zero); larger values are the offset plus 3. A
/// result of 0 is corrupt input, which the caller refuses.
pub(crate) const fn resolve_offset(offset_value: u32, lit_len: usize, hist: &mut [u32; 3]) -> u32 {
    let [r0, r1, r2] = *hist;
    if offset_value > 3 {
        let offset = offset_value - 3;
        *hist = [offset, r0, r1];
        return offset;
    }
    let index = if lit_len == 0 {
        offset_value
    } else {
        offset_value.saturating_sub(1)
    };
    match index {
        0 => r0,
        1 => {
            *hist = [r1, r0, r2];
            r1
        }
        2 => {
            *hist = [r2, r0, r1];
            r2
        }
        _ => {
            let offset = r0.saturating_sub(1);
            *hist = [offset, r0, r1];
            offset
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{INITIAL_OFFSETS, parse_header, resolve_offset};

    #[test]
    fn sequence_counts_at_each_header_width() {
        assert_eq!(parse_header(&[0]).unwrap().0.num_sequences, 0);
        let (h, len) = parse_header(&[127, 0]).unwrap();
        assert_eq!((h.num_sequences, len), (127, 2));
        let (h, len) = parse_header(&[0x81, 0x02, 0]).unwrap();
        assert_eq!((h.num_sequences, len), (0x102, 3));
        let (h, len) = parse_header(&[128, 0]).unwrap();
        assert_eq!((h.num_sequences, len), (0, 2));
        let (h, len) = parse_header(&[255, 0x34, 0x12, 0]).unwrap();
        assert_eq!((h.num_sequences, len), (0x1234 + 0x7F00, 4));
        assert!(parse_header(&[]).is_err());
        assert!(parse_header(&[5]).is_err());
        assert!(parse_header(&[255, 1]).is_err());
    }

    /// RFC 8878 §3.1.2.5's rules, step by step from the initial offsets.
    #[test]
    fn repeat_offsets() {
        let mut h = INITIAL_OFFSETS;
        assert_eq!(resolve_offset(1, 5, &mut h), 1);
        assert_eq!(h, [1, 4, 8]);
        assert_eq!(resolve_offset(2, 5, &mut h), 4);
        assert_eq!(h, [4, 1, 8]);
        assert_eq!(resolve_offset(3, 5, &mut h), 8);
        assert_eq!(h, [8, 4, 1]);
        assert_eq!(resolve_offset(103, 5, &mut h), 100);
        assert_eq!(h, [100, 8, 4]);
        assert_eq!(resolve_offset(1, 0, &mut h), 8);
        assert_eq!(h, [8, 100, 4]);
        assert_eq!(resolve_offset(2, 0, &mut h), 4);
        assert_eq!(h, [4, 8, 100]);
        assert_eq!(resolve_offset(3, 0, &mut h), 3);
        assert_eq!(h, [3, 4, 8]);
        let mut one = [1, 2, 3];
        assert_eq!(resolve_offset(3, 0, &mut one), 0);
    }
}
