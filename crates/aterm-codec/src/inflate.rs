// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! RFC 1951 (DEFLATE) + RFC 1950 (zlib) decompression on std alone (the gzip reader's CRC-32, [`crate::crc32`], is astream-wire's).

/// Why a DEFLATE/zlib stream could not be decompressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InflateError {
    /// The bit stream ended before the data did.
    Truncated,
    /// The 2-byte zlib header is not `CM=8`/`FCHECK`-valid, or asks for a preset dictionary (unsupported).
    BadZlibHeader,
    /// A DEFLATE block declared the reserved block type `11`.
    BadBlockType,
    /// A stored block's `LEN`/`NLEN` ones-complement check failed.
    BadStoredLength,
    /// A Huffman code did not resolve to any symbol (corrupt table or stream).
    BadSymbol,
    /// A back-reference distance is zero or points before the output start.
    BadDistance,
    /// Decompression would exceed the caller's `max_output` ceiling (anti-bomb).
    OutputTooLarge,
    /// The zlib trailer's Adler-32 did not match the decompressed data.
    BadChecksum,
}

/// Streaming decompression over `std::io::Read`, for output too large to buffer whole.
pub mod stream;

const MAXBITS: usize = 15;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CLCL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// LSB-first bit reader over a byte slice (DEFLATE bit order).
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_buffer: u64,
    bits_in_buffer: u32,
}

impl<'a> BitReader<'a> {
    #[inline]
    const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_buffer: 0,
            bits_in_buffer: 0,
        }
    }

    /// Pull as many whole bytes as fit into the accumulator. The `<= 56` guard
    /// keeps the shift below 64.
    fn refill(&mut self) {
        let mut bits = self.bits_in_buffer;
        let mut buf = self.bit_buffer;
        let mut pos = self.byte_pos;
        while bits <= 56 {
            let Some(&byte) = self.data.get(pos) else {
                break;
            };
            buf |= u64::from(byte).wrapping_shl(bits);
            bits += 8;
            pos = pos.saturating_add(1);
        }
        self.bits_in_buffer = bits;
        self.bit_buffer = buf;
        self.byte_pos = pos;
    }

    #[inline]
    fn read_bit(&mut self) -> Result<u32, InflateError> {
        if self.bits_in_buffer == 0 {
            self.refill();
            if self.bits_in_buffer == 0 {
                return Err(InflateError::Truncated);
            }
        }
        let bit = (self.bit_buffer & 1) as u32;
        self.bit_buffer >>= 1;
        self.bits_in_buffer = self.bits_in_buffer.saturating_sub(1);
        Ok(bit)
    }

    #[inline]
    fn read_bits(&mut self, n: u32) -> Result<u32, InflateError> {
        let n = n.min(32);
        while self.bits_in_buffer < n {
            if self.byte_pos >= self.data.len() {
                return Err(InflateError::Truncated);
            }
            self.refill();
        }
        let mask = (1u64 << n) - 1;
        let v = (self.bit_buffer & mask) as u32;
        self.bit_buffer >>= n;
        self.bits_in_buffer = self.bits_in_buffer.saturating_sub(n);
        Ok(v)
    }

    /// Look at the next `want` bits without consuming them.
    #[inline]
    fn peek_bits(&mut self, want: u32) -> (u32, u32) {
        if self.bits_in_buffer < want {
            self.refill();
        }
        let avail = want.min(self.bits_in_buffer);
        let mask = (1u64 << avail.min(32)) - 1;
        ((self.bit_buffer & mask) as u32, avail)
    }

    /// Drop `n` bits already validated by [`peek_bits`](Self::peek_bits);
    /// clamped to what is buffered.
    #[inline]
    fn consume_bits(&mut self, n: u32) {
        let n = n.min(self.bits_in_buffer);
        self.bit_buffer = self.bit_buffer.wrapping_shr(n);
        self.bits_in_buffer = self.bits_in_buffer.saturating_sub(n);
    }

    /// Discard bits up to the next byte boundary (for stored blocks).
    #[inline]
    const fn align_to_byte(&mut self) {
        let bits = self.bits_in_buffer;
        let drop = bits % 8;
        self.bit_buffer = self.bit_buffer.wrapping_shr(drop);
        self.bits_in_buffer = bits.wrapping_sub(drop);
    }

    /// Read a byte-aligned `u8` (caller must be byte-aligned, i.e. after
    /// `align_to_byte`, so `bits_in_buffer` is a multiple of 8).
    #[inline]
    fn read_aligned_byte(&mut self) -> Result<u8, InflateError> {
        if self.bits_in_buffer >= 8 {
            let b = (self.bit_buffer & 0xff) as u8;
            self.bit_buffer >>= 8;
            self.bits_in_buffer -= 8;
            Ok(b)
        } else {
            let pos = self.byte_pos;
            let b = *self.data.get(pos).ok_or(InflateError::Truncated)?;
            self.byte_pos = pos.saturating_add(1);
            Ok(b)
        }
    }

    /// Read a byte-aligned little-endian `u16`.
    #[inline]
    fn read_aligned_u16(&mut self) -> Result<u16, InflateError> {
        let lo = self.read_aligned_byte()?;
        let hi = self.read_aligned_byte()?;
        Ok(u16::from_le_bytes([lo, hi]))
    }
}

