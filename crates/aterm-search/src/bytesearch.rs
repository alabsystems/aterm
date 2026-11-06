// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SWAR byte scanning (`memchr`, `memchr2`) and Two-Way substring search (`Searcher`).

use std::cell::Cell;

#[cfg(test)]
std::thread_local! {
    static BYTE_READS: Cell<usize> = const { Cell::new(0) };
}

const WORD: usize = core::mem::size_of::<usize>();
const UNROLL: usize = 8;
const BLOCK: usize = WORD * UNROLL;
const LO: usize = usize::MAX / 0xFF;
const HI: usize = LO << 7;

/// `byte` repeated into every lane of a word.
#[inline]
const fn splat(byte: u8) -> usize {
    LO.wrapping_mul(byte as usize)
}

/// The SWAR zero test of `word`.
#[inline]
const fn zero_mask(word: usize) -> usize {
    word.wrapping_sub(LO) & !word & HI
}

/// Whether `word` contains a zero byte.
#[inline]
const fn has_zero(word: usize) -> bool {
    zero_mask(word) != 0
}

/// Read a chunk as a word with byte `i` in bits `8i..8i+8` on every target.
#[inline]
const fn word_at(chunk: &[u8; WORD]) -> usize {
    usize::from_le_bytes(*chunk)
}

/// The OR of the `UNROLL` SWAR tests over `block` against `splatted`.
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
#[inline]
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

/// Returns the first index in `haystack` holding `first` or `second`.
#[inline]
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

/// The last index in `haystack` holding `needle`: [`memchr`]'s split walked from the far end.
#[inline]
pub(crate) fn memrchr(needle: u8, haystack: &[u8]) -> Option<usize> {
    memrchr_impl::<false>(needle, needle, haystack)
}

/// The last index holding either byte, scanning both alternatives in one pass.
#[inline]
pub(crate) fn memrchr2(first: u8, second: u8, haystack: &[u8]) -> Option<usize> {
    memrchr_impl::<true>(first, second, haystack)
}

