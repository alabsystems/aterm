// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A whole-buffer DEFLATE (RFC 1951) that spends time for size, for [`crate::gzip`]: zlib
//! level 9's match search (hash chains [`MAX_CHAIN`] long, lazy matching), then each block
//! of up to [`BLOCK_SYMBOLS`] symbols sent as the smallest of a dynamic-Huffman, a
//! fixed-Huffman and a stored block. Its parameters are constants, so the same input is the
//! same stream on every host. The PNG encoder keeps its streaming fixed-Huffman compressor
//! ([`crate::deflate`]), whose memory is bounded by its window rather than its input.

use crate::deflate::{
    BitWriter, DIST_EXTRA, HASH_SIZE, LENGTH_EXTRA, MAX_MATCH, MIN_MATCH, WINDOW, distance_code,
    fixed_litlen, hash3, length_code,
};

/// Candidates tried per position (zlib level 9's `max_chain`)…
const MAX_CHAIN: usize = 4096;
/// …a quarter as many once the match already in hand is this long (`good_length`)…
const GOOD_LENGTH: usize = 32;
/// …and a match this long ends the search (`nice_length`, also `max_lazy`).
const NICE_LENGTH: usize = 258;
/// A three-byte match farther back than this costs more than its literals (zlib's
/// `TOO_FAR`).
const TOO_FAR: usize = 4096;
/// Symbols per block (zlib's `lit_bufsize - 1` at its default `memLevel` 8).
const BLOCK_SYMBOLS: usize = 16_383;
/// The longest literal/length or distance code.
const MAX_BITS: u8 = 15;
/// The longest code-length code.
const MAX_CL_BITS: u8 = 7;
/// The most bytes one stored block holds.
const STORED_MAX: usize = 65_535;
/// The order a dynamic block's code-length code lengths are sent in (RFC 1951 §3.2.7).
const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];
/// The literal/length alphabet a block may use (286 and 287 never appear).
const LITLEN: usize = 286;
/// The distance alphabet a block may use (30 and 31 never appear).
const DISTANCES: usize = 30;
/// The end-of-block symbol.
const END: usize = 256;

/// One LZ77 symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Literal(u8),
    Match { len: u16, dist: u16 },
}

impl Token {
    /// The input bytes the symbol stands for.
    const fn covers(self) -> usize {
        match self {
            Self::Literal(_) => 1,
            Self::Match { len, .. } => len as usize,
        }
    }
}

/// `data` as one complete raw DEFLATE stream (no zlib or gzip framing).
#[must_use]
pub(crate) fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(data.len() / 2 + 64);
    if data.is_empty() {
        w.bits(1, 1);
        w.bits(1, 2);
        let (code, len) = fixed_litlen(END as u16);
        w.huffman(code, len);
        return w.finish();
    }
    let tokens = parse(data);
    let blocks = tokens.len().div_ceil(BLOCK_SYMBOLS);
    let mut start = 0;
    for (k, block) in tokens.chunks(BLOCK_SYMBOLS).enumerate() {
        let covered: usize = block.iter().map(|t| t.covers()).sum();
        write_block(
            &mut w,
            block,
            &data[start..start + covered],
            k + 1 == blocks,
        );
        start += covered;
    }
    debug_assert_eq!(start, data.len(), "the blocks cover the input");
    w.finish()
}