/// Bits consulted by the flat root lookup table. Nine covers the whole fixed
/// literal/length alphabet; longer codes fall back to the bit-at-a-time walk,
/// so the table stays one flat level with no new error paths.
const ROOT_BITS: u32 = 9;
/// Entries in the root table: one per distinct `ROOT_BITS`-bit prefix.
const ROOT_SIZE: usize = 1 << ROOT_BITS;
/// Shift of the code-length field in a packed root entry: low 12 bits are the
/// symbol, high 4 the length (1..=`ROOT_BITS`); 0 means "no short code here".
const ROOT_LEN_SHIFT: u32 = 12;
/// Mask selecting the symbol out of a packed root entry.
const ROOT_SYM_MASK: u16 = (1 << ROOT_LEN_SHIFT) - 1;
/// Symbols a block must decode before its literal/length and distance root
/// tables are built.
const ROOT_BUILD_AFTER: u32 = 1024;

/// Reverse the low `len` bits of `code`.
#[inline]
fn reverse_code(code: u32, len: u32) -> u32 {
    let len = len.min(ROOT_BITS);
    let mut src = code;
    let mut out = 0u32;
    for _ in 0..len {
        out = (out << 1) | (src & 1);
        src >>= 1;
    }
    out
}

/// Build the flat root lookup table from the canonical `count`/`symbol` form.
fn build_root_table(count: &[u16; MAXBITS + 1], symbol: &[u16]) -> Vec<u16> {
    let mut root = vec![0u16; ROOT_SIZE];
    let mut code: u32 = 0;
    let mut index: usize = 0;
    for (len, &cnt) in count.iter().enumerate().skip(1) {
        let cnt = cnt as usize;
        let len_u32 = len as u32;
        if len_u32 <= ROOT_BITS {
            let step = 1usize << len_u32;
            for k in 0..cnt {
                let Some(&sym) = symbol.get(index.saturating_add(k)) else {
                    break;
                };
                let entry = ((len_u32 as u16) << ROOT_LEN_SHIFT) | (sym & ROOT_SYM_MASK);
                let mut slot = reverse_code(code.saturating_add(k as u32), len_u32) as usize;
                while let Some(cell) = root.get_mut(slot) {
                    *cell = entry;
                    slot = slot.saturating_add(step);
                }
            }
        }
        index = index.saturating_add(cnt);
        code = code.saturating_add(cnt as u32).wrapping_shl(1);
    }
    root
}

/// Test-only switch forcing every decode down the bit-at-a-time walk, so a
/// differential test can compare table and walk results.
#[cfg(test)]
mod oracle {
    use std::cell::Cell;

    thread_local! {
        static FORCE_BITWISE: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn force_bitwise() -> bool {
        FORCE_BITWISE.with(Cell::get)
    }

    /// Run `f` with the root table disabled.
    pub(super) fn without_root_table<T>(f: impl FnOnce() -> T) -> T {
        FORCE_BITWISE.with(|c| c.set(true));
        let out = f();
        FORCE_BITWISE.with(|c| c.set(false));
        out
    }
}

/// Canonical Huffman decode table (count/symbol form, RFC 1951 / `puff.c`),
/// plus a flat root lookup table that resolves short codes in one step.
struct Huffman {
    count: [u16; MAXBITS + 1],
    symbol: Vec<u16>,
    /// `ROOT_SIZE` packed `(length, symbol)` entries indexed by the next
    /// `ROOT_BITS` bits, LSB-first.
    root: Vec<u16>,
}

impl Huffman {
    /// Build from per-symbol code lengths (0 = symbol unused).
    fn new(lengths: &[u16]) -> Result<(Self, i32), InflateError> {
        const MAX_SYMBOLS: usize = 288;
        if lengths.len() > MAX_SYMBOLS {
            return Err(InflateError::BadSymbol);
        }
        let mut count = [0u16; MAXBITS + 1];
        for &len in lengths {
            let l = len as usize;
            if l > MAXBITS {
                return Err(InflateError::BadSymbol);
            }
            count[l] = count[l].saturating_add(1);
        }
        let mut left: i32 = 1;
        for &c in count.iter().skip(1) {
            left = (left << 1).saturating_sub(i32::from(c));
            if left < 0 {
                // over-subscribed code
                return Err(InflateError::BadSymbol);
            }
        }
        let mut offs = [0u16; MAXBITS + 2];
        let mut acc = 0u16;
        for (dst, &c) in offs.iter_mut().skip(2).zip(count.iter().skip(1)) {
            acc = acc.saturating_add(c);
            *dst = acc;
        }
        let total = lengths.iter().filter(|&&l| l != 0).count().min(MAX_SYMBOLS);
        let mut symbol = vec![0u16; total];
        let mut next = offs;
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                let l = len as usize;
                if l > MAXBITS {
                    continue;
                }
                let slot = next[l] as usize;
                if slot < symbol.len() {
                    symbol[slot] = sym as u16;
                    next[l] = next[l].saturating_add(1);
                }
            }
        }
        Ok((
            Self {
                count,
                symbol,
                root: Vec::new(),
            },
            left,
        ))
    }

    /// Decode one symbol.
    #[inline]
    fn decode(&self, br: &mut BitReader) -> Result<u16, InflateError> {
        #[cfg(test)]
        if oracle::force_bitwise() {
            return self.decode_bitwise(br);
        }
        let (peeked, avail) = br.peek_bits(ROOT_BITS);
        if let Some(&entry) = self.root.get(peeked as usize) {
            let len = u32::from(entry >> ROOT_LEN_SHIFT);
            if len != 0 && len <= avail {
                br.consume_bits(len);
                return Ok(entry & ROOT_SYM_MASK);
            }
        }
        self.decode_bitwise(br)
    }

    /// Build this alphabet's root table if it does not have one yet. Called
    /// once per block after `ROOT_BUILD_AFTER` symbols; kept out of line.
    #[cold]
    #[inline(never)]
    fn arm_root(&mut self) {
        if self.root.is_empty() {
            self.root = build_root_table(&self.count, &self.symbol);
        }
    }

    /// Decode one symbol one bit at a time (the `puff.c` walk). `#[inline]` so
    /// an alphabet with no root table runs this with no extra call layer.
    #[inline]
    fn decode_bitwise(&self, br: &mut BitReader) -> Result<u16, InflateError> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for &c in self.count.iter().skip(1) {
            code |= (br.read_bit()? & 1) as i32;
            let cnt = i32::from(c);
            let diff = code.saturating_sub(first);
            if diff < cnt {
                let idx = usize::try_from(index.saturating_add(diff)).unwrap_or(usize::MAX);
                return self.symbol.get(idx).copied().ok_or(InflateError::BadSymbol);
            }
            index = index.saturating_add(cnt);
            first = first.saturating_add(cnt);
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::BadSymbol)
    }
}