#[inline]
fn memrchr_impl<const TWO: bool>(needle: u8, second: u8, haystack: &[u8]) -> Option<usize> {
    let splatted = splat(needle);
    let second_splatted = splat(second);
    let (blocks, tail) = haystack.as_chunks::<BLOCK>();
    let (words, rest) = tail.as_chunks::<WORD>();
    let blocks_len = blocks.len().saturating_mul(BLOCK);
    let words_len = words.len().saturating_mul(WORD);
    let rest_base = blocks_len.saturating_add(words_len);
    for (i, &byte) in rest.iter().enumerate().rev() {
        if byte == needle || (TWO && byte == second) {
            return Some(rest_base.saturating_add(i));
        }
    }
    for (index, word) in words.iter().enumerate().rev() {
        let read = word_at(word);
        if has_zero(read ^ splatted) || (TWO && has_zero(read ^ second_splatted)) {
            for (i, &byte) in word.iter().enumerate().rev() {
                if byte == needle || (TWO && byte == second) {
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
        if block_mask(block, splatted) != 0 || (TWO && block_mask(block, second_splatted) != 0) {
            for (i, &byte) in block.iter().enumerate().rev() {
                if byte == needle || (TWO && byte == second) {
                    return Some(index.saturating_mul(BLOCK).saturating_add(i));
                }
            }
        }
    }
    None
}

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

const COMMONNESS: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut byte = 0usize;
    while byte < 256 {
        table[byte] = commonness(byte as u8);
        byte += 1;
    }
    table
};

/// The needle byte to skip on, normalized for the selected comparison mode.
#[inline]
fn rare_byte<const FOLD: bool>(needle: &[u8]) -> Option<(u8, usize)> {
    let mut best_at = 0usize;
    let mut best_score = u8::MAX;
    for (at, &byte) in needle.iter().enumerate() {
        let folded = if FOLD {
            byte.to_ascii_lowercase()
        } else {
            byte
        };
        let score = COMMONNESS.get(folded as usize).copied().unwrap_or(0);
        if score < best_score {
            best_score = score;
            best_at = at;
        }
    }
    let byte = needle.get(best_at).copied()?;
    Some((
        if FOLD {
            byte.to_ascii_lowercase()
        } else {
            byte
        },
        best_at,
    ))
}

const PROBE: u32 = 8;
const MIN_STRIDE: u32 = 4;

struct Skip {
    /// The needle byte scanned for.
    byte: u8,
    /// Its logical offset in the needle, in the direction being searched.
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
    #[inline]
    const fn new(byte: u8, offset: usize) -> Self {
        Self {
            byte,
            offset,
            on: true,
            calls: 0,
            jumped: 0,
        }
    }

    /// The first position at or after `at` where the skip byte lines up.
    #[inline]
    fn advance<const REV: bool, const FOLD: bool>(
        &mut self,
        haystack: &View<'_, REV, FOLD>,
        at: usize,
    ) -> Option<usize> {
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

struct View<'a, const REV: bool, const FOLD: bool = false> {
    /// The underlying bytes, in physical order.
    bytes: &'a [u8],
}

impl<'a, const REV: bool, const FOLD: bool> View<'a, REV, FOLD> {
    /// A view of `bytes`.
    #[inline]
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    /// The number of bytes in the view.
    #[inline]
    const fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The byte at logical index `i`, normalized by the selected ASCII-fold mode.
    #[inline]
    fn get(&self, i: usize) -> Option<u8> {
        #[cfg(test)]
        BYTE_READS.with(|reads| reads.set(reads.get() + 1));
        let at = if REV {
            self.bytes.len().checked_sub(1)?.checked_sub(i)?
        } else {
            i
        };
        self.bytes.get(at).copied().map(|byte| {
            if FOLD {
                byte.to_ascii_lowercase()
            } else {
                byte
            }
        })
    }

    /// The first logical index at or after `from` whose byte matches under the selected mode.
    #[inline]
    fn scan(&self, byte: u8, from: usize) -> Option<usize> {
        let lower = if FOLD {
            byte.to_ascii_lowercase()
        } else {
            byte
        };
        let upper = if FOLD {
            byte.to_ascii_uppercase()
        } else {
            byte
        };
        if REV {
            let end = self.bytes.len().checked_sub(from)?;
            let eligible = self.bytes.get(..end)?;
            let hit = if lower == upper {
                memrchr(lower, eligible)
            } else {
                memrchr2(lower, upper, eligible)
            }?;
            self.bytes.len().checked_sub(1)?.checked_sub(hit)
        } else {
            let eligible = self.bytes.get(from..)?;
            let hit = if lower == upper {
                memchr(lower, eligible)
            } else {
                memchr2(lower, upper, eligible)
            }?;
            hit.checked_add(from)
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
    /// Whether the prefix repeats at `period`.
    periodic: bool,
}

fn maximal_suffix<const REV: bool, const FOLD: bool>(
    needle: &View<'_, REV, FOLD>,
    reversed: bool,
) -> (usize, usize) {
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

fn factorize<const REV: bool, const FOLD: bool>(needle: &View<'_, REV, FOLD>) -> Factorization {
    let (cut_less, period_less) = maximal_suffix(needle, false);
    let (cut_more, period_more) = maximal_suffix(needle, true);
    let (cut, period) = if cut_less >= cut_more {
        (cut_less, period_less)
    } else {
        (cut_more, period_more)
    };
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

struct ScanState {
    at: usize,
    memory: usize,
}

fn two_way<const REV: bool, const FOLD: bool>(
    haystack: &View<'_, REV, FOLD>,
    needle: &View<'_, REV, FOLD>,
    factored: &Factorization,
    skip: &mut Skip,
    state: &mut ScanState,
) -> Option<usize> {
    let n = haystack.len();
    let m = needle.len();
    let Factorization {
        cut,
        period,
        periodic,
    } = *factored;
    let mut at = state.at;
    if periodic {
        let mut memory = state.memory;
        while at.saturating_add(m) <= n {
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
            let mut j = cut;
            while j > memory
                && needle.get(j.saturating_sub(1))
                    == haystack.get(at.saturating_add(j).saturating_sub(1))
            {
                j = j.saturating_sub(1);
            }
            if j <= memory {
                // Shifting a full match by its period preserves this suffix as a known prefix.
                state.at = at.saturating_add(period);
                state.memory = m.saturating_sub(period);
                return Some(at);
            }
            at = at.saturating_add(period);
            memory = m.saturating_sub(period);
        }
        None
    } else {
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
                    state.at = at.saturating_add(shift);
                    return Some(at);
                }
                at = at.saturating_add(shift);
            }
            at = skip.advance(haystack, at)?;
        }
        None
    }
}

/// Resumable overlapping byte matches, retaining Two-Way's known periodic prefix.
pub(crate) struct Matches<'h, 'n, const REV: bool, const FOLD: bool = false> {
    haystack: View<'h, REV, FOLD>,
    needle: View<'n, REV, FOLD>,
    factored: Factorization,
    skip: Skip,
    state: ScanState,
    done: bool,
}

impl<const REV: bool, const FOLD: bool> Iterator for Matches<'_, '_, REV, FOLD> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        if self.done {
            return None;
        }
        let logical = if self.needle.len() == 1 {
            let found = self.haystack.scan(self.needle.get(0)?, self.state.at);
            if let Some(at) = found {
                self.state.at = at.saturating_add(1);
            }
            found
        } else if FOLD && self.needle.len() == 2 {
            // Filter on the physical first needle byte in either direction.
            let scan_offset = if REV { 1 } else { 0 };
            let first = self.needle.get(scan_offset)?;
            let other_offset = 1usize.saturating_sub(scan_offset);
            let other = self.needle.get(other_offset)?;
            loop {
                let Some(hit) = self
                    .haystack
                    .scan(first, self.state.at.saturating_add(scan_offset))
                else {
                    break None;
                };
                let at = hit.checked_sub(scan_offset)?;
                self.state.at = at.saturating_add(1);
                if at.saturating_add(2) > self.haystack.len() {
                    break None;
                }
                if Some(other) == self.haystack.get(at.saturating_add(other_offset)) {
                    break Some(at);
                }
            }
        } else {
            two_way(
                &self.haystack,
                &self.needle,
                &self.factored,
                &mut self.skip,
                &mut self.state,
            )
        };
        let Some(logical) = logical else {
            self.done = true;
            return None;
        };
        if REV {
            self.haystack
                .len()
                .checked_sub(logical)?
                .checked_sub(self.needle.len())
        } else {
            Some(logical)
        }
    }
}

/// Prepared needle for repeated forward and reverse substring searches.
pub(crate) struct Searcher<'n, const FOLD: bool = false> {
    /// The bytes being searched for.
    needle: &'n [u8],
    /// The factorization used reading forwards.
    forward: Cell<Option<Factorization>>,
    /// The factorization used reading backwards, computed on first use.
    backward: Cell<Option<Factorization>>,
    /// The skip byte and its physical offset in the needle.
    rare: Cell<Option<(u8, usize)>>,
}

impl<'n> Searcher<'n> {
    /// Prepare a case-sensitive byte needle.
    #[inline]
    pub(crate) const fn new(needle: &'n [u8]) -> Self {
        Self::with_mode(needle)
    }
}

impl<'n> Searcher<'n, true> {
    /// Prepare an ASCII case-insensitive byte needle without copying it.
    #[inline]
    pub(crate) const fn new_ascii(needle: &'n [u8]) -> Self {
        Self::with_mode(needle)
    }
}

impl<'n, const FOLD: bool> Searcher<'n, FOLD> {
    /// Walk all overlapping occurrences of a nonempty needle, in the selected direction.
    pub(crate) fn matches_in<'h, const REV: bool>(
        &self,
        bytes: &'h [u8],
    ) -> Matches<'h, 'n, REV, FOLD> {
        let haystack = View::<REV, FOLD>::new(bytes);
        let needle = View::<REV, FOLD>::new(self.needle);
        let mut done = needle.len() == 0 || needle.len() > haystack.len();
        let mut skip = Skip::new(0, 0);
        let mut factored = Factorization {
            cut: 0,
            period: 1,
            periodic: false,
        };
        let mut state = ScanState { at: 0, memory: 0 };
        if !done && needle.len() > 1 && (!FOLD || needle.len() > 2) {
            let (byte, physical) = self.rare().unwrap_or((0, 0));
            let offset = if REV {
                needle.len().saturating_sub(1).saturating_sub(physical)
            } else {
                physical
            };
            skip = Skip::new(byte, offset);
            if let Some(at) = skip.advance(&haystack, 0) {
                state.at = at;
                factored = self.factorization::<REV>();
            } else {
                done = true;
            }
        }
        Matches {
            haystack,
            needle,
            factored,
            skip,
            state,
            done,
        }
    }
    /// Prepare `needle`.
    #[inline]
    const fn with_mode(needle: &'n [u8]) -> Self {
        Self {
            needle,
            forward: Cell::new(None),
            backward: Cell::new(None),
            rare: Cell::new(None),
        }
    }

    /// The skip byte and its physical offset, choosing it once.
    #[inline]
    fn rare(&self) -> Option<(u8, usize)> {
        match self.rare.get() {
            Some(ready) => Some(ready),
            None => {
                let chosen = rare_byte::<FOLD>(self.needle)?;
                self.rare.set(Some(chosen));
                Some(chosen)
            }
        }
    }

    /// The bytes this searcher looks for.
    #[inline]
    pub(crate) const fn needle(&self) -> &'n [u8] {
        self.needle
    }

    /// The factorization for `REV`, computing it once.
    #[inline]
    fn factorization<const REV: bool>(&self) -> Factorization {
        let slot = if REV { &self.backward } else { &self.forward };
        match slot.get() {
            Some(ready) => ready,
            None => {
                let computed = factorize(&View::<REV, FOLD>::new(self.needle));
                slot.set(Some(computed));
                computed
            }
        }
    }

    /// The first index at which the needle occurs in `haystack`.
    #[cfg(test)]
    #[inline]
    pub(crate) fn find_in(&self, haystack: &[u8]) -> Option<usize> {
        let m = self.needle.len();
        let Some(&first) = self.needle.first() else {
            return Some(0);
        };
        if m > haystack.len() {
            return None;
        }
        if m == 1 && !FOLD {
            return memchr(first, haystack);
        }
        self.matches_in::<false>(haystack).next()
    }

    /// The last index at which the needle occurs in `haystack`.
    #[cfg(test)]
    #[inline]
    pub(crate) fn rfind_in(&self, haystack: &[u8]) -> Option<usize> {
        let n = haystack.len();
        let m = self.needle.len();
        let Some(&first) = self.needle.first() else {
            return Some(n);
        };
        if m > n {
            return None;
        }
        if m == 1 && !FOLD {
            return memrchr(first, haystack);
        }
        self.matches_in::<true>(haystack).next()
    }
}

