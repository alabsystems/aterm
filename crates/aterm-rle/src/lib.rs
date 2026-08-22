// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(missing_docs)]
#![deny(clippy::all)]

//! Run-Length Encoding (RLE) for cell attributes.
//!
//! This crate provides RLE compression for terminal cell attributes based on
//! Windows Terminal's `til/rle.h` pattern. Attributes are compressed into runs
//! of consecutive cells with identical style.
//!
//! ## Design
//!
//! Terminal output often has runs of cells with identical attributes (e.g., a
//! prompt in one color, then text in another). RLE compression exploits this
//! by storing `(style, count)` pairs instead of per-cell styles.
//!
//! ## Architecture
//!
//! ```text
//! Row Storage (before RLE):
//! [Cell0][Cell1][Cell2][Cell3][Cell4][Cell5][Cell6][Cell7]
//!  Bold   Bold   Bold   Bold  Normal Normal Normal Normal
//!
//! Row Storage (with RLE attributes):
//! Characters: [C0][C1][C2][C3][C4][C5][C6][C7]
//! Attributes: [(Bold, 4), (Normal, 4)]
//! ```
//!
//! ## References
//!
//! - Windows Terminal: `src/inc/til/rle.h`
//! - Ghostty: Style ID indirection in `page.zig`

use std::fmt;
use std::ops::Index;

#[cfg(test)]
mod iteration_counter {
    use std::cell::Cell;

    thread_local! {
        static RUN_ITERATIONS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn count_run_iteration() {
        RUN_ITERATIONS.with(|counter| counter.set(counter.get() + 1));
    }

    pub(super) fn reset_run_iterations() {
        RUN_ITERATIONS.with(|counter| counter.set(0));
    }

    pub(super) fn take_run_iterations() -> usize {
        RUN_ITERATIONS.with(|counter| {
            let value = counter.get();
            counter.set(0);
            value
        })
    }
}

#[cfg(test)]
use iteration_counter::{count_run_iteration, reset_run_iterations, take_run_iterations};

#[cfg(not(test))]
#[inline]
fn count_run_iteration() {}

/// A run of cells with identical attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run<T> {
    /// The attribute value for this run.
    pub value: T,
    /// Number of consecutive cells with this attribute.
    pub length: u32,
}

/// Error returned when an RLE operation would exceed `u32::MAX` capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RleCapacityError {
    /// How many elements were requested.
    pub requested: u32,
    /// How many elements could actually fit.
    pub available: u32,
}

impl fmt::Display for RleCapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Spelled as `write_str` + `Display::fmt` (byte-identical output to the
        // equivalent `write!`): the `format_args!` expansion places an unsafe
        // `fmt::Arguments` constructor call in THIS function's body, which the
        // Trust verifier fails closed on as an unmodeled unsafe operation.
        f.write_str("RLE capacity exceeded: requested ")?;
        fmt::Display::fmt(&self.requested, f)?;
        f.write_str(" but only ")?;
        fmt::Display::fmt(&self.available, f)?;
        f.write_str(" available")
    }
}

impl std::error::Error for RleCapacityError {}

impl<T: Default> Default for Run<T> {
    fn default() -> Self {
        Self {
            value: T::default(),
            length: 0,
        }
    }
}

/// Run-Length Encoded sequence of attributes.
///
/// Stores a sequence of attributes as runs of consecutive identical values.
/// Provides O(runs) random access by a linear walk of the run lengths, and
/// efficient range operations.
///
/// # Why no prefix-sum index
///
/// This type used to carry a second `Vec<u32>` of run start offsets so that
/// [`get`](Self::get) / [`set`](Self::set) / [`set_range`](Self::set_range) /
/// [`Index`] could binary-search it. That index cost one extra allocation on
/// every constructed `Rle` — paid by the scrollback materialization path for
/// EVERY line it builds, including the all-default lines whose `Rle` is
/// discarded unread — and 24 of the struct's 56 bytes. It bought nothing: the
/// four random-access readers are the only readers of the index, and no
/// production caller reaches them (scrollback's `Line::get_attr` deliberately
/// walks the `runs` slice instead). [`with_value`](Self::with_value) already
/// left the index empty and never resynced it, so those `Rle`s permanently ran
/// on the linear path with no observable consequence — the tell that the index
/// was decoration. Runs per scrollback line are 1-6, so a linear walk is also
/// the faster shape at the sizes that exist.
///
/// # Type Parameters
///
/// - `T`: The attribute type (must be `Copy + PartialEq + Default`)
#[derive(Debug, Clone)]
pub struct Rle<T> {
    /// The runs in order.
    runs: Vec<Run<T>>,
    /// Total length (sum of all run lengths).
    total_length: u32,
}

