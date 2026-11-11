// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Finite State Entropy encoding (RFC 8878 §4.1): normalizing a histogram, writing its
//! table description (§4.1.1), and the encoding table that mirrors the decoding table a
//! decoder builds from the same description.

use super::bits::BitWriter;

/// The smallest accuracy log a table description can carry: its 4-bit field holds the
/// log less 5 (RFC 8878 §4.1.1).
pub(crate) const MIN_LOG: u32 = 5;

/// `floor(log2(x))` for `x > 0`.
#[inline]
pub(crate) const fn highbit(x: u32) -> u32 {
    31 - x.leading_zeros()
}

/// The accuracy log for `n` symbols drawn from `0..=max_symbol`, at most `max_log`: the
/// fewest bits that still spend states where the histogram has detail, as libzstd's
/// `FSE_optimalTableLog` chooses it (a coding choice; any log from 5 to the limit is
/// valid, RFC 8878 §4.1.1).
pub(crate) fn optimal_log(max_log: u32, n: usize, max_symbol: u32) -> u32 {
    let n = u32::try_from(n).unwrap_or(u32::MAX).max(2);
    let mut log = max_log;
    let from_src = highbit(n - 1).saturating_sub(2);
    if from_src < log {
        log = from_src;
    }
    let min_bits = (highbit(n) + 1).min(highbit(max_symbol.max(1)) + 2);
    if min_bits > log {
        log = min_bits;
    }
    log.clamp(MIN_LOG, max_log)
}

/// `counts` scaled to sum `1 << log`, every present symbol at least 1, the rounding
/// spent where it costs least: the `Normalized_Counter` a table description carries.
/// Never writes -1 ("less than 1"): like 1, it holds one state, so it would save nothing.
///
/// `counts` must hold at least two present symbols and at most `1 << log` of them.
pub(crate) fn normalize(counts: &[u32], log: u32, norm: &mut Vec<i16>) {
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    let scale = 1i64 << log;
    norm.clear();
    norm.resize(counts.len(), 0);
    let mut sum = 0i64;
    for (n, &c) in norm.iter_mut().zip(counts) {
        if c > 0 {
            let share = ((u64::from(c) << log) + total / 2) / total.max(1);
            let share = share.max(1) as i64;
            *n = share as i16;
            sum += share;
        }
    }
    // Too many states: take them where a state costs least (the largest shares).
    while sum > scale {
        let mut best: Option<(usize, f64)> = None;
        for (s, (&n, &c)) in norm.iter().zip(counts).enumerate() {
            if n > 1 {
                let loss = f64::from(c) * (f64::from(n) / f64::from(n - 1)).log2();
                if best.is_none_or(|(_, l)| loss < l) {
                    best = Some((s, loss));
                }
            }
        }
        let Some((s, _)) = best else { break };
        if let Some(n) = norm.get_mut(s) {
            *n -= 1;
        }
        sum -= 1;
    }
    // Too few: give them where a state saves most.
    while sum < scale {
        let mut best: Option<(usize, f64)> = None;
        for (s, (&n, &c)) in norm.iter().zip(counts).enumerate() {
            if c > 0 {
                let gain = f64::from(c) * (f64::from(n + 1) / f64::from(n)).log2();
                if best.is_none_or(|(_, g)| gain > g) {
                    best = Some((s, gain));
                }
            }
        }
        let Some((s, _)) = best else { break };
        if let Some(n) = norm.get_mut(s) {
            *n += 1;
        }
        sum += 1;
    }
}

/// Bits a stream with `counts` costs under the distribution `norm` of accuracy `log`, or
/// `None` when it gives a present symbol no state.
pub(crate) fn cost_bits(counts: &[u32], norm: &[i16], log: u32) -> Option<f64> {
    let mut bits = 0.0;
    for (s, &c) in counts.iter().enumerate() {
        if c == 0 {
            continue;
        }
        let p = *norm.get(s)?;
        if p == 0 {
            return None;
        }
        let p = if p < 0 { 1.0 } else { f64::from(p) };
        bits += f64::from(c) * (f64::from(log) - p.log2());
    }
    Some(bits)
}

/// Writes the table description of `norm` (whose last symbol is present) at accuracy
/// `log` (RFC 8878 §4.1.1): the log less 5 in 4 bits, then each symbol's count plus one in
/// the fewest bits the probability still to spend allows, a zero count followed by 2-bit
/// repeat flags for the zeros after it, stopping once the whole table is spent.
pub(crate) fn write_description(norm: &[i16], log: u32, out: &mut Vec<u8>) {
    let mut w = BitWriter::new(out);
    w.add(u64::from(log - MIN_LOG), 4);
    let table = 1i32 << log;
    let mut remaining = table + 1;
    let mut threshold = table;
    let mut nb_bits = log + 1;
    let mut symbol = 0usize;
    let mut previous_zero = false;
    while symbol < norm.len() && remaining > 1 {
        if previous_zero {
            let mut start = symbol;
            while norm.get(symbol) == Some(&0) {
                symbol += 1;
            }
            if symbol == norm.len() {
                break;
            }
            while symbol >= start + 24 {
                start += 24;
                w.add(0xFFFF, 16);
            }
            while symbol >= start + 3 {
                start += 3;
                w.add(3, 2);
            }
            w.add((symbol - start) as u64, 2);
        }
        let mut count = i32::from(norm.get(symbol).copied().unwrap_or(0));
        symbol += 1;
        let max = (2 * threshold - 1) - remaining;
        remaining -= count.abs();
        count += 1;
        if count >= threshold {
            count += max;
        }
        let width = if count < max { nb_bits - 1 } else { nb_bits };
        w.add(count as u64, width);
        previous_zero = count == 1;
        while remaining < threshold {
            nb_bits -= 1;
            threshold >>= 1;
        }
    }
    debug_assert_eq!(remaining, 1, "the description spends the whole table");
    w.finish();
}

