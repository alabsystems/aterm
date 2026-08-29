// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Byte scanning and substring search — the four `memchr` entry points this
//! crate ever used, first-party.
//!
//! `memchr` cost 15,799 lines across the whole shipped graph for
//! [`memchr`](memchr()), [`memchr2`], `memmem::find` and `memmem::rfind`. This
//! module is those four — the last two behind [`Searcher`] — in a few hundred
//! lines, with no `unsafe` and no CPU feature detection.
//!
//! # How it is fast without SIMD
//!
//! Single-byte scanning is SWAR: the haystack is read a `usize` at a time and
//! the classic `(x - 0x0101..) & !x & 0x8080..` test finds a word CONTAINING a
//! zero byte in a couple of instructions. A word that tests positive is then
//! scanned byte by byte to find which one — the test's lowest set bit is
//! trustworthy but its higher bits are not (a `0x01` byte immediately after a
//! zero byte borrows into the same position), and re-scanning on the rare hit
//! costs nothing while removing the trap entirely.
//!
//! The loops step a BLOCK of eight words at a time and OR the eight tests
//! together, so the machine has eight independent loads in flight and pays one
//! branch and one bounds check per 64 bytes rather than per 8. That is worth
//! roughly 3x over the same test written a word at a time — measured, at every
//! size from a terminal line to a megabyte, in `benches/literal_scan.rs` and
//! the scanner table in this module's tests. It does not reach a vector
//! implementation (`memchr` stays about 2-3x ahead on this machine) and is not
//! meant to: it is eight bytes per compare against sixteen or thirty-two, with
//! no dispatch, no warm-up and no `unsafe`.
//!
//! # Why substring search is Two-Way and not "find the first byte, then verify"
//!
//! The needle is a query the user typed and the haystack is program output.
//! Prefilter-and-verify is quadratic on inputs as ordinary as a screen of `a`
//! searched for `aaaa…a`, and "the user did it to themselves" is not a reason
//! to ship a cliff. Two-Way (Crochemore-Perrin) is `O(n + m)` in the worst case
//! with constant extra space, so there is no cliff to reason about.
//!
//! # …and why it still carries a byte skip
//!
//! Two-Way alone walks the haystack one byte at a time whenever the shift is
//! one, which is the common case: `find` was measured 40-60x behind `memchr`
//! on a 4 KiB line for exactly that reason. So the scanning loop keeps a SKIP:
//! the needle's RAREST byte (by the static frequency estimate in
//! [`commonness`]) is scanned for with the word-at-a-time scanner above, and
//! every position it steps over is a position where that byte of the needle
//! cannot line up, so no occurrence is skipped. This is a pure shift
//! improvement — it never changes which position matches, only how quickly the
//! non-matching ones are dismissed.
//!
//! The skip is ADAPTIVE, which is what preserves the worst case: a needle
//! whose rarest byte is common in this particular haystack (`aaaa` in a screen
//! of `a`) makes the scan return almost immediately every time, so after a few
//! calls that pay for themselves in under [`MIN_STRIDE`] bytes each the skip
//! switches off for the rest of the haystack and the search is plain Two-Way,
//! bounded by `O(n + m)` again.
//!
//! # Reverse search is the same algorithm, read backwards
//!
//! [`Searcher::rfind_in`] runs the SAME Two-Way loop over a [`View`] that maps logical
//! index `i` to physical index `len - 1 - i`, on both the haystack and the
//! needle, and maps the answer back at the end. A reverse search is therefore
//! one reverse pass that stops at the first (rightmost) occurrence — not a
//! forward walk of every occurrence keeping the last, which is what this
//! module did until it was measured at 320x behind `memchr` driving the search
//! UI's find-prev.

use std::cell::Cell;

/// Bytes in a machine word.
const WORD: usize = core::mem::size_of::<usize>();
/// Machine words tested per iteration of the scanning loops.
const UNROLL: usize = 8;
/// Bytes tested per iteration of the scanning loops.
const BLOCK: usize = WORD * UNROLL;
/// `0x0101…01` — the low bit of every byte.
const LO: usize = usize::MAX / 0xFF;
/// `0x8080…80` — the high bit of every byte.
const HI: usize = LO << 7;

/// `byte` repeated into every lane of a word.
const fn splat(byte: u8) -> usize {
    LO.wrapping_mul(byte as usize)
}

/// The SWAR zero test of `word`, as a mask rather than a bool, so a block of
/// them can be OR-ed into one branch.
///
/// Exact for the LOWEST set byte lane, which is all the callers below rely on:
/// every one of them re-scans byte by byte once this is non-zero.
const fn zero_mask(word: usize) -> usize {
    word.wrapping_sub(LO) & !word & HI
}

/// Whether `word` contains a zero byte.
const fn has_zero(word: usize) -> bool {
    zero_mask(word) != 0
}

/// Read a fixed-size chunk as a word with byte `i` in bits `8i..8i+8` on every
/// target.
///
/// `from_le_bytes` rather than `from_ne_bytes` so the lane order is fixed: on a
/// little-endian machine it is free, and on a big-endian one it is one byte
/// swap that buys the same code being correct there.
#[inline]
const fn word_at(chunk: &[u8; WORD]) -> usize {
    usize::from_le_bytes(*chunk)
}

/// The OR of the `UNROLL` SWAR tests over `block` against `splatted`.
///
/// `as_chunks` splits a FIXED-size array into fixed-size words, so there is no
/// bounds check and no `Option` in here at all — which is most of why the block
/// form is ~3x the same test written a word at a time.
#[inline]
fn block_mask(block: &[u8; BLOCK], splatted: usize) -> usize {
    let (words, _) = block.as_chunks::<WORD>();
    let mut hit = 0usize;
    for word in words {
        hit |= zero_mask(word_at(word) ^ splatted);
    }
    hit
}