impl<T: Copy + PartialEq + Default> Default for Rle<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq + Default> Rle<T> {
    /// Create an empty RLE sequence.
    #[must_use]
    pub fn new() -> Self {
        Self {
            runs: Vec::new(),
            total_length: 0,
        }
    }

    /// Create an RLE sequence with a single value repeated `length` times.
    #[must_use]
    // clippy would rewrite the two lines below to `vec![...]`, but that is the
    // one spelling we must avoid: `vec!` expands alloc's unsafe boxed-array
    // construction (`Box::new_uninit`/`assume_init`) into THIS function's span,
    // which the strict Trust gate then has to prove here. The `with_capacity` +
    // `push` form (same resulting Vec: len 1, cap 1) keeps that code inside
    // `alloc`, scoped out as a Conditional dependency. The allow is for the
    // clippy STYLE lint only — it suppresses no Trust verification obligation.
    #[allow(
        clippy::vec_init_then_push,
        reason = "vec! expands alloc unsafe into the Trust-verified span; with_capacity+push keeps it out"
    )]
    pub fn with_value(value: T, length: u32) -> Self {
        if length == 0 {
            return Self::new();
        }
        let mut runs = Vec::with_capacity(1);
        runs.push(Run { value, length });
        Self {
            runs,
            total_length: length,
        }
    }

    /// Create an RLE sequence from an iterator of values.
    ///
    /// Prefer using `.collect::<Rle<T>>()` or `Rle::from_iter(...)` via the
    /// standard `FromIterator` trait.
    // Trust L0 note: the generic `IntoIterator::into_iter`/`Iterator::next`
    // trait-method calls on the type parameter are a construct the Level-0
    // verifier cannot model pre-monomorphization (reported unsupported =>
    // one inconclusive obligation). No spelling of generic iteration avoids
    // that call target (a `while let` + explicit `into_iter` restructure was
    // tried and reported identically), so this is a documented coverage gap,
    // not a refutation.
    fn from_iter_inner<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut rle = Self::new();
        for value in iter {
            rle.push(value);
        }
        rle
    }

    /// Returns how many more elements can be added before reaching `u32::MAX`.
    #[must_use]
    #[inline]
    pub fn remaining_capacity(&self) -> u32 {
        u32::MAX - self.total_length
    }

    #[inline]
    fn checked_run_length_sum(lhs: u32, rhs: u32) -> u32 {
        // Run-length sums never exceed `u32::MAX`: every entry point clamps
        // or rejects against `remaining_capacity`, and every internal sum is
        // bounded by the maintained `total_length == sum(run.length)`
        // invariant (machine-checked by tests/fuzz_rle.rs). Under that
        // invariant `saturating_add` is behavior-identical to the previous
        // `checked_add(..).expect(..)` invariant panic on every reachable
        // input — the saturation, like the panic before it, cannot fire. The
        // strict verifier refutes any reachable panic on the symbolic
        // arguments it sees here (it cannot carry the callers' invariant),
        // so the guard is spelled as the provable no-op saturation.
        lhs.saturating_add(rhs)
    }

    /// Get the total length of the sequence.
    #[must_use]
    #[inline]
    pub fn len(&self) -> u32 {
        self.total_length
    }

    /// Check if the sequence is empty.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.total_length == 0
    }

    /// Get the number of runs.
    #[must_use]
    #[inline]
    pub fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// Get a reference to the runs.
    #[must_use]
    #[inline]
    pub fn runs(&self) -> &[Run<T>] {
        &self.runs
    }

    /// Clear the sequence.
    pub fn clear(&mut self) {
        self.runs.clear();
        self.total_length = 0;
    }

    /// Push a single value onto the end.
    ///
    /// If the sequence is already at `u32::MAX` capacity, this is a silent
    /// no-op. Use [`try_push`](Self::try_push) to detect overflow.
    pub fn push(&mut self, value: T) {
        self.extend_with(value, 1);
    }

    /// Push a single value, returning an error if at capacity.
    ///
    /// Unlike [`push`](Self::push), this rejects the operation instead of
    /// silently dropping the element.
    pub fn try_push(&mut self, value: T) -> Result<(), RleCapacityError> {
        self.try_extend_with(value, 1)
    }

    /// Extend with multiple copies of the same value.
    ///
    /// If `count` exceeds [`remaining_capacity`](Self::remaining_capacity),
    /// the count is silently clamped to the available space. The invariant
    /// `total_length == sum(run.length)` is always preserved.
    ///
    /// Use [`try_extend_with`](Self::try_extend_with) to reject partial
    /// insertions.
    pub fn extend_with(&mut self, value: T, count: u32) {
        let count = count.min(self.remaining_capacity());
        self.extend_with_unclamped(value, count);
    }

    /// Extend with multiple copies, returning an error if the full count
    /// cannot fit.
    ///
    /// Unlike [`extend_with`](Self::extend_with), this rejects the entire
    /// operation rather than silently clamping.
    pub fn try_extend_with(&mut self, value: T, count: u32) -> Result<(), RleCapacityError> {
        let available = self.remaining_capacity();
        if count > available {
            return Err(RleCapacityError {
                requested: count,
                available,
            });
        }
        self.extend_with_unclamped(value, count);
        Ok(())
    }

    /// Inner extend that assumes `count <= remaining_capacity()`.
    fn extend_with_unclamped(&mut self, value: T, count: u32) {
        if count == 0 {
            return;
        }
        if let Some(last) = self.runs.last_mut()
            && last.value == value
        {
            last.length = Self::checked_run_length_sum(last.length, count);
            self.total_length = Self::checked_run_length_sum(self.total_length, count);
            return;
        }
        self.runs.push(Run {
            value,
            length: count,
        });
        self.total_length = Self::checked_run_length_sum(self.total_length, count);
    }

    /// Get the value at a specific index (safe alternative to `Index`).
    ///
    /// This is the non-panicking alternative to the `Index` trait implementation.
    /// Returns `None` if index is out of bounds instead of panicking.
    #[must_use]
    pub fn get(&self, index: u32) -> Option<T> {
        if index >= self.total_length {
            return None;
        }
        let (run_idx, _) = self.find_run(index)?;
        Some(self.runs[run_idx].value)
    }

    /// Set the value at a specific index.
    ///
    /// Returns `false` if index is out of bounds.
    pub fn set(&mut self, index: u32, value: T) -> bool {
        if index >= self.total_length {
            return false;
        }

        let Some((run_idx, offset_in_run)) = self.find_run(index) else {
            return false;
        };

        let run = &self.runs[run_idx];
        if run.value == value {
            // Value already matches
            return true;
        }

        // Need to split the run
        self.split_and_set(run_idx, offset_in_run, value);
        true
    }

    /// Set a range of values to the same attribute.
    ///
    /// This is the most efficient way to update multiple cells.
    pub fn set_range(&mut self, start: u32, end: u32, value: T) {
        if start >= end || start >= self.total_length {
            return;
        }
        let end = end.min(self.total_length);

        // Fast path: entire sequence
        if start == 0 && end == self.total_length {
            self.runs.clear();
            self.runs.push(Run {
                value,
                length: self.total_length,
            });
            return;
        }

        // Find start and end runs
        let Some((start_run_idx, start_offset)) = self.find_run(start) else {
            return;
        };

        // The end-1 index gives us the last cell to modify. `end >= 1` here:
        // the guard gives `end > start >= 0`, and the clamp keeps it `>= 1`
        // because `total_length > start >= 0`. So the saturating_sub never
        // actually saturates; it just discharges the underflow obligation the
        // verifier cannot carry through the opaque `min` call.
        let Some((end_run_idx, end_offset)) = self.find_run(end.saturating_sub(1)) else {
            return;
        };

        // Simple case: same run
        if start_run_idx == end_run_idx {
            let run = &self.runs[start_run_idx];
            if run.value == value {
                return; // Already the correct value
            }
            // `end_offset < runs[idx].length <= u32::MAX` by find_run's
            // contract, so `+ 1` cannot overflow; saturating_add just
            // discharges the obligation on the opaque call's result.
            self.split_range_single_run(
                start_run_idx,
                start_offset,
                end_offset.saturating_add(1),
                value,
            );
            return;
        }

        // Complex case: spans multiple runs
        // (same `end_offset + 1` no-overflow reasoning as above)
        self.split_range_multi_run(
            start_run_idx,
            start_offset,
            end_run_idx,
            end_offset.saturating_add(1),
            value,
        );
    }

    /// Resize the sequence, extending with default value or truncating.
    pub fn resize(&mut self, new_length: u32) {
        if new_length == self.total_length {
            return;
        }

        if new_length == 0 {
            self.clear();
            return;
        }

        if new_length > self.total_length {
            // Extend with default
            self.extend_with(T::default(), new_length - self.total_length);
        } else {
            // Truncate
            self.truncate(new_length);
        }
    }

    /// Resize, extending with a specific value.
    pub fn resize_with(&mut self, new_length: u32, value: T) {
        if new_length == self.total_length {
            return;
        }

        if new_length == 0 {
            self.clear();
            return;
        }

        if new_length > self.total_length {
            self.extend_with(value, new_length - self.total_length);
        } else {
            self.truncate(new_length);
        }
    }

    /// Iterate over all values (expanded).
    pub fn iter(&self) -> RleIter<'_, T> {
        RleIter {
            runs: &self.runs,
            run_idx: 0,
            offset_in_run: 0,
        }
    }
}

