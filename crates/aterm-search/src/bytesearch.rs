// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! SWAR byte scanning (`memchr`, `memchr2`) and Two-Way substring search (`Searcher`).

use std::borrow::Cow;
use std::cell::Cell;

#[cfg(test)]
std::thread_local! {
    static BYTE_READS: Cell<usize> = const { Cell::new(0) };
    static FACTORIZATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn factorizations() -> usize {
    FACTORIZATIONS.with(Cell::get)
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
    #[cfg(test)]
    FACTORIZATIONS.with(|n| n.set(n.get() + 1));
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
    plan: Cow<'n, NeedlePlan>,
}

#[derive(Clone)]
struct NeedlePlan {
    /// The factorization used reading forwards.
    forward: Cell<Option<Factorization>>,
    /// The factorization used reading backwards, computed on first use.
    backward: Cell<Option<Factorization>>,
    /// The skip byte and its physical offset in the needle.
    rare: Cell<Option<(u8, usize)>>,
}

impl NeedlePlan {
    const fn new() -> Self {
        Self {
            forward: Cell::new(None),
            backward: Cell::new(None),
            rare: Cell::new(None),
        }
    }
}

/// An immutable owned query and its preparation, shared by successive row searches.
pub(crate) struct OwnedSearcher {
    needle: String,
    /// Exact-byte facts, including the raw byte chosen for skipping.
    plan: NeedlePlan,
    /// ASCII folding can change both the skip byte and the query's period.
    ascii_plan: NeedlePlan,
}

impl OwnedSearcher {
    pub(crate) fn new(query: &str) -> Self {
        query.to_owned().into()
    }

    pub(crate) fn searcher(&self) -> Searcher<'_> {
        Searcher {
            needle: self.needle.as_bytes(),
            plan: Cow::Borrowed(&self.plan),
        }
    }

    pub(crate) fn searcher_ascii(&self) -> Searcher<'_, true> {
        Searcher {
            needle: self.needle.as_bytes(),
            plan: Cow::Borrowed(&self.ascii_plan),
        }
    }
}

impl From<String> for OwnedSearcher {
    fn from(needle: String) -> Self {
        Self {
            needle,
            plan: NeedlePlan::new(),
            ascii_plan: NeedlePlan::new(),
        }
    }
}

impl std::ops::Deref for OwnedSearcher {
    type Target = str;

    fn deref(&self) -> &str {
        &self.needle
    }
}

impl<'n> Searcher<'n> {
    /// Prepare a case-sensitive byte needle.
    #[inline]
    pub(crate) const fn new(needle: &'n [u8]) -> Self {
        Self::with_mode(needle)
    }
}

#[cfg(test)]
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
            plan: Cow::Owned(NeedlePlan::new()),
        }
    }

    /// The skip byte and its physical offset, choosing it once.
    #[inline]
    fn rare(&self) -> Option<(u8, usize)> {
        match self.plan.rare.get() {
            Some(ready) => Some(ready),
            None => {
                let chosen = rare_byte::<FOLD>(self.needle)?;
                self.plan.rare.set(Some(chosen));
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
        let slot = if REV {
            &self.plan.backward
        } else {
            &self.plan.forward
        };
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
#[path = "bytesearch_tests.rs"]
mod tests;

/// Two-Way's critical factorization has cases only particular periodic needles reach.
#[cfg(test)]
#[path = "bytesearch_oracle_tests.rs"]
mod oracle;