fn fixed_lit() -> Huffman {
    let mut lengths = [0u16; 288];
    for (sym, len) in lengths.iter_mut().enumerate() {
        *len = match sym {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Huffman::new(&lengths)
        .map(|(h, _left)| h)
        .unwrap_or(Huffman {
            count: [0; MAXBITS + 1],
            symbol: Vec::new(),
            root: Vec::new(),
        })
}

fn fixed_dist() -> Huffman {
    let lengths = [5u16; 30];
    Huffman::new(&lengths)
        .map(|(h, _left)| h)
        .unwrap_or(Huffman {
            count: [0; MAXBITS + 1],
            symbol: Vec::new(),
            root: Vec::new(),
        })
}

fn read_dynamic_tables(br: &mut BitReader) -> Result<(Huffman, Huffman), InflateError> {
    let hlit = br.read_bits(5)? as usize + 257;
    let hdist = br.read_bits(5)? as usize + 1;
    let hclen = br.read_bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 || hclen > 19 {
        return Err(InflateError::BadSymbol);
    }
    let mut cl_lengths = [0u16; 19];
    for &slot in CLCL_ORDER.iter().take(hclen) {
        let v = (br.read_bits(3)? & 0x7) as u16;
        if let Some(e) = cl_lengths.get_mut(slot) {
            *e = v;
        }
    }
    let (cl_huff, cl_left) = Huffman::new(&cl_lengths)?;
    if cl_left != 0 {
        return Err(InflateError::BadSymbol);
    }

    let mut lengths = vec![0u16; (hlit + hdist).min(316)];
    let mut i = 0;
    while i < lengths.len() {
        let sym = cl_huff.decode_bitwise(br)?;
        match sym {
            0..=15 => {
                // opaque `decode` call.
                if let Some(slot) = lengths.get_mut(i) {
                    *slot = sym;
                }
                i = i.saturating_add(1);
            }
            16 => {
                // Repeat the previous length 3..6 times.
                if i == 0 {
                    return Err(InflateError::BadSymbol);
                }
                let Some(&prev) = lengths.get(i.saturating_sub(1)) else {
                    return Err(InflateError::BadSymbol);
                };
                let repeat = 3 + br.read_bits(2)? as usize;
                for _ in 0..repeat {
                    if i >= lengths.len() {
                        return Err(InflateError::BadSymbol);
                    }
                    if let Some(slot) = lengths.get_mut(i) {
                        *slot = prev;
                    }
                    i = i.saturating_add(1);
                }
            }
            17 => {
                let repeat = 3 + br.read_bits(3)? as usize;
                for _ in 0..repeat {
                    if i >= lengths.len() {
                        return Err(InflateError::BadSymbol);
                    }
                    if let Some(slot) = lengths.get_mut(i) {
                        *slot = 0;
                    }
                    i = i.saturating_add(1);
                }
            }
            18 => {
                let repeat = 11 + br.read_bits(7)? as usize;
                for _ in 0..repeat {
                    if i >= lengths.len() {
                        return Err(InflateError::BadSymbol);
                    }
                    if let Some(slot) = lengths.get_mut(i) {
                        *slot = 0;
                    }
                    i = i.saturating_add(1);
                }
            }
            _ => return Err(InflateError::BadSymbol),
        }
    }

    let (lit_lengths, dist_lengths) = lengths.split_at(hlit.min(lengths.len()));
    let (lit, lit_left) = Huffman::new(lit_lengths)?;
    if lit_left != 0 {
        return Err(InflateError::BadSymbol);
    }
    let (dist, dist_left) = Huffman::new(dist_lengths)?;
    let dist_special = dist.symbol.is_empty() || (dist.symbol.len() == 1 && dist.count[1] == 1);
    if dist_left != 0 && !dist_special {
        return Err(InflateError::BadSymbol);
    }
    Ok((lit, dist))
}

fn inflate_stored(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    max_output: usize,
) -> Result<(), InflateError> {
    br.align_to_byte();
    let len16 = br.read_aligned_u16()?;
    let nlen = br.read_aligned_u16()?;
    if len16 != !nlen {
        return Err(InflateError::BadStoredLength);
    }
    let len = len16 as usize;
    if out.len().saturating_add(len) > max_output {
        return Err(InflateError::OutputTooLarge);
    }
    out.reserve(len);
    for _ in 0..len {
        let b = br.read_aligned_byte()?;
        out.push(b);
    }
    Ok(())
}

/// Why a [`block_loop`] returned.
#[derive(PartialEq, Eq)]
enum BlockStop {
    /// The end-of-block symbol was decoded; the block is finished.
    EndOfBlock,
    /// The warm-up budget ran out and the block is still going.
    BudgetSpent,
}

/// Decode one DEFLATE block in two phases.
fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    max_output: usize,
    lit: &mut Huffman,
    dist: &mut Huffman,
) -> Result<(), InflateError> {
    if block_loop::<false>(br, out, max_output, lit, dist, ROOT_BUILD_AFTER)?
        == BlockStop::EndOfBlock
    {
        return Ok(());
    }
    lit.arm_root();
    dist.arm_root();
    block_loop::<true>(br, out, max_output, lit, dist, 0).map(|_| ())
}

/// The literal/match loop. `TABLE` selects the root-table decoder over the
/// walk, so each phase compiles to a loop with only its own decoder.
fn block_loop<const TABLE: bool>(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    max_output: usize,
    lit: &Huffman,
    dist: &Huffman,
    budget: u32,
) -> Result<BlockStop, InflateError> {
    let mut left = budget;
    loop {
        if !TABLE {
            if left == 0 {
                return Ok(BlockStop::BudgetSpent);
            }
            left = left.saturating_sub(1);
        }
        let sym = if TABLE {
            lit.decode(br)?
        } else {
            lit.decode_bitwise(br)?
        };
        if sym < 256 {
            if out.len() >= max_output {
                return Err(InflateError::OutputTooLarge);
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(BlockStop::EndOfBlock);
        } else {
            let li = (sym as usize).saturating_sub(257);
            let (base, extra) = match (LENGTH_BASE.get(li), LENGTH_EXTRA.get(li)) {
                (Some(&b), Some(&e)) => (b, e),
                _ => return Err(InflateError::BadSymbol),
            };
            let length = (base as usize + (br.read_bits(extra)?.min(31)) as usize).min(258);
            let dsym = if TABLE {
                dist.decode(br)? as usize
            } else {
                dist.decode_bitwise(br)? as usize
            };
            let (dbase, dextra) = match (DIST_BASE.get(dsym), DIST_EXTRA.get(dsym)) {
                (Some(&b), Some(&e)) => (b, e),
                _ => return Err(InflateError::BadDistance),
            };
            let distance = dbase as usize + br.read_bits(dextra)? as usize;
            let cur = out.len();
            if distance == 0 || distance > cur {
                return Err(InflateError::BadDistance);
            }
            if cur.saturating_add(length) > max_output {
                return Err(InflateError::OutputTooLarge);
            }
            out.reserve(length);
            let start = cur - distance;
            if distance >= length {
                // match is one memcpy.
                out.extend_from_within(start..start.saturating_add(length));
            } else {
                for i in 0..length {
                    let Some(&b) = out.get(start.saturating_add(i)) else {
                        return Err(InflateError::BadDistance);
                    };
                    out.push(b);
                }
            }
        }
    }
}

/// Decompress a raw DEFLATE (RFC 1951) stream, bounded by `max_output` bytes.
///
/// # Errors
/// Returns an [`InflateError`] for any malformed input or if the decompressed size would exceed `max_output` (decompression-bomb guard).
#[inline]
pub fn inflate(input: &[u8], max_output: usize) -> Result<Vec<u8>, InflateError> {
    inflate_prefix(input, max_output).map(|(out, _)| out)
}

/// Decompress a raw DEFLATE (RFC 1951) stream, bounded by `max_output` bytes, also returning how many input bytes the stream occupied.
///
/// # Errors
/// Returns an [`InflateError`] for any malformed input or if the decompressed size would exceed `max_output` (decompression-bomb guard).
pub fn inflate_prefix(input: &[u8], max_output: usize) -> Result<(Vec<u8>, usize), InflateError> {
    let mut br = BitReader::new(input);
    let mut out: Vec<u8> = Vec::new();
    let mut fixed: Option<(Huffman, Huffman)> = None;
    loop {
        let bfinal = br.read_bit()?;
        let btype = br.read_bits(2)?;
        match btype {
            0 => inflate_stored(&mut br, &mut out, max_output)?,
            1 => {
                let (lit, dist) = &mut *fixed.get_or_insert_with(|| (fixed_lit(), fixed_dist()));
                inflate_block(&mut br, &mut out, max_output, lit, dist)?;
            }
            2 => {
                let (mut lit, mut dist) = read_dynamic_tables(&mut br)?;
                inflate_block(&mut br, &mut out, max_output, &mut lit, &mut dist)?;
            }
            _ => return Err(InflateError::BadBlockType),
        }
        if bfinal == 1 {
            let unread_whole_bytes = (br.bits_in_buffer / 8) as usize;
            let used = br.byte_pos.saturating_sub(unread_whole_bytes);
            return Ok((out, used));
        }
    }
}

/// A running RFC 1950 Adler-32 over a byte stream fed in arbitrary pieces.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Adler32 {
    a: u32,
    b: u32,
}

impl Adler32 {
    /// The specified seed (`a = 1`, `b = 0`).
    #[inline]
    pub(crate) const fn new() -> Self {
        Self { a: 1, b: 0 }
    }

    /// Fold `data` into the running checksum.
    pub(crate) fn update(&mut self, data: &[u8]) {
        const MOD: u32 = 65521;
        const NMAX: usize = 5552;
        let (mut a, mut b) = (self.a, self.b);
        for block in data.chunks(NMAX) {
            for &byte in block {
                a = a.wrapping_add(u32::from(byte));
                b = b.wrapping_add(a);
            }
            a %= MOD;
            b %= MOD;
        }
        self.a = a;
        self.b = b;
    }

    /// The finished checksum.
    #[inline]
    pub(crate) const fn finish(self) -> u32 {
        (self.b << 16) | self.a
    }
}

/// The RFC 1950 Adler-32 checksum of `data`.
#[inline]
fn adler32(data: &[u8]) -> u32 {
    let mut sum = Adler32::new();
    sum.update(data);
    sum.finish()
}

/// RFC 1950's two header bytes: `CM` must be 8 (DEFLATE), `FCHECK` must make
/// `CMF * 256 + FLG` a multiple of 31, and a preset dictionary is unsupported.
#[inline]
fn check_zlib_header(cmf: u8, flg: u8) -> Result<(), InflateError> {
    if (cmf & 0x0f) != 8 {
        // CM must be 8 (DEFLATE)
        return Err(InflateError::BadZlibHeader);
    }
    if ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
        // FCHECK
        return Err(InflateError::BadZlibHeader);
    }
    if (flg & 0x20) != 0 {
        // preset dictionary unsupported
        return Err(InflateError::BadZlibHeader);
    }
    Ok(())
}