/// The first index in `haystack` holding `needle`.
///
/// The `memchr::memchr` entry point.
pub(crate) fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    let splatted = splat(needle);
    let (blocks, tail) = haystack.as_chunks::<BLOCK>();
    let mut base = 0usize;
    for block in blocks {
        if block_mask(block, splatted) != 0 {
            for (i, &byte) in block.iter().enumerate() {
                if byte == needle {
                    return Some(base.saturating_add(i));
                }
            }
        }
        base = base.saturating_add(BLOCK);
    }
    // The tail, shorter than a block: whole words, then loose bytes.
    let (words, rest) = tail.as_chunks::<WORD>();
    for word in words {
        if has_zero(word_at(word) ^ splatted) {
            for (i, &byte) in word.iter().enumerate() {
                if byte == needle {
                    return Some(base.saturating_add(i));
                }
            }
        }
        base = base.saturating_add(WORD);
    }
    for (i, &byte) in rest.iter().enumerate() {
        if byte == needle {
            return Some(base.saturating_add(i));
        }
    }
    None
}

/// The first index in `haystack` holding either `first` or `second`.
///
/// The `memchr::memchr2` entry point. Used for ASCII case-insensitive scanning,
/// where the two needles are the two cases of one letter.
pub(crate) fn memchr2(first: u8, second: u8, haystack: &[u8]) -> Option<usize> {
    let splat_first = splat(first);
    let splat_second = splat(second);
    let (blocks, tail) = haystack.as_chunks::<BLOCK>();
    let mut base = 0usize;
    for block in blocks {
        if block_mask(block, splat_first) | block_mask(block, splat_second) != 0 {
            for (i, &byte) in block.iter().enumerate() {
                if byte == first || byte == second {
                    return Some(base.saturating_add(i));
                }
            }
        }
        base = base.saturating_add(BLOCK);
    }
    let (words, rest) = tail.as_chunks::<WORD>();
    for word in words {
        let read = word_at(word);
        if has_zero(read ^ splat_first) || has_zero(read ^ splat_second) {
            for (i, &byte) in word.iter().enumerate() {
                if byte == first || byte == second {
                    return Some(base.saturating_add(i));
                }
            }
        }
        base = base.saturating_add(WORD);
    }
    for (i, &byte) in rest.iter().enumerate() {
        if byte == first || byte == second {
            return Some(base.saturating_add(i));
        }
    }
    None
}

/// The LAST index in `haystack` holding `needle`.
///
/// The same split as [`memchr`], walked from the far end: the loose tail bytes
/// first, then the tail's whole words, then the blocks in reverse.
fn memrchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    let splatted = splat(needle);
    let (blocks, tail) = haystack.as_chunks::<BLOCK>();
    let (words, rest) = tail.as_chunks::<WORD>();
    let blocks_len = blocks.len().saturating_mul(BLOCK);
    let words_len = words.len().saturating_mul(WORD);
    let rest_base = blocks_len.saturating_add(words_len);
    for (i, &byte) in rest.iter().enumerate().rev() {
        if byte == needle {
            return Some(rest_base.saturating_add(i));
        }
    }
    for (index, word) in words.iter().enumerate().rev() {
        if has_zero(word_at(word) ^ splatted) {
            for (i, &byte) in word.iter().enumerate().rev() {
                if byte == needle {
                    return Some(
                        blocks_len
                            .saturating_add(index.saturating_mul(WORD))
                            .saturating_add(i),
                    );
                }
            }
        }
    }
    for (index, block) in blocks.iter().enumerate().rev() {
        if block_mask(block, splatted) != 0 {
            for (i, &byte) in block.iter().enumerate().rev() {
                if byte == needle {
                    return Some(index.saturating_mul(BLOCK).saturating_add(i));
                }
            }
        }
    }
    None
}

// ── The byte skip ──────────────────────────────────────────────────────────

/// How common `byte` is in the text a terminal search runs over — higher is
/// more common, and the search skips on the needle byte scoring LOWEST.
///
/// This is an estimate, not a measurement, and it does not have to be right:
/// a bad choice costs scanning time and can never change an answer. The
/// ordering is the obvious one for the mixture this program sees — build logs,
/// source, shell transcripts — space and the high-frequency English letters at
/// the top, then structure and digits, then the letters that are rare in
/// English, then capitals, then the punctuation that only appears in code,
/// then non-ASCII, and control bytes last because a line that contains one at
/// all is unusual.
const fn commonness(byte: u8) -> u8 {
    match byte {
        b' ' => 255,
        b'e' => 240,
        b't' => 230,
        b'a' => 225,
        b'o' => 220,
        b'i' => 215,
        b'n' => 210,
        b's' => 205,
        b'r' => 200,
        b'h' => 195,
        b'l' | b'd' | b'c' | b'u' => 190,
        b'.' | b',' | b'/' | b'-' | b'_' | b':' | b'(' | b')' => 180,
        b'0'..=b'9' => 170,
        b'm' | b'p' | b'f' | b'g' | b'w' | b'y' | b'b' | b'v' => 160,
        b'\n' | b'\t' => 150,
        b'=' | b'"' | b'\'' | b';' | b'{' | b'}' | b'[' | b']' | b'<' | b'>' => 140,
        b'k' | b'x' | b'j' | b'q' | b'z' => 90,
        b'A'..=b'Z' => 80,
        b'!' | b'?' | b'*' | b'&' | b'|' | b'%' | b'$' | b'#' | b'@' | b'+' | b'^' | b'~'
        | b'`' | b'\\' => 70,
        0x80..=0xFF => 40,
        _ => 10,
    }
}