#[cfg(test)]
mod tests {
    use super::{Searcher, memchr};

    #[test]
    fn ascii_folded_cursors_match_a_byte_reference_in_both_directions() {
        use aterm_spec::rng::XorShift;
        let mut rng = XorShift::new(0xA5C1_1F01_DE00_0001);
        let alphabet = b"aAbBcC .\0\xff";
        for case in 0..8192 {
            let len = rng.index(257);
            let haystack: Vec<_> = (0..len).map(|_| *rng.pick(alphabet)).collect();
            let needle_len = rng.index(33);
            let needle: Vec<_> = if case % 2 == 0 && needle_len < len {
                let at = rng.index(len - needle_len + 1);
                haystack[at..at + needle_len]
                    .iter()
                    .map(u8::to_ascii_uppercase)
                    .collect()
            } else {
                (0..needle_len).map(|_| *rng.pick(alphabet)).collect()
            };
            let expected = (0..len).filter(|&at| {
                !needle.is_empty()
                    && haystack
                        .get(at..at + needle.len())
                        .is_some_and(|part| part.eq_ignore_ascii_case(&needle))
            });
            let searcher = Searcher::new_ascii(&needle);
            assert!(searcher.matches_in::<false>(&haystack).eq(expected.clone()));
            assert!(searcher.matches_in::<true>(&haystack).eq(expected.rev()));
        }
        let haystack = b"aA".repeat(8192);
        let needle = vec![b'A'; 256];
        for backward in [false, true] {
            let before = super::BYTE_READS.with(std::cell::Cell::get);
            let searcher = Searcher::new_ascii(&needle);
            let expected = 0..=haystack.len() - needle.len();
            if backward {
                assert!(searcher.matches_in::<true>(&haystack).eq(expected.rev()));
            } else {
                assert!(searcher.matches_in::<false>(&haystack).eq(expected));
            }
            let reads = super::BYTE_READS.with(std::cell::Cell::get) - before;
            assert!(reads <= 20 * (haystack.len() + needle.len()));
        }
    }