/// Decompress a zlib (RFC 1950) stream, bounded by `max_output` bytes, verifying the header and the trailing Adler-32.
///
/// # Errors
/// Returns an [`InflateError`] for a bad header, malformed DEFLATE data, size over `max_output`, or bad Adler-32.
pub fn zlib_decompress(input: &[u8], max_output: usize) -> Result<Vec<u8>, InflateError> {
    // 2-byte header + at least an empty deflate stream + 4-byte Adler-32.
    if input.len() < 6 {
        return Err(InflateError::Truncated);
    }
    let cmf = input[0];
    let flg = input[1];
    if (cmf & 0x0f) != 8 {
        // CM must be 8 (DEFLATE)
        return Err(InflateError::BadZlibHeader);
    }
    if ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
        // FCHECK
        return Err(InflateError::BadZlibHeader);
    }
    if (flg & 0x20) != 0 {
        // preset dictionary unsupported
        return Err(InflateError::BadZlibHeader);
    }
    let deflate = &input[2..input.len() - 4];
    let (out, used) = inflate_prefix(deflate, max_output)?;
    if used != deflate.len() {
        return Err(InflateError::BadChecksum);
    }
    let trailer = &input[input.len() - 4..];
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    if adler32(&out) != expected {
        return Err(InflateError::BadChecksum);
    }
    Ok(out)
}