/// [`commonness`] as a table, so choosing a skip byte is one load per needle
/// byte rather than a chain of range compares.
const COMMONNESS: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut byte = 0usize;
    while byte < 256 {
        table[byte] = commonness(byte as u8);
        byte += 1;
    }
    table
};

/// The needle byte to skip on, as `(byte, offset)`, picking the rarest and the
/// EARLIEST among equals so the choice is a function of the needle alone.
fn rare_byte(needle: &[u8]) -> Option<(u8, usize)> {
    let mut best_at = 0usize;
    let mut best_score = u8::MAX;
    for (at, &byte) in needle.iter().enumerate() {
        let score = COMMONNESS.get(byte as usize).copied().unwrap_or(0);
        if score < best_score {
            best_score = score;
            best_at = at;
        }
    }
    Some((needle.get(best_at).copied()?, best_at))
}

/// Skip calls made before the skip's usefulness is judged at all.
const PROBE: u32 = 8;
/// Bytes a skip call must be worth, on average, to stay switched on.
const MIN_STRIDE: u32 = 4;

/// The adaptive byte skip carried through one scan of one haystack.
///
/// `on` starts true and only ever falls: once the skip is not earning
/// [`MIN_STRIDE`] bytes per call it is off for the rest of this haystack, and
/// the loop degrades to Two-Way's own shift — which is the whole reason the
/// worst case stays `O(n + m)` instead of a prefilter's `O(n·m)`.
struct Skip {
    /// The needle byte scanned for.
    byte: u8,
    /// Its LOGICAL offset in the needle, in the direction being searched.
    offset: usize,
    /// Whether the skip is still paying for itself.
    on: bool,
    /// Skip calls so far.
    calls: u32,
    /// Bytes skipped over so far.
    jumped: u32,
}

impl Skip {
    /// A skip on `byte` at logical `offset`, switched on.
    fn new(byte: u8, offset: usize) -> Self {
        Self {
            byte,
            offset,
            on: true,
            calls: 0,
            jumped: 0,
        }
    }

    /// The first position at or after `at` where the skip byte lines up, or
    /// `None` when no such position exists — which means the needle does not
    /// occur at `at` or after, so the caller is finished.
    ///
    /// Every position this steps over is one where `needle[offset]` does not
    /// equal the haystack byte it would have to equal, so stepping over it
    /// cannot skip an occurrence.
    fn advance<const REV: bool>(&mut self, haystack: &View<'_, REV>, at: usize) -> Option<usize> {
        if !self.on {
            return Some(at);
        }
        let from = at.checked_add(self.offset)?;
        let next = haystack.scan(self.byte, from)?.checked_sub(self.offset)?;
        self.calls = self.calls.saturating_add(1);
        self.jumped = self
            .jumped
            .saturating_add(u32::try_from(next.saturating_sub(at)).unwrap_or(u32::MAX));
        if self.calls >= PROBE && self.jumped < self.calls.saturating_mul(MIN_STRIDE) {
            self.on = false;
        }
        Some(next)
    }
}

// ── Two-Way (Crochemore-Perrin) ────────────────────────────────────────────

/// A byte slice read forwards (`REV == false`) or backwards (`REV == true`).
///
/// The reverse search is the forward algorithm over reversed views of both the
/// haystack and the needle, so there is ONE Two-Way in this module and the
/// oracle's exhaustive sweep covers both directions of it. `REV` is a const
/// generic, so each direction monomorphizes to straight-line indexing with no
/// branch of its own.
struct View<'a, const REV: bool> {
    /// The underlying bytes, in physical order.
    bytes: &'a [u8],
}

impl<'a, const REV: bool> View<'a, REV> {
    /// A view of `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// The number of bytes in the view.
    #[inline]
    fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The byte at LOGICAL index `i`.
    #[inline]
    fn get(&self, i: usize) -> Option<u8> {
        let at = if REV {
            self.bytes.len().checked_sub(1)?.checked_sub(i)?
        } else {
            i
        };
        self.bytes.get(at).copied()
    }

    /// The first LOGICAL index at or after `from` whose byte is `byte`.
    #[inline]
    fn scan(&self, byte: u8, from: usize) -> Option<usize> {
        if REV {
            // Logical `from..` is physical `..len - from`, and the FIRST
            // logical hit in it is the LAST physical one.
            let end = self.bytes.len().checked_sub(from)?;
            let hit = memrchr(byte, self.bytes.get(..end)?)?;
            self.bytes.len().checked_sub(1)?.checked_sub(hit)
        } else {
            memchr(byte, self.bytes.get(from..)?)?.checked_add(from)
        }
    }
}

/// A needle's critical factorization, computed once per needle.
#[derive(Clone, Copy)]
struct Factorization {
    /// The critical position: bytes before it are the "left half".
    cut: usize,
    /// The period of the needle's prefix at that position.
    period: usize,
    /// Whether the prefix repeats at `period`, which is the form that may
    /// carry `memory` across a shift.
    periodic: bool,
}