/// `data` as LZ77 symbols: zlib's `deflate_slow`, which holds each match back one position
/// to see whether the next starts a longer one.
fn parse(data: &[u8]) -> Vec<Token> {
    let n = data.len();
    let mut tokens = Vec::with_capacity(n / 3 + 16);
    let mut table = Table {
        head: vec![usize::MAX; HASH_SIZE],
        prev: vec![usize::MAX; WINDOW],
    };
    let mut held_len = MIN_MATCH - 1;
    let mut held_dist = 0;
    let mut pending = false;
    let mut i = 0;
    while i < n {
        let candidate = if i + MIN_MATCH <= n {
            table.insert(data, i)
        } else {
            usize::MAX
        };
        let (mut len, mut dist) = (MIN_MATCH - 1, 0);
        if candidate != usize::MAX && held_len < NICE_LENGTH && i - candidate <= WINDOW {
            let chain = if held_len >= GOOD_LENGTH {
                MAX_CHAIN >> 2
            } else {
                MAX_CHAIN
            };
            if let Some((l, d)) = table.longest(data, i, candidate, chain, held_len)
                && !(l == MIN_MATCH && d > TOO_FAR)
            {
                (len, dist) = (l, d);
            }
        }
        if held_len >= MIN_MATCH && len <= held_len {
            // The held match, from `i - 1`, is at least as long: take it. `i` is in the
            // table already; the rest of its positions join it.
            tokens.push(Token::Match {
                len: held_len as u16,
                dist: held_dist as u16,
            });
            let end = i - 1 + held_len;
            for k in i + 1..end {
                if k + MIN_MATCH <= n {
                    table.insert(data, k);
                }
            }
            i = end;
            pending = false;
            held_len = MIN_MATCH - 1;
        } else {
            if pending {
                tokens.push(Token::Literal(data[i - 1]));
            }
            pending = true;
            (held_len, held_dist) = (len, dist);
            i += 1;
        }
    }
    if pending {
        tokens.push(Token::Literal(data[n - 1]));
    }
    tokens
}

/// The hash chains: the latest position per bucket, and per position (within the window)
/// the one before it in its bucket.
struct Table {
    head: Vec<usize>,
    prev: Vec<usize>,
}

impl Table {
    /// Put position `i` of `data` at the head of its bucket; the bucket's previous head,
    /// the chain to search for `i`.
    fn insert(&mut self, data: &[u8], i: usize) -> usize {
        let h = hash3(data, i);
        let was = self.head[h];
        self.prev[i & (WINDOW - 1)] = was;
        self.head[h] = i;
        was
    }

    /// The longest match in `data` for position `i` longer than `best`, walking at most
    /// `chain` candidates from `candidate`, as `(length, distance)`.
    fn longest(
        &self,
        data: &[u8],
        i: usize,
        mut candidate: usize,
        mut chain: usize,
        best: usize,
    ) -> Option<(usize, usize)> {
        let limit = (data.len() - i).min(MAX_MATCH);
        if best >= limit {
            return None;
        }
        let nice = NICE_LENGTH.min(limit);
        let (mut best_len, mut best_dist, mut found) = (best, 0, false);
        loop {
            let dist = i - candidate;
            if dist == 0 || dist > WINDOW {
                break;
            }
            if data[candidate + best_len] == data[i + best_len] && data[candidate] == data[i] {
                let mut len = 0;
                while len < limit && data[candidate + len] == data[i + len] {
                    len += 1;
                }
                if len > best_len {
                    (best_len, best_dist, found) = (len, dist, true);
                    if len >= nice {
                        break;
                    }
                }
            }
            chain -= 1;
            let next = self.prev[candidate & (WINDOW - 1)];
            // A slot a newer position took leads forward, not back: the chain ends there.
            if chain == 0 || next == usize::MAX || next >= candidate {
                break;
            }
            candidate = next;
        }
        found.then_some((best_len, best_dist))
    }
}

/// Code lengths for symbols used `freq` times, none longer than `limit`: a Huffman code,
/// rebuilt with the counts halved until it fits. Fewer than two symbols used still get a
/// complete two-word code, which every inflater takes.
fn lengths(freq: &[u32], limit: u8) -> Vec<u8> {
    let mut lens = vec![0u8; freq.len()];
    let used: Vec<usize> = (0..freq.len()).filter(|&s| freq[s] > 0).collect();
    if used.len() < 2 {
        let first = used.first().copied().unwrap_or(0);
        lens[first] = 1;
        lens[usize::from(first == 0)] = 1;
        return lens;
    }
    let mut weights: Vec<u64> = freq.iter().map(|&f| u64::from(f)).collect();
    loop {
        let depths = depths(&weights, &used);
        if depths.iter().all(|&(_, d)| d <= usize::from(limit)) {
            for (s, d) in depths {
                lens[s] = d as u8;
            }
            return lens;
        }
        for &s in &used {
            weights[s] = weights[s].div_ceil(2);
        }
    }
}