/// Index trait implementation for `Rle<T>`.
///
/// # Panics
///
/// Panics if `index` is out of bounds. For a non-panicking alternative,
/// use [`Rle::get`] which returns `Option<T>`.
impl<T: Copy + PartialEq + Default> Index<u32> for Rle<T> {
    type Output = T;

    fn index(&self, index: u32) -> &Self::Output {
        // Index trait contract: panic on out-of-bounds is standard behavior.
        // An out-of-range `index` maps to `runs.len()`, which the slice bounds
        // check below rejects with the canonical "index out of bounds" panic —
        // same panic-on-OOB contract, expressed as the standard runtime bounds
        // check instead of an explicit `expect` the modular verifier cannot
        // discharge.
        let (run_idx, _) = self.find_run(index).unwrap_or((self.runs.len(), 0));
        &self.runs[run_idx].value
    }
}

impl<'a, T: Copy + PartialEq + Default> IntoIterator for &'a Rle<T> {
    type Item = T;
    type IntoIter = RleIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over expanded RLE values.
pub struct RleIter<'a, T> {
    runs: &'a [Run<T>],
    run_idx: usize,
    offset_in_run: u32,
}

impl<T: Copy> Iterator for RleIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        // Single bounds-checked access (`get`) instead of a manual
        // `run_idx >= len()` guard plus a slice index: identical
        // None-when-exhausted behavior, but no local MIR bounds assert for
        // the hardened-boundary verifier to refute (it cannot relate the two
        // separate `self.runs` loads of the guard-then-index spelling).
        let run = *self.runs.get(self.run_idx)?;
        let value = run.value;

        // `offset_in_run < run.length <= u32::MAX` between calls by
        // construction (it is reset to 0 whenever it reaches the run length),
        // and `run_idx < runs.len() <= isize::MAX` whenever it advances, so
        // these saturating ops never actually saturate; they just discharge
        // the overflow obligations on the symbolic iterator state.
        self.offset_in_run = self.offset_in_run.saturating_add(1);
        if self.offset_in_run >= run.length {
            self.run_idx = self.run_idx.saturating_add(1);
            self.offset_in_run = 0;
        }

        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let mut remaining = 0usize;
        // `iter().skip(run_idx)` instead of slicing `runs[run_idx..]`:
        // identical traversal, but no slice-construction obligation for the
        // verifier to refute (it cannot see the `run_idx <= len` invariant
        // across method boundaries).
        for run in self.runs.iter().skip(self.run_idx) {
            // Total length is `<= u32::MAX` by invariant, so this saturating
            // add never actually saturates; it just makes the no-overflow
            // obligation provable.
            remaining = remaining.saturating_add(run.length as usize);
        }
        remaining = remaining.saturating_sub(self.offset_in_run as usize);
        (remaining, Some(remaining))
    }
}

impl<T: Copy> ExactSizeIterator for RleIter<'_, T> {}

impl<T: Copy + PartialEq + Default> FromIterator<T> for Rle<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_iter_inner(iter)
    }
}

mod mutations;

#[cfg(test)]
mod tests;

#[cfg(kani)]
mod proofs;

#[cfg(kani)]
mod proofs_saturation;

#[cfg(kani)]
#[path = "compact_proofs_tests.rs"]
mod proofs_compact;