/// The maximal suffix of `needle` under `<` (or under `>` when `reversed`),
/// returned as `(start index, period)`.
///
/// Indices are carried as "one more than the real index" so the algorithm's
/// `-1` sentinel is a plain `0` and no signed arithmetic appears. The returned
/// start index is already the value the caller wants — the real index plus one
/// IS the length of the prefix before the suffix.
fn maximal_suffix<const REV: bool>(needle: &View<'_, REV>, reversed: bool) -> (usize, usize) {
    let n = needle.len();
    let mut suffix = 0usize; // real suffix start + 1
    let mut probe = 1usize; // real probe position + 1
    let mut offset = 1usize;
    let mut period = 1usize;
    while probe.saturating_add(offset) <= n {
        let (Some(a), Some(b)) = (
            needle.get(probe.saturating_add(offset).saturating_sub(1)),
            needle.get(suffix.saturating_add(offset).saturating_sub(1)),
        ) else {
            break;
        };
        let smaller = if reversed { a > b } else { a < b };
        let larger = if reversed { a < b } else { a > b };
        if smaller {
            probe = probe.saturating_add(offset);
            offset = 1;
            period = probe.saturating_sub(suffix);
        } else if larger {
            suffix = probe;
            probe = suffix.saturating_add(1);
            offset = 1;
            period = 1;
        } else if offset == period {
            probe = probe.saturating_add(period);
            offset = 1;
        } else {
            offset = offset.saturating_add(1);
        }
    }
    (suffix, period)
}

/// The critical factorization: the later of the two maximal suffixes, with its
/// period, plus whether the needle's prefix repeats at that period.
fn factorize<const REV: bool>(needle: &View<'_, REV>) -> Factorization {
    let (cut_less, period_less) = maximal_suffix(needle, false);
    let (cut_more, period_more) = maximal_suffix(needle, true);
    let (cut, period) = if cut_less >= cut_more {
        (cut_less, period_less)
    } else {
        (cut_more, period_more)
    };
    // A needle whose prefix repeats at one period is PERIODIC, and the search
    // may then carry `memory` — how much of the left half a shift already
    // verified — so no byte is compared twice.
    let mut periodic = period <= needle.len();
    let mut i = 0usize;
    while periodic && i < cut {
        periodic = needle.get(i) == needle.get(period.saturating_add(i));
        i = i.saturating_add(1);
    }
    Factorization {
        cut,
        period,
        periodic,
    }
}

/// Two-Way substring search over a view. `needle` is non-empty and no longer
/// than `haystack`; the returned index is LOGICAL.
fn two_way<const REV: bool>(
    haystack: &View<'_, REV>,
    needle: &View<'_, REV>,
    factored: &Factorization,
    skip: &mut Skip,
    from: usize,
) -> Option<usize> {
    let n = haystack.len();
    let m = needle.len();
    let Factorization {
        cut,
        period,
        periodic,
    } = *factored;
    let mut at = from;
    if periodic {
        let mut memory = 0usize;
        while at.saturating_add(m) <= n {
            // Right half, left to right, resuming past what a shift proved.
            let mut i = cut.max(memory);
            while i < m && needle.get(i) == haystack.get(at.saturating_add(i)) {
                i = i.saturating_add(1);
            }
            if i < m {
                at = at.saturating_add(i.saturating_sub(cut)).saturating_add(1);
                memory = 0;
                at = skip.advance(haystack, at)?;
                continue;
            }
            // Left half, right to left, stopping at what a shift proved.
            let mut j = cut;
            while j > memory
                && needle.get(j.saturating_sub(1))
                    == haystack.get(at.saturating_add(j).saturating_sub(1))
            {
                j = j.saturating_sub(1);
            }
            if j <= memory {
                return Some(at);
            }
            // NO skip here: `memory` is only sound for the position `period`
            // ahead, so jumping past it would have to throw the memory away —
            // and the memory is what keeps the periodic case linear.
            at = at.saturating_add(period);
            memory = m.saturating_sub(period);
        }
        None
    } else {
        // The memoryless shift: one past the longer half.
        let shift = cut.max(m.saturating_sub(cut)).saturating_add(1);
        while at.saturating_add(m) <= n {
            let mut i = cut;
            while i < m && needle.get(i) == haystack.get(at.saturating_add(i)) {
                i = i.saturating_add(1);
            }
            if i < m {
                at = at.saturating_add(i.saturating_sub(cut)).saturating_add(1);
            } else {
                let mut j = cut;
                while j > 0
                    && needle.get(j.saturating_sub(1))
                        == haystack.get(at.saturating_add(j).saturating_sub(1))
                {
                    j = j.saturating_sub(1);
                }
                if j == 0 {
                    return Some(at);
                }
                at = at.saturating_add(shift);
            }
            at = skip.advance(haystack, at)?;
        }
        None
    }
}

/// One needle, prepared for repeated searching.
///
/// The critical factorization is two `O(m)` passes and the skip byte is one
/// more, all of which used to run on EVERY call — and a line with `k` matches
/// calls `find` `k` times. Preparing the needle once per query and reusing it
/// down the line is what makes that `O(m)` instead of `O(k·m)`.
pub(crate) struct Searcher<'n> {
    /// The bytes being searched for.
    needle: &'n [u8],
    /// The factorization used reading forwards, computed on first use.
    ///
    /// LAZY on purpose, and it is not a micro-optimization: the common verdict
    /// is "this line does not contain the query at all", and the skip scan
    /// below reaches that verdict in one pass without needing a factorization
    /// at all. Computing one up front made a 90-byte line — the size this
    /// crate searches most — 8x slower than the code this replaced.
    forward: Cell<Option<Factorization>>,
    /// The factorization used reading backwards, computed on first use.
    backward: Cell<Option<Factorization>>,
    /// The skip byte and its PHYSICAL offset in the needle, on first use.
    ///
    /// `None` means "not chosen yet", not "there is none": a non-empty needle
    /// always has a rarest byte, and an empty one is answered before this is
    /// ever read. Lazy for the same reason the factorizations are — a query
    /// the trigram index rejects outright must not pay a pass over the needle
    /// for a scan that never runs.
    rare: Cell<Option<(u8, usize)>>,
}

impl<'n> Searcher<'n> {
    /// Prepare `needle`.
    pub(crate) fn new(needle: &'n [u8]) -> Self {
        Self {
            needle,
            forward: Cell::new(None),
            backward: Cell::new(None),
            rare: Cell::new(None),
        }
    }