/// Each used symbol's depth in a Huffman tree over `weights`: the two-queue build over the
/// symbols sorted by weight, then by symbol, so equal weights tie the same way every time.
fn depths(weights: &[u64], used: &[usize]) -> Vec<(usize, usize)> {
    let mut order = used.to_vec();
    order.sort_by_key(|&s| (weights[s], s));
    let leaves = order.len();
    let nodes = 2 * leaves - 1;
    let mut weight: Vec<u64> = order.iter().map(|&s| weights[s]).collect();
    let mut parent = vec![usize::MAX; nodes];
    let (mut leaf, mut inner) = (0, leaves);
    for made in leaves..nodes {
        let mut pick = || {
            let from_leaf = leaf < leaves && (inner >= made || weight[leaf] <= weight[inner]);
            if from_leaf {
                leaf += 1;
                leaf - 1
            } else {
                inner += 1;
                inner - 1
            }
        };
        let (a, b) = (pick(), pick());
        weight.push(weight[a] + weight[b]);
        parent[a] = made;
        parent[b] = made;
    }
    let mut depth = vec![0usize; nodes];
    for node in (0..nodes - 1).rev() {
        depth[node] = depth[parent[node]] + 1;
    }
    order.into_iter().zip(depth).collect()
}

/// The canonical codes for `lens` (RFC 1951 §3.2.2), most-significant bit first.
fn codes(lens: &[u8]) -> Vec<u16> {
    let mut count = [0u32; 16];
    for &l in lens {
        if l > 0 {
            count[usize::from(l)] += 1;
        }
    }
    let mut next = [0u32; 16];
    let mut code = 0u32;
    for bits in 1..16 {
        code = (code + count[bits - 1]) << 1;
        next[bits] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[usize::from(l)];
                next[usize::from(l)] += 1;
                c as u16
            }
        })
        .collect()
}

/// `lens` as code-length symbols `(symbol, extra value, extra bits)`: a run of zeros as 17
/// (3–10) or 18 (11–138), a run of one length as the length then 16s (3–6 repeats each), and
/// what is left as the lengths themselves. Each run starts with its own length, so no 16
/// reaches back past `lens`.
fn rle(lens: &[u8], out: &mut Vec<(u8, u32, u32)>) {
    let mut i = 0;
    while i < lens.len() {
        let cur = lens[i];
        let mut run = 1;
        while i + run < lens.len() && lens[i + run] == cur {
            run += 1;
        }
        i += run;
        let mut left = run;
        if cur == 0 {
            while left >= 11 {
                let k = left.min(138);
                out.push((18, (k - 11) as u32, 7));
                left -= k;
            }
            if left >= 3 {
                out.push((17, (left - 3) as u32, 3));
                left = 0;
            }
        } else {
            out.push((cur, 0, 0));
            left -= 1;
            while left >= 3 {
                let k = left.min(6);
                out.push((16, (k - 3) as u32, 2));
                left -= k;
            }
        }
        out.extend(std::iter::repeat_n((cur, 0, 0), left));
    }
}

/// A block's codes: lengths and codes per literal/length and per distance symbol.
struct Codes {
    lit_lens: Vec<u8>,
    lit_codes: Vec<u16>,
    dist_lens: Vec<u8>,
    dist_codes: Vec<u16>,
}

impl Codes {
    /// DEFLATE's fixed code (RFC 1951 §3.2.6).
    fn fixed() -> Codes {
        let (lit_codes, lit_lens) = (0..LITLEN as u16)
            .map(|s| {
                let (code, len) = fixed_litlen(s);
                (code, len as u8)
            })
            .unzip();
        Codes {
            lit_lens,
            lit_codes,
            dist_lens: vec![5; DISTANCES],
            dist_codes: (0..DISTANCES as u16).collect(),
        }
    }

    /// The bits `lit` and `dist` counts cost under these codes, their extra bits included.
    fn cost(&self, lit: &[u32], dist: &[u32]) -> u64 {
        let mut bits = 0u64;
        for (s, &f) in lit.iter().enumerate() {
            let extra = if s > END {
                LENGTH_EXTRA[s - END - 1]
            } else {
                0
            };
            bits += u64::from(f) * (u64::from(self.lit_lens[s]) + u64::from(extra));
        }
        for (d, &f) in dist.iter().enumerate() {
            bits += u64::from(f) * (u64::from(self.dist_lens[d]) + u64::from(DIST_EXTRA[d]));
        }
        bits
    }