    #[test]
    fn reverse_two_byte_scans_match_a_reference_across_word_and_block_edges() {
        for len in 0..=200 {
            for first in [0, 1, b'a', b'A', 0xff] {
                let second = first.wrapping_add(1);
                let mut haystack = vec![first.wrapping_add(2); len];
                for at in 0..len {
                    haystack[at] = if at % 2 == 0 { first } else { second };
                    let expected = haystack.iter().rposition(|&b| b == first || b == second);
                    assert_eq!(super::memrchr2(first, second, &haystack), expected);
                    assert_eq!(super::memrchr2(second, first, &haystack), expected);
                }
                assert_eq!(
                    super::memrchr2(first, second, &vec![first.wrapping_add(2); len]),
                    None
                );
            }
        }
    }

    #[test]
    fn dense_periodic_matches_reuse_prefix_facts_in_both_directions() {
        for unit in [&b"a"[..], &b"ab"[..], &b"abacaba"[..]] {
            let haystack = unit.repeat(8192);
            let needle = unit.repeat(256);
            for backward in [false, true] {
                let before = super::BYTE_READS.with(std::cell::Cell::get);
                let searcher = Searcher::new(&needle);
                let count = if backward {
                    searcher.matches_in::<true>(&haystack).count()
                } else {
                    searcher.matches_in::<false>(&haystack).count()
                };
                let reads = super::BYTE_READS.with(std::cell::Cell::get) - before;
                assert_eq!(count, 8192 - 256 + 1);
                assert!(
                    reads <= 20 * (haystack.len() + needle.len()),
                    "unit={unit:?} backward={backward}: {reads} byte reads"
                );
            }
        }
        // Negative control: discarding the known prefix recreates the repeated work.
        let haystack = vec![b'a'; 8192];
        let needle = vec![b'a'; 256];
        let searcher = Searcher::new(&needle);
        let before = super::BYTE_READS.with(std::cell::Cell::get);
        let mut cursor = searcher.matches_in::<false>(&haystack);
        while cursor.next().is_some() {
            cursor.state.memory = 0;
        }
        let reads = super::BYTE_READS.with(std::cell::Cell::get) - before;
        assert!(
            reads > 20 * (haystack.len() + needle.len()),
            "the bound must detect a discarded periodic prefix"
        );
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).find_in(haystack)
    }

    fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).rfind_in(haystack)
    }

    #[test]
    fn a_one_byte_after_a_zero_byte_does_not_shift_the_answer() {
        let haystack = [0x00u8, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01];
        assert_eq!(memchr(0x00, &haystack), Some(0));
        assert_eq!(memchr(0x01, &haystack), Some(1));
        assert_eq!(super::memrchr(0x00, &haystack), Some(0));
        assert_eq!(super::memrchr(0x01, &haystack), Some(8));
    }

    #[test]
    fn a_useless_skip_switches_itself_off_and_the_answer_is_unchanged() {
        let haystack = vec![b'a'; 4096];
        assert_eq!(find(&haystack, &[b'a'; 64]), Some(0));
        assert_eq!(rfind(&haystack, &[b'a'; 64]), Some(4032));
        let mut tail = vec![b'a'; 4096];
        let last = tail.len().saturating_sub(1);
        tail[last] = b'b';
        let needle: Vec<u8> = std::iter::repeat_n(b'a', 63).chain(*b"b").collect();
        assert_eq!(find(&tail, &needle), Some(4032));
        assert_eq!(rfind(&tail, &needle), Some(4032));
    }
}