    /// The skip byte and its physical offset, choosing it once.
    fn rare(&self) -> Option<(u8, usize)> {
        match self.rare.get() {
            Some(ready) => Some(ready),
            None => {
                let chosen = rare_byte(self.needle)?;
                self.rare.set(Some(chosen));
                Some(chosen)
            }
        }
    }

    /// The bytes this searcher looks for.
    pub(crate) fn needle(&self) -> &'n [u8] {
        self.needle
    }

    /// The factorization for `REV`, computing it once.
    fn factorization<const REV: bool>(&self) -> Factorization {
        let slot = if REV { &self.backward } else { &self.forward };
        match slot.get() {
            Some(ready) => ready,
            None => {
                let computed = factorize(&View::<REV>::new(self.needle));
                slot.set(Some(computed));
                computed
            }
        }
    }

    /// The first index at which the needle occurs in `haystack`.
    pub(crate) fn find_in(&self, haystack: &[u8]) -> Option<usize> {
        let m = self.needle.len();
        let Some(&first) = self.needle.first() else {
            return Some(0);
        };
        if m > haystack.len() {
            return None;
        }
        if m == 1 {
            return memchr(first, haystack);
        }
        let (byte, offset) = self.rare()?;
        let haystack = View::<false>::new(haystack);
        let mut skip = Skip::new(byte, offset);
        // The FIRST candidate, before anything is factorized: a line that does
        // not contain the needle's rarest byte at all cannot contain the
        // needle, and that is the answer this crate asks for most.
        let from = skip.advance(&haystack, 0)?;
        two_way(
            &haystack,
            &View::<false>::new(self.needle),
            &self.factorization::<false>(),
            &mut skip,
            from,
        )
    }

    /// The LAST index at which the needle occurs in `haystack`.
    pub(crate) fn rfind_in(&self, haystack: &[u8]) -> Option<usize> {
        let n = haystack.len();
        let m = self.needle.len();
        let Some(&first) = self.needle.first() else {
            return Some(n);
        };
        if m > n {
            return None;
        }
        if m == 1 {
            return memrchr(first, haystack);
        }
        // The skip byte keeps its identity; only its offset mirrors.
        let (byte, at) = self.rare()?;
        let haystack = View::<true>::new(haystack);
        let mut skip = Skip::new(byte, m.checked_sub(1)?.checked_sub(at)?);
        let from = skip.advance(&haystack, 0)?;
        let logical = two_way(
            &haystack,
            &View::<true>::new(self.needle),
            &self.factorization::<true>(),
            &mut skip,
            from,
        )?;
        // A logical start of `logical` covers logical `logical..logical + m`,
        // which is physical `n - logical - m .. n - logical`.
        n.checked_sub(logical)?.checked_sub(m)
    }
}

#[cfg(test)]
mod tests {
    use super::{Searcher, memchr, memchr2};

    /// One-shot `memchr::memmem::find`, the shape the reference is written in.
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).find_in(haystack)
    }

    /// One-shot `memchr::memmem::rfind`.
    fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).rfind_in(haystack)
    }

    #[test]
    fn empty_needle_and_empty_haystack() {
        assert_eq!(find(b"", b""), Some(0));
        assert_eq!(rfind(b"", b""), Some(0));
        assert_eq!(find(b"abc", b""), Some(0));
        assert_eq!(rfind(b"abc", b""), Some(3));
        assert_eq!(find(b"", b"a"), None);
        assert_eq!(rfind(b"", b"a"), None);
        assert_eq!(memchr(b'a', b""), None);
        assert_eq!(memchr2(b'a', b'b', b""), None);
    }

    #[test]
    fn needle_longer_than_haystack() {
        assert_eq!(find(b"ab", b"abc"), None);
        assert_eq!(rfind(b"ab", b"abc"), None);
    }

    #[test]
    fn overlapping_matches_are_all_reachable() {
        // `rfind` must see the LAST of a run of overlapping occurrences.
        assert_eq!(find(b"aaaa", b"aa"), Some(0));
        assert_eq!(rfind(b"aaaa", b"aa"), Some(2));
        assert_eq!(find(b"abababa", b"aba"), Some(0));
        assert_eq!(rfind(b"abababa", b"aba"), Some(4));
    }

    #[test]
    fn non_ascii_bytes_are_just_bytes() {
        let haystack = "héllo wörld".as_bytes();
        assert_eq!(find(haystack, "ö".as_bytes()), Some(8));
        assert_eq!(memchr(0xC3, haystack), Some(1));
        assert_eq!(rfind(haystack, &[0xFF]), None);
    }

    #[test]
    fn word_boundaries_are_covered() {
        // A hit at every offset across, and past, the SWAR word and block
        // strides — 64 bytes on a 64-bit target, so the sweep runs past two
        // blocks and every tail length in between.
        for len in 0..200usize {
            for at in 0..len {
                let mut haystack = vec![b'.'; len];
                haystack[at] = b'!';
                assert_eq!(memchr(b'!', &haystack), Some(at), "len {len} at {at}");
                assert_eq!(
                    memchr2(b'!', b'?', &haystack),
                    Some(at),
                    "len {len} at {at}"
                );
                assert_eq!(
                    super::memrchr(b'!', &haystack),
                    Some(at),
                    "len {len} at {at}"
                );
            }
        }
    }

    /// The `0x01`-after-`0x00` case the module doc calls out: the SWAR test's
    /// higher bits lie, so a scanner that trusted them would report the wrong
    /// index. Named because it is invisible in random data.
    #[test]
    fn a_one_byte_after_a_zero_byte_does_not_shift_the_answer() {
        let haystack = [0x00u8, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01];
        assert_eq!(memchr(0x00, &haystack), Some(0));
        assert_eq!(memchr(0x01, &haystack), Some(1));
        assert_eq!(super::memrchr(0x00, &haystack), Some(0));
        assert_eq!(super::memrchr(0x01, &haystack), Some(8));
    }

    /// A prepared [`Searcher`] answers exactly what the one-shot entry points
    /// do, in either direction and in either ORDER — the lazily-computed
    /// factorizations must not leak the direction they were first asked for.
    #[test]
    fn a_reused_searcher_answers_the_same_as_a_fresh_one() {
        let haystacks: [&[u8]; 5] = [
            b"the quick brown fox jumps over the lazy dog",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
            b"abababababababab",
            b"",
            "héllo wörld — wörld".as_bytes(),
        ];
        for haystack in haystacks {
            for needle in [
                &b"the"[..],
                b"aab",
                b"abab",
                "wörld".as_bytes(),
                b"zzz",
                b"a",
                b"",
            ] {
                let searcher = Searcher::new(needle);
                // Reverse FIRST, so the backward factorization is the one the
                // cache fills in before the forward answer is asked for.
                assert_eq!(searcher.rfind_in(haystack), rfind(haystack, needle));
                assert_eq!(searcher.find_in(haystack), find(haystack, needle));
                assert_eq!(searcher.find_in(haystack), find(haystack, needle));
                assert_eq!(searcher.rfind_in(haystack), rfind(haystack, needle));
            }
        }
    }

    /// The skip's adaptive switch-off, exercised rather than asserted in prose:
    /// a needle whose rarest byte is the ONLY byte in the haystack makes every
    /// skip call return where it started, and the search must still terminate
    /// with the right answer rather than degrade into a prefilter's quadratic.
    #[test]
    fn a_useless_skip_switches_itself_off_and_the_answer_is_unchanged() {
        let haystack = vec![b'a'; 4096];
        assert_eq!(find(&haystack, &[b'a'; 64]), Some(0));
        assert_eq!(rfind(&haystack, &[b'a'; 64]), Some(4032));
        // The same shape with the match only at the very end, so the whole
        // haystack is walked with the skip off.
        let mut tail = vec![b'a'; 4096];
        let last = tail.len().saturating_sub(1);
        tail[last] = b'b';
        let needle: Vec<u8> = std::iter::repeat_n(b'a', 63).chain(*b"b").collect();
        assert_eq!(find(&tail, &needle), Some(4032));
        assert_eq!(rfind(&tail, &needle), Some(4032));
    }
}