    /// `tokens` and the end of the block, in these codes.
    fn emit(&self, w: &mut BitWriter, tokens: &[Token]) {
        for &t in tokens {
            match t {
                Token::Literal(b) => {
                    let s = usize::from(b);
                    w.huffman(self.lit_codes[s], u32::from(self.lit_lens[s]));
                }
                Token::Match { len, dist } => {
                    let (sym, extra, extra_bits) = length_code(len);
                    let s = usize::from(sym);
                    w.huffman(self.lit_codes[s], u32::from(self.lit_lens[s]));
                    if extra_bits > 0 {
                        w.bits(extra, extra_bits);
                    }
                    let (d, extra, extra_bits) = distance_code(dist);
                    let d = usize::from(d);
                    w.huffman(self.dist_codes[d], u32::from(self.dist_lens[d]));
                    if extra_bits > 0 {
                        w.bits(extra, extra_bits);
                    }
                }
            }
        }
        w.huffman(self.lit_codes[END], u32::from(self.lit_lens[END]));
    }
}

/// One block of `tokens`, which stand for `raw`, as the smallest of a stored, a fixed and a
/// dynamic block (`last` sets BFINAL).
fn write_block(w: &mut BitWriter, tokens: &[Token], raw: &[u8], last: bool) {
    let mut lit = [0u32; LITLEN];
    let mut dist = [0u32; DISTANCES];
    lit[END] = 1;
    for &t in tokens {
        match t {
            Token::Literal(b) => lit[usize::from(b)] += 1,
            Token::Match { len, dist: d } => {
                lit[usize::from(length_code(len).0)] += 1;
                dist[usize::from(distance_code(d).0)] += 1;
            }
        }
    }
    let lit_lens = lengths(&lit, MAX_BITS);
    let dist_lens = lengths(&dist, MAX_BITS);
    let hlit = lit_lens
        .iter()
        .rposition(|&l| l > 0)
        .map_or(0, |p| p + 1)
        .max(257);
    let hdist = dist_lens
        .iter()
        .rposition(|&l| l > 0)
        .map_or(0, |p| p + 1)
        .max(1);
    let mut header = Vec::new();
    rle(&lit_lens[..hlit], &mut header);
    rle(&dist_lens[..hdist], &mut header);
    let mut cl = [0u32; 19];
    for &(s, _, _) in &header {
        cl[usize::from(s)] += 1;
    }
    let cl_lens = lengths(&cl, MAX_CL_BITS);
    let cl_codes = codes(&cl_lens);
    let hclen = CL_ORDER
        .iter()
        .rposition(|&s| cl_lens[s] > 0)
        .map_or(0, |p| p + 1)
        .max(4);
    let dynamic = Codes {
        lit_codes: codes(&lit_lens),
        lit_lens,
        dist_codes: codes(&dist_lens),
        dist_lens,
    };
    let fixed = Codes::fixed();
    let header_bits: u64 = 3
        + 5
        + 5
        + 4
        + 3 * hclen as u64
        + header
            .iter()
            .map(|&(s, _, extra_bits)| u64::from(cl_lens[usize::from(s)]) + u64::from(extra_bits))
            .sum::<u64>();
    let dynamic_bits = header_bits + dynamic.cost(&lit, &dist);
    let fixed_bits = 3 + fixed.cost(&lit, &dist);
    // Each stored block: its 3 header bits, at most 7 bits of padding, LEN and NLEN.
    let stored_bits = raw
        .chunks(STORED_MAX)
        .map(|c| 3 + 7 + 32 + 8 * c.len() as u64)
        .sum::<u64>();
    if stored_bits < dynamic_bits.min(fixed_bits) {
        let pieces = raw.len().div_ceil(STORED_MAX);
        for (k, piece) in raw.chunks(STORED_MAX).enumerate() {
            w.bits(u32::from(last && k + 1 == pieces), 1);
            w.bits(0, 2);
            w.align();
            let len = piece.len() as u16;
            w.bytes(&len.to_le_bytes());
            w.bytes(&(!len).to_le_bytes());
            w.bytes(piece);
        }
    } else if fixed_bits <= dynamic_bits {
        w.bits(u32::from(last), 1);
        w.bits(1, 2);
        fixed.emit(w, tokens);
    } else {
        w.bits(u32::from(last), 1);
        w.bits(2, 2);
        w.bits((hlit - 257) as u32, 5);
        w.bits((hdist - 1) as u32, 5);
        w.bits((hclen - 4) as u32, 4);
        for &s in &CL_ORDER[..hclen] {
            w.bits(u32::from(cl_lens[s]), 3);
        }
        for &(s, extra, extra_bits) in &header {
            let s = usize::from(s);
            w.huffman(cl_codes[s], u32::from(cl_lens[s]));
            if extra_bits > 0 {
                w.bits(extra, extra_bits);
            }
        }
        dynamic.emit(w, tokens);
    }
    debug_assert!(w.pending() < 8);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inflate(stream: &[u8], len: usize) -> Vec<u8> {
        aterm_codec::inflate::inflate(stream, len + 64)
            .unwrap_or_else(|e| panic!("inflate failed on {len} bytes: {e:?}"))
    }

    /// Seeded inputs over every path: empty, one byte, text, every byte value, noise (stored
    /// blocks), long runs, repeats at the window's far edge, and enough symbols for several
    /// blocks.
    fn corpus() -> Vec<(&'static str, Vec<u8>)> {
        let mut state = 0x2545_F491u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let words = [
            "the ",
            "terminal ",
            "aterm ",
            "renders ",
            "a ",
            "glyph ",
            "atlas ",
            "and ",
            "<div ",
            "class=\"row\">",
            "</div>\n",
            "font-size: 14px; ",
            "function ",
            "return ",
            "{ ",
            "} ",
            "0.",
            "1",
            "2",
            "3",
        ];
        let text: String = (0..60_000)
            .map(|_| words[(next() % words.len() as u32) as usize])
            .collect();
        let noise: Vec<u8> = (0..200_000).map(|_| (next() >> 24) as u8).collect();
        let block: Vec<u8> = (0..300).map(|_| (next() >> 24) as u8).collect();
        let mut far = block.clone();
        far.extend((0..WINDOW - block.len()).map(|_| (next() >> 24) as u8));
        far.extend_from_slice(&block);
        far.extend((0..WINDOW - block.len() + 1).map(|_| (next() >> 24) as u8));
        far.extend_from_slice(&block);
        let mut skewed = Vec::new();
        for (i, b) in (0..=255u8).enumerate() {
            skewed.extend(std::iter::repeat_n(b, 1 + (1usize << (i % 18)) / 64));
            skewed.extend((0..(next() % 5)).map(|_| (next() >> 24) as u8));
        }
        vec![
            ("empty", Vec::new()),
            ("one", b"a".to_vec()),
            ("two", b"ab".to_vec()),
            ("short", b"abcabcabcabd".to_vec()),
            ("text", text.into_bytes()),
            ("bytes", (0..=255u8).cycle().take(100_000).collect()),
            ("noise", noise),
            ("run", vec![0xA5; 5 * WINDOW + 13]),
            ("far", far),
            ("skewed", skewed),
        ]
    }

    /// Every input inflates back through aterm's own inflater, which refuses an incomplete or
    /// over-subscribed code, a bad stored length and a distance past the start. NEGATIVE
    /// CONTROL: one flipped bit in a dynamic block's stream is caught or changes the output.
    #[test]
    fn every_input_round_trips() {
        for (label, data) in corpus() {
            let stream = deflate(&data);
            assert_eq!(inflate(&stream, data.len()), data, "{label}");
            assert_eq!(stream, deflate(&data), "{label}: the same bytes every time");
        }
        let noise: Vec<u8> = corpus()[6].1[..2 * STORED_MAX + 7].to_vec();
        let literals: Vec<Token> = noise.iter().map(|&b| Token::Literal(b)).collect();
        let mut w = BitWriter::with_capacity(0);
        write_block(&mut w, &literals, &noise, true);
        let stream = w.finish();
        assert_eq!(
            stream.len(),
            noise.len() + 3 * 5,
            "one block, stored as three pieces"
        );
        assert_eq!(inflate(&stream, noise.len()), noise);
        let text = &corpus()[4].1;
        let mut stream = deflate(text);
        assert_eq!(
            (stream[0] >> 1) & 0b11,
            2,
            "the text's first block is dynamic"
        );
        stream[40] ^= 0x10;
        assert_ne!(
            aterm_codec::inflate::inflate(&stream, text.len() + 64)
                .ok()
                .as_ref(),
            Some(text)
        );
    }

    /// Text compresses well past the fixed-Huffman encoder, and noise costs little more than
    /// its own bytes (it goes stored). NEGATIVE CONTROL: the fixed-Huffman encoder's noise is
    /// larger than the noise.
    #[test]
    fn each_block_is_the_smallest_kind() {
        let corpus = corpus();
        let text = &corpus[4].1;
        let fixed = crate::deflate::zlib_compress(text).len() - 6;
        let squeezed = deflate(text).len();
        assert!(
            squeezed * 100 < fixed * 85,
            "text: {squeezed} against fixed {fixed}"
        );
        let noise = &corpus[6].1;
        let stored = deflate(noise).len();
        assert!(
            stored <= noise.len() + 5 * noise.len().div_ceil(BLOCK_SYMBOLS) + 1,
            "noise: {stored} for {}",
            noise.len()
        );
        assert!(crate::deflate::zlib_compress(noise).len() > noise.len() + 1000);
    }

    /// The codes are length-limited and complete, whatever the counts: a Fibonacci-skewed
    /// alphabet that a plain Huffman tree would make 25 bits deep fits in 15 (and 7), and
    /// one or no symbol used is still a two-word code. NEGATIVE CONTROL: the unlimited tree
    /// over the same counts is deeper than the limit.
    #[test]
    fn codes_are_limited_and_complete() {
        let mut fib = vec![1u32, 1];
        while fib.len() < 26 {
            let n = fib[fib.len() - 1] + fib[fib.len() - 2];
            fib.push(n);
        }
        let used: Vec<usize> = (0..fib.len()).collect();
        let weights: Vec<u64> = fib.iter().map(|&f| u64::from(f)).collect();
        assert!(depths(&weights, &used).iter().any(|&(_, d)| d > 15));
        for limit in [MAX_BITS, MAX_CL_BITS] {
            let lens = lengths(&fib, limit);
            assert!(lens.iter().all(|&l| (1..=limit).contains(&l)), "{lens:?}");
            let kraft: f64 = lens.iter().map(|&l| 0.5f64.powi(i32::from(l))).sum();
            assert!((kraft - 1.0).abs() < 1e-12, "complete: {kraft}");
        }
        assert_eq!(lengths(&[0, 0, 0], 15), [1, 1, 0]);
        assert_eq!(lengths(&[0, 0, 9], 15), [1, 0, 1]);
        assert_eq!(lengths(&[9, 0, 0], 15), [1, 1, 0]);
        assert_eq!(codes(&[2, 1, 3, 3]), [0b10, 0b0, 0b110, 0b111]);
    }

    /// The code-length runs say back the lengths they encode. NEGATIVE CONTROL: a run of two
    /// is never a repeat code.
    #[test]
    fn code_length_runs_decode_to_the_lengths() {
        let lens: Vec<u8> = [vec![0; 140], vec![8; 9], vec![0; 4], vec![5; 2], vec![7; 4]].concat();
        let mut runs = Vec::new();
        rle(&lens, &mut runs);
        let mut back = Vec::new();
        for (s, extra, _) in &runs {
            match s {
                16 => {
                    let last = *back.last().unwrap();
                    back.extend(std::iter::repeat_n(last, 3 + *extra as usize));
                }
                17 => back.extend(std::iter::repeat_n(0, 3 + *extra as usize)),
                18 => back.extend(std::iter::repeat_n(0, 11 + *extra as usize)),
                l => back.push(*l),
            }
        }
        assert_eq!(back, lens);
        assert!(runs.windows(2).any(|w| w == [(5, 0, 0), (5, 0, 0)]));
    }
}