/// How one symbol moves the encoder's state (libzstd's `symbolTT`).
#[derive(Clone, Copy, Debug, Default)]
struct Transform {
    /// Added to the state: its top 16 bits are the bits to write.
    delta_nb_bits: u32,
    /// Where the symbol's states start in `next`, less its count.
    delta_find_state: i32,
}

/// An FSE encoding table: the inverse of the decoding table a decoder builds from the
/// same normalized counts (the symbol spread of RFC 8878 §4.1.1).
#[derive(Clone, Debug)]
pub(crate) struct Table {
    log: u32,
    /// Per symbol, in symbol order: the decoding states that decode it, plus the table
    /// size, each the state that follows encoding it.
    next: Vec<u16>,
    transform: Vec<Transform>,
}

impl Table {
    /// The table for `norm` at accuracy `log` (`norm` sums to `1 << log`, -1 counting 1).
    pub(crate) fn new(norm: &[i16], log: u32) -> Self {
        let size = 1usize << log;
        let mask = size - 1;
        let mut spread = vec![0u8; size];
        // "Less than 1" symbols take the last states, in symbol order from the top.
        let mut high = size;
        let mut cumulative = Vec::with_capacity(norm.len() + 1);
        cumulative.push(0u32);
        let mut at = 0u32;
        for (s, &n) in norm.iter().enumerate() {
            if n == -1 {
                high -= 1;
                if let Some(slot) = spread.get_mut(high) {
                    *slot = s as u8;
                }
                at += 1;
            } else {
                at += n.max(0) as u32;
            }
            cumulative.push(at);
        }
        // The rest are spread with the step every decoder uses.
        let step = (size >> 1) + (size >> 3) + 3;
        let mut pos = 0usize;
        for (s, &n) in norm.iter().enumerate() {
            for _ in 0..n.max(0) {
                if let Some(slot) = spread.get_mut(pos) {
                    *slot = s as u8;
                }
                pos = (pos + step) & mask;
                while pos >= high {
                    pos = (pos + step) & mask;
                }
            }
        }
        let mut next = vec![0u16; size];
        let mut fill = cumulative.clone();
        for (u, &s) in spread.iter().enumerate() {
            if let Some(f) = fill.get_mut(usize::from(s)) {
                if let Some(slot) = next.get_mut(*f as usize) {
                    *slot = (size + u) as u16;
                }
                *f += 1;
            }
        }
        let mut transform = vec![Transform::default(); norm.len()];
        let mut total = 0i32;
        let size32 = size as u32;
        for (t, &n) in transform.iter_mut().zip(norm) {
            *t = match n {
                0 => Transform {
                    delta_nb_bits: ((log + 1) << 16) - size32,
                    delta_find_state: 0,
                },
                -1 | 1 => {
                    let t = Transform {
                        delta_nb_bits: (log << 16) - size32,
                        delta_find_state: total - 1,
                    };
                    total += 1;
                    t
                }
                n => {
                    let n = n as u32;
                    let max_bits_out = log - highbit(n - 1);
                    let min_state_plus = n << max_bits_out;
                    let t = Transform {
                        delta_nb_bits: (max_bits_out << 16) - min_state_plus,
                        delta_find_state: total - n as i32,
                    };
                    total += n as i32;
                    t
                }
            };
        }
        Self {
            log,
            next,
            transform,
        }
    }

    /// The accuracy log.
    pub(crate) const fn log(&self) -> u32 {
        self.log
    }

    #[inline]
    fn transform(&self, symbol: u8) -> Transform {
        self.transform
            .get(usize::from(symbol))
            .copied()
            .unwrap_or_default()
    }

    #[inline]
    fn next(&self, at: i64) -> u32 {
        usize::try_from(at)
            .ok()
            .and_then(|i| self.next.get(i))
            .map_or(0, |&s| u32::from(s))
    }

    /// The state that starts a stream whose last symbol is `symbol`: its first decoding
    /// state, which writes no bits and reads at least one when the symbol is not the
    /// table's only one (what ends a two-state Huffman weight stream, RFC 8878 §4.2.1.2).
    #[inline]
    pub(crate) fn init(&self, symbol: u8) -> u32 {
        let t = self.transform(symbol);
        let nb = (t.delta_nb_bits + (1 << 15)) >> 16;
        let value = (nb << 16) - t.delta_nb_bits;
        self.next(i64::from(value >> nb) + i64::from(t.delta_find_state))
    }

    /// Encodes `symbol` from `state`: writes the bits a decoder reads to come back to it.
    #[inline]
    pub(crate) fn encode(&self, w: &mut BitWriter<'_>, state: &mut u32, symbol: u8) {
        let t = self.transform(symbol);
        let nb = (*state + t.delta_nb_bits) >> 16;
        w.add(u64::from(*state), nb);
        *state = self.next(i64::from(*state >> nb) + i64::from(t.delta_find_state));
    }

    /// Writes the final state: the decoder's first, `log` bits.
    #[inline]
    pub(crate) fn flush(&self, w: &mut BitWriter<'_>, state: u32) {
        w.add(u64::from(state), self.log);
    }
}

#[cfg(test)]
#[path = "fse_tests.rs"]
mod tests;