/// Differential oracle: this module against the `memchr` it replaces.
///
/// Substring search is the kind of algorithm where "it passed the tests I
/// thought of" means nothing — Two-Way's critical factorization has cases that
/// only appear for particular periodic needles, and a hand-written case list
/// will not contain them. So the core of this module is EXHAUSTIVE: every
/// haystack and every needle up to a length bound over a two- and three-letter
/// alphabet, which is where periodicity, overlap and self-similarity all live.
/// Millions of pairs, each compared against the reference — in BOTH directions,
/// which is what covers the reverse view the reverse search reads through.
///
/// `memchr` is a `[dev-dependencies]` entry only; it reaches no shipped binary.
#[cfg(test)]
mod oracle {
    use super::{Searcher, memchr, memchr2, memrchr};

    /// One-shot `memchr::memmem::find`, the shape the reference is written in.
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).find_in(haystack)
    }

    /// One-shot `memchr::memmem::rfind`.
    fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).rfind_in(haystack)
    }

    /// This module's own entry points, named so the assertions below read as
    /// "ours" against "the reference".
    mod ours {
        pub(super) use super::{find, memchr, memchr2, memrchr, rfind};
    }

    /// The exhaustive sweeps run under `cargo test`, which is an UNOPTIMIZED
    /// build, so the bound is what fits in a few seconds there rather than the
    /// largest one that would still be affordable in release. A release test
    /// build (`cargo test --release -p aterm-search`) runs the same sweep two
    /// needle bytes deeper, which is ~33M pairs.
    const BINARY_NEEDLE: usize = if cfg!(debug_assertions) { 8 } else { 10 };
    /// Haystack bound for the binary sweep.
    const BINARY_HAYSTACK: usize = if cfg!(debug_assertions) { 11 } else { 13 };
    /// Needle bound for the ternary sweep.
    const TERNARY_NEEDLE: usize = if cfg!(debug_assertions) { 5 } else { 6 };
    /// Haystack bound for the ternary sweep.
    const TERNARY_HAYSTACK: usize = if cfg!(debug_assertions) { 7 } else { 8 };

    /// Every string of length `len` over `alphabet`, as bytes.
    fn strings(alphabet: &[u8], len: usize) -> impl Iterator<Item = Vec<u8>> + '_ {
        let total = alphabet.len().checked_pow(len as u32).expect("small");
        (0..total).map(move |mut n| {
            (0..len)
                .map(|_| {
                    let byte = alphabet[n % alphabet.len()];
                    n /= alphabet.len();
                    byte
                })
                .collect()
        })
    }

    fn check_pair(haystack: &[u8], needle: &[u8]) {
        assert_eq!(
            ours::find(haystack, needle),
            memchr::memmem::find(haystack, needle),
            "find({:?}, {:?})",
            String::from_utf8_lossy(haystack),
            String::from_utf8_lossy(needle),
        );
        assert_eq!(
            ours::rfind(haystack, needle),
            memchr::memmem::rfind(haystack, needle),
            "rfind({:?}, {:?})",
            String::from_utf8_lossy(haystack),
            String::from_utf8_lossy(needle),
        );
    }

    /// The shipped path is ONE [`Searcher`] reused down a whole screen of
    /// lines, in whichever direction the caller asked for — so the cached
    /// factorizations are checked against the reference with the cache both
    /// cold and warm, and warmed from EITHER direction first.
    ///
    /// The exhaustive sweeps already reach `find_in`/`rfind_in` through
    /// [`find`]/[`rfind`], which construct a searcher per call; what is only
    /// reachable here is a second call on a searcher that has already answered.
    #[test]
    fn a_warm_searcher_matches_the_reference() {
        let mut state: u64 = 0x1234_5678_9ABC_DEF0;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u32
        };
        for _ in 0..40_000 {
            let alphabet: &[u8] = match next() % 3 {
                0 => b"ab",
                1 => b"abcde",
                _ => &[0x00, 0x01, 0xFF, b'z'],
            };
            let h_len = (next() % 300) as usize;
            let haystack: Vec<u8> = (0..h_len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect();
            let n_len = (next() % 12) as usize;
            let needle: Vec<u8> = if h_len > n_len && next() % 2 == 0 {
                let at = (next() as usize) % (h_len - n_len + 1);
                haystack[at..at + n_len].to_vec()
            } else {
                (0..n_len)
                    .map(|_| alphabet[(next() as usize) % alphabet.len()])
                    .collect()
            };
            let want_find = memchr::memmem::find(&haystack, &needle);
            let want_rfind = memchr::memmem::rfind(&haystack, &needle);
            let forward_first = Searcher::new(&needle);
            assert_eq!(forward_first.find_in(&haystack), want_find);
            assert_eq!(forward_first.rfind_in(&haystack), want_rfind);
            assert_eq!(forward_first.find_in(&haystack), want_find);
            let reverse_first = Searcher::new(&needle);
            assert_eq!(reverse_first.rfind_in(&haystack), want_rfind);
            assert_eq!(reverse_first.find_in(&haystack), want_find);
            assert_eq!(reverse_first.rfind_in(&haystack), want_rfind);
        }
    }

    /// EXHAUSTIVE over a binary alphabet: every haystack up to
    /// [`BINARY_HAYSTACK`] bytes against every needle up to
    /// [`BINARY_NEEDLE`]. This is where a Two-Way bug lives if there is one —
    /// `aaaa`, `abab`, `aabaab` and friends are all in here, in every
    /// alignment.
    #[test]
    fn exhaustive_binary_alphabet() {
        let mut pairs = 0u64;
        for h_len in 0..=BINARY_HAYSTACK {
            for haystack in strings(b"ab", h_len) {
                for n_len in 0..=BINARY_NEEDLE.min(h_len + 1) {
                    for needle in strings(b"ab", n_len) {
                        check_pair(&haystack, &needle);
                        pairs += 1;
                    }
                }
            }
        }
        eprintln!("binary alphabet: {pairs} (haystack, needle) pairs");
        assert!(pairs > 1_000_000, "corpus too small: {pairs}");
    }

    /// EXHAUSTIVE over a ternary alphabet at a smaller bound, so needles that
    /// are not periodic in a binary world are still covered.
    #[test]
    fn exhaustive_ternary_alphabet() {
        let mut pairs = 0u64;
        for h_len in 0..=TERNARY_HAYSTACK {
            for haystack in strings(b"abc", h_len) {
                for n_len in 0..=TERNARY_NEEDLE {
                    for needle in strings(b"abc", n_len) {
                        check_pair(&haystack, &needle);
                        pairs += 1;
                    }
                }
            }
        }
        eprintln!("ternary alphabet: {pairs} (haystack, needle) pairs");
        assert!(pairs > 1_000_000, "corpus too small: {pairs}");
    }

    /// Randomized PERIODIC needles at lengths past what the exhaustive sweeps
    /// reach — a repeated unit is the shape that drives Two-Way's memory-
    /// carrying branch, and the branch the skip deliberately does NOT jump
    /// from. Longer than the exhaustive bound and cheap, so both bounds are
    /// covered from opposite ends.
    #[test]
    fn periodic_needles_match_the_reference() {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u32
        };
        for _ in 0..20_000 {
            let alphabet: &[u8] = match next() % 3 {
                0 => b"ab",
                1 => b"abc",
                _ => &[0x00, 0x01, 0xFF],
            };
            let h_len = (next() % 400) as usize;
            let haystack: Vec<u8> = (0..h_len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect();
            let unit_len = 1 + (next() % 4) as usize;
            let unit: Vec<u8> = (0..unit_len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect();
            let n_len = 1 + (next() % 40) as usize;
            let needle: Vec<u8> = unit.iter().cycle().take(n_len).copied().collect();
            check_pair(&haystack, &needle);
        }
    }

    /// The named edge cases the assignment calls out, kept as their own test so a
    /// regression names itself rather than arriving as one of four million.
    #[test]
    fn named_edges_match_the_reference() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"", b""),
            (b"", b"a"),
            (b"a", b""),
            (b"a", b"a"),
            (b"ab", b"abc"),
            (b"aaaa", b"aa"),
            (b"abababa", b"aba"),
            (b"aaaaaaaaaaaaaaaa", b"aaaaaaaaaaaaaaaa"),
            (b"aaaaaaaaaaaaaaaa", b"aaaaaaaab"),
            ("héllo wörld".as_bytes(), "ö".as_bytes()),
            ("日本語テキスト".as_bytes(), "テキ".as_bytes()),
            (&[0x00, 0x01, 0x00, 0x01], &[0x01, 0x00]),
            (&[0xFF; 40], &[0xFF; 40]),
            (&[0xFF; 40], &[0xFF; 41]),
        ];
        for (haystack, needle) in cases {
            check_pair(haystack, needle);
        }
    }

    /// Single-byte scanning at every length and every offset across the word
    /// stride, plus the two-needle form.
    #[test]
    fn byte_scanning_matches_the_reference() {
        for len in 0..=200usize {
            // Absent.
            let filler = vec![b'.'; len];
            assert_eq!(ours::memchr(b'!', &filler), memchr::memchr(b'!', &filler));
            assert_eq!(
                ours::memchr2(b'!', b'?', &filler),
                memchr::memchr2(b'!', b'?', &filler)
            );
            // Present at each offset.
            for at in 0..len {
                let mut haystack = filler.clone();
                haystack[at] = b'!';
                assert_eq!(
                    ours::memchr(b'!', &haystack),
                    memchr::memchr(b'!', &haystack),
                    "len {len} at {at}"
                );
                assert_eq!(
                    ours::memchr2(b'?', b'!', &haystack),
                    memchr::memchr2(b'?', b'!', &haystack),
                    "len {len} at {at}"
                );
                assert_eq!(
                    ours::memrchr(b'!', &haystack),
                    memchr::memrchr(b'!', &haystack),
                    "len {len} at {at}"
                );
            }
            // Two needles, one of each, so `memchr2` has to pick the EARLIER.
            for a in 0..len {
                for b in 0..len {
                    if a == b {
                        continue;
                    }
                    let mut haystack = filler.clone();
                    haystack[a] = b'x';
                    haystack[b] = b'y';
                    assert_eq!(
                        ours::memchr2(b'x', b'y', &haystack),
                        memchr::memchr2(b'x', b'y', &haystack),
                        "len {len}, x at {a}, y at {b}"
                    );
                }
                if len > 24 {
                    break; // the quadratic sweep only needs the short lengths
                }
            }
        }
    }

    /// Every byte VALUE, so no lane of the word test goes unexercised and the
    /// zero byte is not special-cased by accident.
    #[test]
    fn every_byte_value_is_findable() {
        for needle in 0u16..=255 {
            let needle = needle as u8;
            for len in [1usize, 7, 8, 9, 15, 16, 17, 33, 63, 64, 65, 129] {
                for at in 0..len {
                    let mut haystack = vec![needle.wrapping_add(1); len];
                    haystack[at] = needle;
                    assert_eq!(
                        ours::memchr(needle, &haystack),
                        memchr::memchr(needle, &haystack),
                        "byte {needle:#04x} len {len} at {at}"
                    );
                    assert_eq!(
                        ours::memrchr(needle, &haystack),
                        memchr::memrchr(needle, &haystack),
                        "byte {needle:#04x} len {len} at {at}"
                    );
                }
            }
        }
    }

    /// Long, structured haystacks: the shapes a terminal actually produces, at
    /// sizes past the word stride and past any prefilter's warm-up.
    #[test]
    fn realistic_haystacks_match_the_reference() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u32
        };
        let mut haystacks: Vec<Vec<u8>> = vec![
            b"error: cannot find value `foo` in this scope".to_vec(),
            vec![b' '; 4096],
            (0..4096).map(|i| (i % 251) as u8).collect(),
            "héllo wörld — a line with multi-byte text "
                .repeat(64)
                .into_bytes(),
        ];
        for _ in 0..200 {
            let len = (next() % 4096) as usize;
            haystacks.push((0..len).map(|_| (next() & 0xFF) as u8).collect());
            let len = (next() % 512) as usize;
            // Low-entropy: lots of candidate starts, which is where a naive
            // prefilter-and-verify degrades and Two-Way must not.
            haystacks.push((0..len).map(|_| b'a' + (next() % 2) as u8).collect());
        }
        for haystack in &haystacks {
            for n_len in [1usize, 2, 3, 5, 8, 13, 32, 64] {
                for _ in 0..8 {
                    // Needles both present (a slice of the haystack) and absent.
                    if haystack.len() > n_len {
                        let at = (next() as usize) % (haystack.len() - n_len + 1);
                        check_pair(haystack, &haystack[at..at + n_len]);
                    }
                    let needle: Vec<u8> = (0..n_len).map(|_| (next() & 0xFF) as u8).collect();
                    check_pair(haystack, &needle);
                    let runs: Vec<u8> = vec![b'a'; n_len];
                    check_pair(haystack, &runs);
                }
            }
        }
    }

    /// The repository's own bytes, searched for slices of themselves — a corpus
    /// with real structure rather than generated regularity.
    #[test]
    fn repository_corpus_matches_the_reference() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .to_path_buf();
        let mut stack = vec![root];
        let mut files = 0usize;
        let mut state: u64 = 0xC2B2_AE3D_27D4_EB4F;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u32
        };
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    stack.push(path);
                } else if meta.is_file() && meta.len() <= 256 * 1024 {
                    let Ok(raw) = std::fs::read(&path) else {
                        continue;
                    };
                    if raw.is_empty() {
                        continue;
                    }
                    files += 1;
                    for _ in 0..6 {
                        let n_len = 1 + (next() as usize) % 40;
                        if raw.len() > n_len {
                            let at = (next() as usize) % (raw.len() - n_len + 1);
                            check_pair(&raw, &raw[at..at + n_len]);
                        }
                    }
                    for byte in [b'\n', b' ', b'{', 0u8, 0xFFu8] {
                        assert_eq!(ours::memchr(byte, &raw), memchr::memchr(byte, &raw));
                        assert_eq!(ours::memrchr(byte, &raw), memchr::memrchr(byte, &raw));
                    }
                }
            }
        }
        assert!(files > 200, "corpus too small: {files} files");
        eprintln!("repository corpus: {files} files");
    }
}