/// Decompress a zlib (RFC 1950) prefix of `input`, verifying its Adler-32 and returning `(output, consumed)`.
///
/// # Errors
/// Returns an [`InflateError`] for a bad header, malformed DEFLATE data, size over `max_output`, or bad Adler-32.
pub fn zlib_decompress_prefix(
    input: &[u8],
    max_output: usize,
) -> Result<(Vec<u8>, usize), InflateError> {
    // 2-byte header + at least an empty deflate stream + 4-byte Adler-32.
    if input.len() < 6 {
        return Err(InflateError::Truncated);
    }
    let cmf = input[0];
    let flg = input[1];
    if (cmf & 0x0f) != 8 {
        // CM must be 8 (DEFLATE)
        return Err(InflateError::BadZlibHeader);
    }
    if ((u16::from(cmf) << 8) | u16::from(flg)) % 31 != 0 {
        // FCHECK
        return Err(InflateError::BadZlibHeader);
    }
    if (flg & 0x20) != 0 {
        // preset dictionary unsupported
        return Err(InflateError::BadZlibHeader);
    }
    let (out, used) = inflate_prefix(&input[2..], max_output)?;
    let end = used.saturating_add(2);
    let trailer = input
        .get(end..end.saturating_add(4))
        .ok_or(InflateError::Truncated)?;
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    if adler32(&out) != expected {
        return Err(InflateError::BadChecksum);
    }
    Ok((out, end.saturating_add(4)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
    const HELLO: &[u8] = &[
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0x28, 0xcf, 0x2f, 0xca, 0x49, 0x01,
        0x00, 0x1d, 0x54, 0x04, 0x89,
    ];
    const STORED: &[u8] = &[
        0x78, 0x01, 0x01, 0x0a, 0x00, 0xf5, 0xff, 0x41, 0x42, 0x43, 0x44, 0x45, 0x41, 0x42, 0x43,
        0x44, 0x45, 0x0e, 0x5b, 0x02, 0x9f,
    ];
    const BACKREFS: &[u8] = &[
        0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0xa4, 0x2a, 0x04, 0x00, 0xd2, 0x74, 0x1e, 0x79,
    ];
    const DYNAMIC: &[u8] = &[
        0x78, 0xda, 0xed, 0xce, 0x41, 0x12, 0x02, 0x21, 0x0c, 0x44, 0xd1, 0xab, 0xf4, 0x09, 0xbc,
        0x13, 0x42, 0x18, 0xa2, 0x40, 0x30, 0x01, 0x71, 0xe6, 0xf4, 0x52, 0xde, 0xc2, 0x2a, 0x56,
        0xbd, 0xf8, 0x8b, 0x7e, 0x3d, 0x11, 0x5e, 0x83, 0xfd, 0x13, 0x77, 0x95, 0x59, 0x11, 0xe5,
        0x83, 0xc7, 0x28, 0xcd, 0x20, 0x6f, 0x52, 0xf4, 0x95, 0xb3, 0xbb, 0x4e, 0x04, 0x39, 0x30,
        0x13, 0x67, 0x82, 0x43, 0x53, 0x39, 0xd4, 0x95, 0xb2, 0xba, 0x77, 0x4a, 0x71, 0xe4, 0x7c,
        0x62, 0x2a, 0x77, 0xb2, 0x55, 0x03, 0xc5, 0xec, 0x3a, 0xad, 0xf5, 0x52, 0x9a, 0x92, 0x99,
        0x28, 0xb8, 0x42, 0x87, 0x75, 0x4c, 0xee, 0x09, 0x17, 0xa9, 0xac, 0xdc, 0xa8, 0x06, 0xaa,
        0x9e, 0xc9, 0x6e, 0xbf, 0x9f, 0xcd, 0xd8, 0x8c, 0xcd, 0xd8, 0x8c, 0xcd, 0xf8, 0x73, 0xc6,
        0x17, 0xc1, 0xd0, 0x5c, 0x27,
    ];

    /// The prefix form reads the Adler-32 at the stream's end, ignores trailing
    /// bytes, and reports where the stream ended.
    #[test]
    fn prefix_form_tolerates_bytes_after_the_stream() {
        for vector in [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC] {
            let (plain, used) = zlib_decompress_prefix(vector, 1 << 20).unwrap();
            assert_eq!(used, vector.len(), "a bare stream consumes all of itself");
            assert_eq!(plain, zlib_decompress(vector, 1 << 20).unwrap());
            for pad in [1usize, 4, 64, 65, 1024] {
                let mut padded = vector.to_vec();
                padded.extend(std::iter::repeat_n(0xABu8, pad));
                let (got, used) = zlib_decompress_prefix(&padded, 1 << 20)
                    .unwrap_or_else(|e| panic!("{pad} bytes of padding: {e:?}"));
                assert_eq!(got, plain, "{pad} bytes of padding changed the output");
                assert_eq!(used, vector.len(), "{pad} bytes of padding moved the end");
                assert!(
                    zlib_decompress(&padded, 1 << 20).is_err(),
                    "the whole-buffer form is expected to read the padding as the checksum",
                );
            }
        }
    }

    /// The whole-buffer form reads the Adler-32 where the DEFLATE data ends
    /// (RFC 1950 §2.2): stray bytes before the input's last four make it refuse
    /// the stream, as zlib does.
    #[test]
    fn whole_buffer_form_reads_the_checksum_where_the_stream_ends() {
        let stray_before_trailer = [
            0x78, 0x01, 0x03, 0x40, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
        ];
        assert_eq!(
            zlib_decompress(&stray_before_trailer, 1 << 20),
            Err(InflateError::BadChecksum),
        );
        for vector in [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC] {
            let (body, trailer) = vector.split_at(vector.len() - 4);
            for stray in [1usize, 3, 64] {
                let mut spliced = body.to_vec();
                spliced.extend(std::iter::repeat_n(0u8, stray));
                spliced.extend_from_slice(trailer);
                assert_eq!(
                    zlib_decompress(&spliced, 1 << 20),
                    Err(InflateError::BadChecksum),
                    "{stray} stray bytes between a {}-byte stream and its checksum",
                    vector.len(),
                );
            }
        }
    }

    /// A corrupt checksum is still caught in the prefix form, and a stream that
    /// ends before its own trailer is truncated, not silently accepted.
    #[test]
    fn prefix_form_still_checks_the_adler_and_the_length() {
        let mut bad = HELLO.to_vec();
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        assert_eq!(
            zlib_decompress_prefix(&bad, 1 << 20),
            Err(InflateError::BadChecksum),
        );
        let short = &HELLO[..HELLO.len() - 1];
        assert_eq!(
            zlib_decompress_prefix(short, 1 << 20),
            Err(InflateError::Truncated),
        );
    }

    /// Reference vectors: an empty stream, fixed-Huffman literals, a stored
    /// block, fixed-Huffman back-references and a dynamic-Huffman block, plus the
    /// bare DEFLATE body without its zlib wrapper.
    #[test]
    fn reference_vectors_decode() {
        let dynamic = "the quick brown fox jumps over the lazy dog while a programmer \
                       carefully writes a deflate decompressor in rust with zero dependencies. "
            .repeat(12);
        for (name, vector, want) in [
            ("empty", EMPTY, Vec::new()),
            ("fixed literals", HELLO, b"hello, world".to_vec()),
            ("stored", STORED, b"ABCDEABCDE".to_vec()),
            ("fixed backrefs", BACKREFS, "ab".repeat(40).into_bytes()),
            ("dynamic", DYNAMIC, dynamic.into_bytes()),
        ] {
            assert_eq!(
                zlib_decompress(vector, 1 << 20).as_deref(),
                Ok(&want[..]),
                "{name}"
            );
        }
        let raw = &HELLO[2..HELLO.len() - 4];
        assert_eq!(inflate(raw, 1 << 20).unwrap(), b"hello, world");
    }

    #[test]
    fn output_cap_rejects_bomb() {
        assert_eq!(
            zlib_decompress(DYNAMIC, 10),
            Err(InflateError::OutputTooLarge)
        );
    }

    #[test]
    fn bad_zlib_header_rejected() {
        assert_eq!(
            zlib_decompress(&[0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01], 1 << 20),
            Err(InflateError::BadZlibHeader)
        );
    }

    #[test]
    fn incomplete_dynamic_huffman_rejected() {
        const INCOMPLETE: &[u8] = &[
            0x78, 0x01, 0x05, 0xc0, 0x01, 0x09, 0x00, 0x00, 0x00, 0x80, 0xa0, 0x6d, 0xfd, 0x3f,
            0x25, 0x01, 0x00, 0x42, 0x00, 0x42,
        ];
        assert_eq!(adler32(b"A"), 0x0042_0042);
        assert!(
            zlib_decompress(INCOMPLETE, 1 << 20).is_err(),
            "incomplete literal/length code must be rejected, not decoded to Ok(b\"A\")"
        );
        let raw = &INCOMPLETE[2..INCOMPLETE.len() - 4];
        assert_eq!(inflate(raw, 1 << 20), Err(InflateError::BadSymbol));
    }

    #[test]
    fn over_subscribed_huffman_rejected() {
        assert_eq!(
            Huffman::new(&[1, 1, 1]).err(),
            Some(InflateError::BadSymbol)
        );
        let (_h, left) = Huffman::new(&[1, 1]).unwrap();
        assert_eq!(left, 0);
        let (_h, left) = Huffman::new(&[0, 2]).unwrap();
        assert!(left > 0);
    }

    /// The stream from `incomplete_dynamic_huffman_rejected`: a malformed
    /// dynamic block for the differential corpus.
    const INCOMPLETE_STREAM: &[u8] = &[
        0x78, 0x01, 0x05, 0xc0, 0x01, 0x09, 0x00, 0x00, 0x00, 0x80, 0xa0, 0x6d, 0xfd, 0x3f, 0x25,
        0x01, 0x00, 0x42, 0x00, 0x42,
    ];

    /// Every zlib fixture here: stored, fixed and dynamic blocks plus a
    /// rejected table.
    const CORPUS: &[&[u8]] = &[EMPTY, HELLO, STORED, BACKREFS, DYNAMIC, INCOMPLETE_STREAM];

    /// The root table resolves the entire fixed literal/length alphabet (longest
    /// code 9 bits = `ROOT_BITS`), filling every slot.
    #[test]
    fn root_table_resolves_every_fixed_literal_code() {
        let mut lit = fixed_lit();
        lit.arm_root();
        assert_eq!(lit.root.len(), ROOT_SIZE, "the root table must be built");
        assert!(
            lit.root.iter().all(|&e| e != 0),
            "a COMPLETE code whose longest word is ROOT_BITS must leave no empty \
             slot — an empty slot would silently route a fixed-block literal to \
             the fallback, and the table would be accelerating nothing"
        );
        for sym in 0u16..288 {
            let (code, len) = fixed_lit_code(sym);
            let bytes = bits_to_bytes(code, len);
            let mut via_table_reader = BitReader::new(&bytes);
            let mut via_walk_reader = BitReader::new(&bytes);
            let via_table = lit.decode(&mut via_table_reader);
            let via_walk = lit.decode_bitwise(&mut via_walk_reader);
            assert_eq!(via_table, Ok(sym), "table decode wrong for symbol {sym}");
            assert_eq!(via_table, via_walk, "paths disagree on symbol {sym}");
            assert_eq!(
                via_table_reader.bits_in_buffer, via_walk_reader.bits_in_buffer,
                "paths consumed a different number of bits for symbol {sym}"
            );
        }
    }

    /// A code longer than `ROOT_BITS` leaves empty slots and still decodes,
    /// through the fallback, identically.
    #[test]
    fn root_table_falls_back_for_codes_longer_than_root_bits() {
        let mut lengths = [0u16; 14];
        for (i, slot) in lengths.iter_mut().enumerate().take(12).skip(1) {
            *slot = u16::try_from(i).unwrap_or(0);
        }
        lengths[12] = 12;
        lengths[13] = 12;
        let (mut h, left) = Huffman::new(&lengths).expect("staircase code is valid");
        assert_eq!(left, 0, "staircase must be a complete code");
        h.arm_root();
        assert!(
            h.root.contains(&0),
            "a code with words longer than ROOT_BITS must leave empty slots"
        );
        for want in [12usize, 13] {
            let (code, len) = canonical_code_of(&lengths, want);
            assert!(len > ROOT_BITS, "symbol {want} must be a long code");
            let bytes = bits_to_bytes(code, len);
            let mut via_table_reader = BitReader::new(&bytes);
            let mut via_walk_reader = BitReader::new(&bytes);
            let want16 = u16::try_from(want).unwrap_or(0);
            assert_eq!(h.decode(&mut via_table_reader), Ok(want16));
            assert_eq!(h.decode_bitwise(&mut via_walk_reader), Ok(want16));
            assert_eq!(
                via_table_reader.bits_in_buffer,
                via_walk_reader.bits_in_buffer
            );
        }
    }

    /// For every fixture, prefix and single-byte mutation, `inflate` and
    /// `zlib_decompress` return the same `Result` with and without the table.
    #[test]
    fn table_and_bitwise_decoders_agree_on_corpus_prefixes_and_mutations() {
        let mut checked = 0usize;
        let mut ok_seen = 0usize;
        let mut err_kinds = std::collections::BTreeSet::new();

        let mut compare = |input: &[u8], what: &str| {
            let raw: &[u8] = if input.len() > 6 {
                &input[2..input.len() - 4]
            } else {
                input
            };
            let table_z = zlib_decompress(input, 1 << 20);
            let walk_z = oracle::without_root_table(|| zlib_decompress(input, 1 << 20));
            assert_eq!(table_z, walk_z, "zlib_decompress diverged on {what}");
            let table_r = inflate(raw, 1 << 20);
            let walk_r = oracle::without_root_table(|| inflate(raw, 1 << 20));
            assert_eq!(table_r, walk_r, "inflate diverged on {what}");
            match &table_z {
                Ok(_) => ok_seen += 1,
                Err(e) => {
                    err_kinds.insert(format!("{e:?}"));
                }
            }
            if let Err(e) = &table_r {
                err_kinds.insert(format!("{e:?}"));
            }
            checked += 1;
        };

        for (ci, corpus) in CORPUS.iter().enumerate() {
            compare(corpus, &format!("corpus[{ci}] whole"));
            for cut in 0..corpus.len() {
                compare(&corpus[..cut], &format!("corpus[{ci}] prefix {cut}"));
            }
            for pos in 0..corpus.len() {
                for mask in [0x01u8, 0x40, 0x80, 0xff] {
                    let mut m = corpus.to_vec();
                    m[pos] ^= mask;
                    compare(&m, &format!("corpus[{ci}] byte {pos} ^ {mask:#04x}"));
                }
            }
        }

        assert!(
            checked > 1000,
            "the oracle must actually cover a wide space, only {checked} cases"
        );
        assert!(
            ok_seen > 0,
            "no case decoded successfully — corpus is broken"
        );
        for want in ["BadSymbol", "Truncated"] {
            assert!(
                err_kinds.contains(want),
                "the mutation space never produced {want}; it is not exercising \
                 the paths the root table could diverge on. Saw: {err_kinds:?}"
            );
        }
    }

    /// RFC 1951 3.2.6 fixed literal/length code word for `sym`, MSB-first.
    fn fixed_lit_code(sym: u16) -> (u32, u32) {
        let mut lengths = [0u16; 288];
        for (i, len) in lengths.iter_mut().enumerate() {
            *len = match i {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        canonical_code_of(&lengths, sym as usize)
    }

    /// Canonical code word for `want` under `lengths`, MSB-first, computed
    /// independently of the code under test.
    fn canonical_code_of(lengths: &[u16], want: usize) -> (u32, u32) {
        let mut count = [0u32; MAXBITS + 2];
        for &l in lengths {
            if l != 0 {
                count[l as usize] += 1;
            }
        }
        let mut next = [0u32; MAXBITS + 2];
        let mut code = 0u32;
        for len in 1..=MAXBITS {
            code = (code + count[len - 1]) << 1;
            next[len] = code;
        }
        let mut cursor = next;
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let c = cursor[l as usize];
            cursor[l as usize] += 1;
            if sym == want {
                return (c, u32::from(l));
            }
        }
        (0, 0)
    }

    /// Pack an MSB-first code word into DEFLATE's LSB-first byte order, padded
    /// with enough trailing bytes that the reader never hits end-of-input.
    fn bits_to_bytes(code: u32, len: u32) -> Vec<u8> {
        let mut out = vec![0u8; 8];
        for i in 0..len {
            let bit = (code >> (len - 1 - i)) & 1;
            let idx = (i / 8) as usize;
            out[idx] |= u8::try_from(bit).unwrap_or(0) << (i % 8);
        }
        out
    }

    /// One step of a deterministic LCG, so any failure reproduces exactly.
    fn next(state: &mut u64) -> u32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*state >> 33) as u32
    }

    #[test]
    fn fuzz_never_panics_and_respects_cap() {
        const CAP: usize = 4096;
        let seeds: [&[u8]; 5] = [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC];
        let mut state = 0x1234_5678_9abc_def0u64;
        for _ in 0..20_000 {
            let len = (next(&mut state) % 64) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next(&mut state) as u8).collect();
            if next(&mut state) & 1 == 0 {
                let seed = seeds[(next(&mut state) as usize) % seeds.len()];
                let cut = (next(&mut state) as usize) % (seed.len() + 1);
                buf = seed[..cut].to_vec();
                for _ in 0..(next(&mut state) % 4) {
                    if !buf.is_empty() {
                        let idx = (next(&mut state) as usize) % buf.len();
                        buf[idx] ^= (next(&mut state) as u8) | 1;
                    }
                }
            }
            if let Ok(out) = zlib_decompress(&buf, CAP) {
                assert!(out.len() <= CAP, "zlib_decompress exceeded the cap");
            }
            if let Ok(out) = inflate(&buf, CAP) {
                assert!(out.len() <= CAP, "inflate exceeded the cap");
            }
        }
    }

    /// Every strict prefix of a bare DEFLATE stream is `Truncated`: the
    /// buffered bit reader never invents bits past the end of its input.
    #[test]
    fn every_strict_prefix_of_a_raw_stream_is_truncated() {
        for v in [EMPTY, HELLO, STORED, BACKREFS, DYNAMIC] {
            let raw = &v[2..v.len() - 4];
            assert!(inflate(raw, 1 << 20).is_ok());
            for cut in 0..raw.len() {
                assert_eq!(
                    inflate(&raw[..cut], 1 << 20),
                    Err(InflateError::Truncated),
                    "cut {cut} of a {}-byte stream",
                    raw.len()
                );
            }
        }
    }
}