/// Two-Way's critical factorization has cases only particular periodic needles reach.
#[cfg(test)]
mod oracle {
    use super::{Searcher, memchr, memchr2, memrchr};

    mod naive {
        pub(super) fn memchr(n: u8, h: &[u8]) -> Option<usize> {
            h.iter().position(|&b| b == n)
        }
        pub(super) fn memchr2(a: u8, b: u8, h: &[u8]) -> Option<usize> {
            h.iter().position(|&x| x == a || x == b)
        }
        pub(super) fn memrchr(n: u8, h: &[u8]) -> Option<usize> {
            h.iter().rposition(|&b| b == n)
        }
        pub(super) mod memmem {
            pub(in super::super) fn find(h: &[u8], n: &[u8]) -> Option<usize> {
                (0..=h.len().checked_sub(n.len())?).find(|&i| &h[i..i + n.len()] == n)
            }
            pub(in super::super) fn rfind(h: &[u8], n: &[u8]) -> Option<usize> {
                (0..=h.len().checked_sub(n.len())?)
                    .rev()
                    .find(|&i| &h[i..i + n.len()] == n)
            }
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).find_in(haystack)
    }

    fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        Searcher::new(needle).rfind_in(haystack)
    }

    mod ours {
        pub(super) use super::{find, memchr, memchr2, memrchr, rfind};
    }

    const BINARY_NEEDLE: usize = if cfg!(debug_assertions) { 8 } else { 10 };
    const BINARY_HAYSTACK: usize = if cfg!(debug_assertions) { 11 } else { 13 };
    const TERNARY_NEEDLE: usize = if cfg!(debug_assertions) { 5 } else { 6 };
    const TERNARY_HAYSTACK: usize = if cfg!(debug_assertions) { 7 } else { 8 };

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
        let searcher = Searcher::new(needle);
        let expected = (0..haystack.len()).filter(|&at| {
            !needle.is_empty() && haystack.get(at..at + needle.len()) == Some(needle)
        });
        assert!(
            searcher.matches_in::<false>(haystack).eq(expected.clone()),
            "overlapping forward {haystack:?} / {needle:?}"
        );
        assert!(
            searcher.matches_in::<true>(haystack).eq(expected.rev()),
            "overlapping backward {haystack:?} / {needle:?}"
        );
        assert_eq!(
            ours::find(haystack, needle),
            naive::memmem::find(haystack, needle),
            "find({:?}, {:?})",
            String::from_utf8_lossy(haystack),
            String::from_utf8_lossy(needle),
        );
        assert_eq!(
            ours::rfind(haystack, needle),
            naive::memmem::rfind(haystack, needle),
            "rfind({:?}, {:?})",
            String::from_utf8_lossy(haystack),
            String::from_utf8_lossy(needle),
        );
    }

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
            let want_find = naive::memmem::find(&haystack, &needle);
            let want_rfind = naive::memmem::rfind(&haystack, &needle);
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

    #[test]
    fn byte_scanning_matches_the_reference() {
        for len in 0..=200usize {
            let filler = vec![b'.'; len];
            assert_eq!(ours::memchr(b'!', &filler), naive::memchr(b'!', &filler));
            assert_eq!(
                ours::memchr2(b'!', b'?', &filler),
                naive::memchr2(b'!', b'?', &filler)
            );
            for at in 0..len {
                let mut haystack = filler.clone();
                haystack[at] = b'!';
                assert_eq!(
                    ours::memchr(b'!', &haystack),
                    naive::memchr(b'!', &haystack),
                    "len {len} at {at}"
                );
                assert_eq!(
                    ours::memchr2(b'?', b'!', &haystack),
                    naive::memchr2(b'?', b'!', &haystack),
                    "len {len} at {at}"
                );
                assert_eq!(
                    ours::memrchr(b'!', &haystack),
                    naive::memrchr(b'!', &haystack),
                    "len {len} at {at}"
                );
            }
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
                        naive::memchr2(b'x', b'y', &haystack),
                        "len {len}, x at {a}, y at {b}"
                    );
                }
                if len > 24 {
                    break; // the quadratic sweep only needs the short lengths
                }
            }
        }
    }

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
                        naive::memchr(needle, &haystack),
                        "byte {needle:#04x} len {len} at {at}"
                    );
                    assert_eq!(
                        ours::memrchr(needle, &haystack),
                        naive::memrchr(needle, &haystack),
                        "byte {needle:#04x} len {len} at {at}"
                    );
                }
            }
        }
    }

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
            haystacks.push((0..len).map(|_| b'a' + (next() % 2) as u8).collect());
        }
        for haystack in &haystacks {
            for n_len in [1usize, 2, 3, 5, 8, 13, 32, 64] {
                for _ in 0..8 {
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
}
