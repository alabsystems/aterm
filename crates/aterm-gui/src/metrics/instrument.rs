// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The storage cells the `metrics` ledgers are built from.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A window statistic: [`Self::reset`] returns it to what it held before its first sample.
pub(crate) trait Windowed {
    fn reset(&self);

    /// Hand every cell under `path` to `f`, named by its field path (tests).
    #[cfg(test)]
    fn visit(&self, path: &str, f: &mut dyn FnMut(&str, &dyn Probe));
}

/// A test's handle on one cell: an instrument, or one part of a composite.
#[cfg(test)]
pub(crate) trait Probe {
    /// Write a sample no published value already holds: [`POKE`] where the cell holds a number, the first label past the empty one where it holds a label, `true` where it holds a flag.
    fn poke(&self);

    /// Whether it is in its reset state.
    fn is_empty(&self) -> bool;

    /// Whether a published field reads it.
    fn observes(&self) -> bool {
        true
    }
}

/// The value [`Probe::poke`] writes: 987.65 s, as no real reading or count is.
#[cfg(test)]
pub(crate) const POKE: u64 = 987_654_321_000;

/// Implement [`Windowed`] for an instrument whose inherent `reset` empties it.
macro_rules! windowed {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl Windowed for $ty {
                fn reset(&self) {
                    <$ty>::reset(self);
                }

                #[cfg(test)]
                fn visit(&self, path: &str, f: &mut dyn FnMut(&str, &dyn Probe)) {
                    f(path, self);
                }
            }
        )+
    };
}

windowed!(
    Counter, Last, LastFlag, LastLabel, Max, Histogram, Baselined, Stamp
);

impl<const N: usize> Windowed for Tally<N> {
    fn reset(&self) {
        Self::reset(self);
    }

    #[cfg(test)]
    fn visit(&self, path: &str, f: &mut dyn FnMut(&str, &dyn Probe)) {
        f(path, self);
    }
}

impl<T: Windowed, const N: usize> Windowed for [T; N] {
    fn reset(&self) {
        for cell in self {
            cell.reset();
        }
    }

    #[cfg(test)]
    fn visit(&self, path: &str, f: &mut dyn FnMut(&str, &dyn Probe)) {
        for (i, cell) in self.iter().enumerate() {
            cell.visit(&format!("{path}[{i}]"), f);
        }
    }
}

/// Declare a ledger: a struct of [`Windowed`] fields with a `const fn new` and a [`Windowed`] impl whose `reset` resets every field, in declaration order.
macro_rules! window_ledger {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$field_meta:meta])*
                $field_vis:vis $field:ident : $ty:ty $(= $init:expr)?
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $($(#[$field_meta])* $field_vis $field: $ty),+
        }

        impl $name {
            $vis const fn new() -> Self {
                Self {
                    $($field: $crate::metrics::instrument::window_ledger!(@init $ty $(, $init)?)),+
                }
            }
        }

        impl $crate::metrics::instrument::Windowed for $name {
            fn reset(&self) {
                $($crate::metrics::instrument::Windowed::reset(&self.$field);)+
            }

            #[cfg(test)]
            fn visit(
                &self,
                path: &str,
                f: &mut dyn FnMut(&str, &dyn $crate::metrics::instrument::Probe),
            ) {
                $(
                    $crate::metrics::instrument::Windowed::visit(
                        &self.$field,
                        &format!("{path}.{}", stringify!($field)),
                        f,
                    );
                )+
            }
        }
    };
    (@init $ty:ty) => {
        <$ty>::new()
    };
    (@init $ty:ty, $init:expr) => {
        $init
    };
}

pub(crate) use window_ledger;

/// Events since the reset.
pub(crate) struct Counter(AtomicU64);

impl Counter {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub(crate) fn inc(&self) {
        self.add(1);
    }

    pub(crate) fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// [`Self::inc`] with Release: what the writer stored before it is visible to a reader whose [`Self::get_acquire`] sees the new count.
    pub(crate) fn inc_release(&self) {
        self.0.fetch_add(1, Ordering::Release);
    }

    #[must_use]
    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// [`Self::get`] with Acquire, the reader's half of [`Self::inc_release`].
    #[must_use]
    pub(crate) fn get_acquire(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    pub(crate) fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Probe for Counter {
    fn poke(&self) {
        self.add(POKE);
    }

    fn is_empty(&self) -> bool {
        self.get() == 0
    }
}

/// The most recent sample of the window; 0 until one lands.
pub(crate) struct Last(AtomicU64);

impl Last {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub(crate) fn set(&self, v: u64) {
        self.0.store(v, Ordering::Relaxed);
    }

    #[must_use]
    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn reset(&self) {
        self.set(0);
    }
}

#[cfg(test)]
impl Probe for Last {
    fn poke(&self) {
        self.set(POKE);
    }

    fn is_empty(&self) -> bool {
        self.get() == 0
    }
}

/// The most recent flag of the window; `false` until one lands.
pub(crate) struct LastFlag(AtomicBool);

impl LastFlag {
    pub(crate) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    pub(crate) fn set(&self, v: bool) {
        self.0.store(v, Ordering::Relaxed);
    }

    #[must_use]
    pub(crate) fn get(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn reset(&self) {
        self.set(false);
    }
}

#[cfg(test)]
impl Probe for LastFlag {
    fn poke(&self) {
        self.set(true);
    }

    fn is_empty(&self) -> bool {
        !self.get()
    }
}

/// The most recent label of the window, as its `label_enum!` index; index 0 (each table's empty label) until one lands.
pub(crate) struct LastLabel(AtomicU64);

impl LastLabel {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub(crate) fn set(&self, index: usize) {
        self.0.store(index as u64, Ordering::Relaxed);
    }

    /// The stored index, for the table's own decoder.
    #[must_use]
    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn reset(&self) {
        self.set(0);
    }
}

#[cfg(test)]
impl Probe for LastLabel {
    fn poke(&self) {
        self.set(1);
    }

    fn is_empty(&self) -> bool {
        self.get() == 0
    }
}

/// The worst sample of the window.
pub(crate) struct Max(AtomicU64);

impl Max {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Fold `v` in; whether it raised the max.
    pub(crate) fn record(&self, v: u64) -> bool {
        v > self.0.load(Ordering::Relaxed) && self.0.fetch_max(v, Ordering::Relaxed) < v
    }

    #[must_use]
    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    pub(crate) fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Probe for Max {
    fn poke(&self) {
        let _ = self.record(POKE);
    }

    fn is_empty(&self) -> bool {
        self.get() == 0
    }
}

/// The most recent and the worst sample of the window.
pub(crate) struct Peak {
    last: Last,
    max: Max,
}

impl Peak {
    pub(crate) const fn new() -> Self {
        Self {
            last: Last::new(),
            max: Max::new(),
        }
    }

    /// Book `v` as the last sample and fold it into the max; whether it raised the max.
    pub(crate) fn record(&self, v: u64) -> bool {
        self.last.set(v);
        self.max.record(v)
    }

    #[must_use]
    pub(crate) fn last(&self) -> u64 {
        self.last.get()
    }

    #[must_use]
    pub(crate) fn max(&self) -> u64 {
        self.max.get()
    }

    pub(crate) fn reset(&self) {
        self.last.reset();
        self.max.reset();
    }
}

impl Windowed for Peak {
    fn reset(&self) {
        Self::reset(self);
    }

    #[cfg(test)]
    fn visit(&self, path: &str, f: &mut dyn FnMut(&str, &dyn Probe)) {
        f(&format!("{path}#last"), &self.last);
        f(&format!("{path}#max"), &self.max);
    }
}

/// The worst sample of the window, the label index of what produced it, and when (`now_ns`).
pub(crate) struct MaxBy {
    max: Max,
    by: LastLabel,
    at: Last,
}

impl MaxBy {
    pub(crate) const fn new() -> Self {
        Self {
            max: Max::new(),
            by: LastLabel::new(),
            at: Last::new(),
        }
    }

    /// Fold `v` in, produced by label `by` at `at`.
    pub(crate) fn record(&self, v: u64, by: usize, at: u64) {
        if self.max.record(v) {
            self.by.set(by);
            self.at.set(at);
        }
    }

    #[must_use]
    pub(crate) fn max(&self) -> u64 {
        self.max.get()
    }

    /// The producer's index, for its table's own decoder.
    #[must_use]
    pub(crate) fn by(&self) -> u64 {
        self.by.get()
    }

    #[must_use]
    pub(crate) fn at(&self) -> u64 {
        self.at.get()
    }

    pub(crate) fn reset(&self) {
        self.max.reset();
        self.by.reset();
        self.at.reset();
    }
}

impl Windowed for MaxBy {
    fn reset(&self) {
        Self::reset(self);
    }

    #[cfg(test)]
    fn visit(&self, path: &str, f: &mut dyn FnMut(&str, &dyn Probe)) {
        f(&format!("{path}#max"), &self.max);
        f(&format!("{path}#by"), &self.by);
        f(&format!("{path}#at"), &self.at);
    }
}

const H_BUCKETS: usize = 168;
/// Nanosecond buckets below 1024 ns, then 1024 per octave through 2^36 ns.
/// The upper edge overestimates by at most max(1 ns, value/1024).
pub(crate) const LATENCY_BUCKETS: usize = 27 * 1024;

/// Fixed-bucket log-linear histogram with atomic counters.
pub(crate) type Histogram = HistogramBins<H_BUCKETS>;
pub(crate) type LatencyDist = Distribution<LATENCY_BUCKETS>;

pub(crate) struct HistogramBins<const N: usize> {
    coarse: [AtomicU64; H_BUCKETS],
    // Keep unused latency ledgers and local Metrics values small. Allocation is
    // once per used latency distribution, outside warmed measurement regions.
    latency: std::sync::OnceLock<Box<[AtomicU64]>>,
}

impl<const N: usize> HistogramBins<N> {
    const SUB_BITS: u32 = if N == LATENCY_BUCKETS { 10 } else { 3 };
    const MIN_SHIFT: u32 = if N == LATENCY_BUCKETS { 10 } else { 16 };

    pub(crate) const fn new() -> Self {
        assert!(N == H_BUCKETS || N == LATENCY_BUCKETS);
        Self {
            coarse: [const { AtomicU64::new(0) }; H_BUCKETS],
            latency: std::sync::OnceLock::new(),
        }
    }

    fn index(v_ns: u64) -> usize {
        if v_ns < (1 << Self::MIN_SHIFT) {
            return (v_ns >> (Self::MIN_SHIFT - Self::SUB_BITS)) as usize;
        }
        let msb = 63 - v_ns.leading_zeros();
        let octave = (msb - Self::MIN_SHIFT + 1) as usize;
        let sub = ((v_ns >> (msb - Self::SUB_BITS)) & ((1 << Self::SUB_BITS) - 1)) as usize;
        ((octave << Self::SUB_BITS) + sub).min(N - 1)
    }

    /// The exclusive upper edge of bucket `idx` in ns. Below 2^36 ns this
    /// keeps percentiles conservative; larger samples saturate in the last
    /// bucket and have no relative-error guarantee.
    fn upper_edge(idx: usize) -> u64 {
        let sub_count = 1u64 << Self::SUB_BITS;
        let i = idx as u64;
        if idx < (1 << Self::SUB_BITS) {
            return (i + 1) << (Self::MIN_SHIFT - Self::SUB_BITS);
        }
        let octave = i >> Self::SUB_BITS;
        let sub = i & (sub_count - 1);
        let msb = Self::MIN_SHIFT + u32::try_from(octave).unwrap_or(u32::MAX) - 1;
        (1u64 << msb) + ((sub + 1) << (msb - Self::SUB_BITS))
    }

    pub(crate) fn record(&self, v_ns: u64) {
        let buckets: &[AtomicU64] = if N == H_BUCKETS {
            &self.coarse
        } else {
            self.latency
                .get_or_init(|| (0..N).map(|_| AtomicU64::new(0)).collect())
        };
        buckets[Self::index(v_ns)].fetch_add(1, Ordering::Relaxed);
    }

    fn existing(&self) -> &[AtomicU64] {
        if N == H_BUCKETS {
            &self.coarse
        } else {
            self.latency.get().map_or(&[], |buckets| buckets)
        }
    }

    pub(crate) fn reset(&self) {
        for b in self.existing() {
            b.store(0, Ordering::Relaxed);
        }
    }

    /// One copy of the buckets.
    #[must_use]
    pub(crate) fn copy_out(&self) -> HistCopy {
        let counts: Box<[u64]> = self
            .existing()
            .iter()
            .map(|cell| cell.load(Ordering::Relaxed))
            .collect();
        HistCopy {
            n: counts.iter().sum(),
            counts,
            upper_edge: Self::upper_edge,
        }
    }
}

#[cfg(test)]
impl<const N: usize> Probe for HistogramBins<N> {
    fn poke(&self) {
        self.record(POKE);
    }

    fn is_empty(&self) -> bool {
        self.copy_out().n() == 0
    }
}

/// The bucket counts one [`Histogram::copy_out`] took, and their sum.
#[derive(Clone, Debug)]
pub(crate) struct HistCopy {
    counts: Box<[u64]>,
    n: u64,
    upper_edge: fn(usize) -> u64,
}

impl HistCopy {
    #[cfg(test)]
    fn from_counts<const N: usize>(counts: [u64; N]) -> Self {
        Self {
            n: counts.iter().sum(),
            counts: Box::new(counts),
            upper_edge: HistogramBins::<N>::upper_edge,
        }
    }

    /// Samples in this copy: the sum of its buckets.
    #[must_use]
    pub(crate) const fn n(&self) -> u64 {
        self.n
    }

    /// The value at quantile `q` (0 < q ≤ 1), as the containing bucket's upper edge in ns — conservative by construction.
    #[must_use]
    pub(crate) fn percentile(&self, q: f64) -> Option<u64> {
        if self.n == 0 {
            return None;
        }
        let target = ((q * self.n as f64).ceil() as u64).clamp(1, self.n);
        let mut seen = 0u64;
        self.counts
            .iter()
            .position(|&c| {
                seen += c;
                seen >= target
            })
            .map(self.upper_edge)
    }
}

/// A distribution with its last and worst sample: every sample lands in all three, so a published max and its percentiles describe the same samples.
pub(crate) type Dist = Distribution<H_BUCKETS>;

pub(crate) struct Distribution<const N: usize> {
    hist: HistogramBins<N>,
    peak: Peak,
}

impl<const N: usize> Distribution<N> {
    pub(crate) const fn new() -> Self {
        Self {
            hist: HistogramBins::new(),
            peak: Peak::new(),
        }
    }

    /// Book one sample; whether it raised the max.
    pub(crate) fn record(&self, v: u64) -> bool {
        self.hist.record(v);
        self.peak.record(v)
    }

    #[must_use]
    pub(crate) fn last(&self) -> u64 {
        self.peak.last()
    }

    #[must_use]
    pub(crate) fn max(&self) -> u64 {
        self.peak.max()
    }

    /// One copy of the distribution ([`Histogram::copy_out`]).
    #[must_use]
    pub(crate) fn copy_out(&self) -> HistCopy {
        self.hist.copy_out()
    }

    pub(crate) fn reset(&self) {
        self.hist.reset();
        self.peak.reset();
    }
}

impl<const N: usize> Windowed for Distribution<N> {
    fn reset(&self) {
        Self::reset(self);
    }

    #[cfg(test)]
    fn visit(&self, path: &str, f: &mut dyn FnMut(&str, &dyn Probe)) {
        f(&format!("{path}#hist"), &self.hist);
        self.peak.visit(path, f);
    }
}

/// One count per label of an `N`-label table, indexed by the label's index.
pub(crate) struct Tally<const N: usize>([AtomicU64; N]);

impl<const N: usize> Tally<N> {
    pub(crate) const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; N])
    }

    /// Count one under `index`.
    pub(crate) fn inc(&self, index: usize) {
        if let Some(slot) = self.0.get(index) {
            slot.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The count under `index`; 0 past the table.
    #[must_use]
    pub(crate) fn get(&self, index: usize) -> u64 {
        self.0
            .get(index)
            .map_or(0, |slot| slot.load(Ordering::Relaxed))
    }

    /// `(label, count)` for each nonzero count, in the order `labels` names the `(index, label)` pairs: the shape every sparse per-label field publishes, so a healthy instance prints a pair or two and a sick one puts its culprit first in sight.
    #[must_use]
    pub(crate) fn sparse(
        &self,
        labels: impl IntoIterator<Item = (usize, &'static str)>,
    ) -> Vec<(&'static str, u64)> {
        labels
            .into_iter()
            .filter_map(|(index, label)| {
                let count = self.get(index);
                (count != 0).then_some((label, count))
            })
            .collect()
    }

    pub(crate) fn reset(&self) {
        for slot in &self.0 {
            slot.store(0, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
impl<const N: usize> Probe for Tally<N> {
    fn poke(&self) {
        for slot in &self.0 {
            slot.fetch_add(POKE, Ordering::Relaxed);
        }
    }

    fn is_empty(&self) -> bool {
        (0..N).all(|index| self.get(index) == 0)
    }
}

/// A count kept as a running total someone else owns, published as the part since the reset: `reset` rebases instead of zeroing, because the total is not ours to clear.
pub(crate) struct Baselined {
    total: AtomicU64,
    base: AtomicU64,
}

impl Baselined {
    pub(crate) const fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            base: AtomicU64::new(0),
        }
    }

    pub(crate) fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    /// Store `total` unless the total already holds a higher one: for a total that never restarts, where a lower sample is an older one.
    pub(crate) fn raise_total(&self, total: u64) {
        self.total.fetch_max(total, Ordering::Relaxed);
    }

    #[must_use]
    pub(crate) fn since_reset(&self) -> u64 {
        self.total
            .load(Ordering::Relaxed)
            .saturating_sub(self.base.load(Ordering::Relaxed))
    }

    pub(crate) fn reset(&self) {
        self.base
            .store(self.total.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Probe for Baselined {
    fn poke(&self) {
        self.total.fetch_add(POKE, Ordering::Relaxed);
    }

    fn is_empty(&self) -> bool {
        self.since_reset() == 0
    }
}

/// The opening edge of a slice still waiting for its close (`now_ns`; 0 = none).
pub(crate) struct Stamp(AtomicU64);

impl Stamp {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Open at `at` unless an edge is already open.
    pub(crate) fn arm(&self, at: u64) {
        let _ = self
            .0
            .compare_exchange(0, at, Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Close: the open edge, if any, leaving none.
    pub(crate) fn take(&self) -> Option<u64> {
        match self.0.swap(0, Ordering::Relaxed) {
            0 => None,
            at => Some(at),
        }
    }

    pub(crate) fn reset(&self) {
        self.0.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Probe for Stamp {
    fn poke(&self) {
        self.arm(POKE);
    }

    fn is_empty(&self) -> bool {
        self.0.load(Ordering::Relaxed) == 0
    }

    fn observes(&self) -> bool {
        false
    }
}

#[cfg(test)]
#[path = "instrument_tests.rs"]
mod tests;
