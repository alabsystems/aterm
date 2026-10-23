// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![warn(missing_docs)]
#![cfg_attr(
    trust_verify,
    feature(custom_inner_attributes, register_tool),
    register_tool(trust)
)]

//! Run-Length Encoding (RLE) for cell attributes.

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
const fn count_run_iteration() {}

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
        f.write_str("RLE capacity exceeded: requested ")?;
        fmt::Display::fmt(&self.requested, f)?;
        f.write_str(" but only ")?;
        fmt::Display::fmt(&self.available, f)?;
        f.write_str(" available")
    }
}

impl<T> Run<T> {
    /// Construct a run of `length` consecutive cells with attribute `value`.
    #[must_use]
    #[inline]
    pub const fn new(value: T, length: u32) -> Self {
        Self { value, length }
    }

    /// Return `true` iff this run has zero length.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// Return `true` iff `offset < self.length`.
    #[must_use]
    #[inline]
    pub const fn contains_offset(&self, offset: u32) -> bool {
        offset < self.length
    }
}

impl RleCapacityError {
    /// Number of elements beyond `available` that were requested (`requested.saturating_sub(available)`).
    #[must_use]
    #[inline]
    pub const fn overflow_amount(self) -> u32 {
        self.requested.saturating_sub(self.available)
    }

    /// Return `true` iff zero capacity remained (`available == 0`).
    #[must_use]
    #[inline]
    pub const fn is_full(self) -> bool {
        self.available == 0
    }
}

/// Remaining elements before reaching `u32::MAX` (`u32::MAX - total_length`).
#[must_use]
#[inline]
pub const fn remaining_rle_capacity(total_length: u32) -> u32 {
    u32::MAX - total_length
}

/// Clamp `count` to the remaining capacity at `total_length`.
#[must_use]
#[inline]
pub const fn clamp_rle_extend_count(total_length: u32, count: u32) -> u32 {
    let avail = u32::MAX - total_length;
    if count > avail { avail } else { count }
}

/// Checked total length after extending `total_length` by `count` elements.
///
/// # Errors
/// Returns [`RleCapacityError`] if `count > u32::MAX - total_length`.
#[inline]
pub const fn checked_rle_extend_total(
    total_length: u32,
    count: u32,
) -> Result<u32, RleCapacityError> {
    let available = u32::MAX - total_length;
    if count > available {
        Err(RleCapacityError {
            requested: count,
            available,
        })
    } else {
        Ok(total_length + count)
    }
}

/// Clamp `[start, end)` to `[0, total_length)`, returning `None` when the clamped range is empty.
#[must_use]
#[inline]
pub const fn clamp_rle_range(start: u32, end: u32, total_length: u32) -> Option<(u32, u32)> {
    if start >= end || start >= total_length {
        return None;
    }
    let clamped_end = if end > total_length {
        total_length
    } else {
        end
    };
    Some((start, clamped_end))
}

/// Compute `(prefix_len, mid_len, suffix_len)` when splitting a single run of length `run_len` across `[start_offset, end_offset_exclusive)`.
#[must_use]
#[inline]
pub const fn single_run_split_lengths(
    run_len: u32,
    start_offset: u32,
    end_offset_exclusive: u32,
) -> Option<(u32, u32, u32)> {
    if start_offset >= end_offset_exclusive || end_offset_exclusive > run_len {
        return None;
    }
    let prefix = start_offset;
    let mid = end_offset_exclusive - start_offset;
    let suffix = run_len - end_offset_exclusive;
    Some((prefix, mid, suffix))
}

/// Compute `(prefix_len, 1, suffix_len)` when splitting a run of length `run_len` at `offset_in_run`.
#[must_use]
#[inline]
pub const fn point_split_lengths(run_len: u32, offset_in_run: u32) -> Option<(u32, u32, u32)> {
    if offset_in_run >= run_len {
        return None;
    }
    let prefix = offset_in_run;
    let suffix = (run_len - 1) - offset_in_run;
    Some((prefix, 1, suffix))
}

/// Compute `(prefix_keep_in_start, suffix_keep_in_end)` across a start run (`start_run_len`) and end run (`end_run_len`).
#[must_use]
#[inline]
pub const fn multi_run_edge_lengths(
    start_run_len: u32,
    start_offset: u32,
    end_run_len: u32,
    end_offset_exclusive: u32,
) -> Option<(u32, u32)> {
    if start_offset >= start_run_len
        || end_offset_exclusive == 0
        || end_offset_exclusive > end_run_len
    {
        return None;
    }
    Some((start_offset, end_run_len - end_offset_exclusive))
}

/// Encode a `(style_id, length)` run pair into an 8-byte little-endian record.
#[must_use]
#[inline]
pub const fn encode_run_pair_le(style_id: u32, length: u32) -> [u8; 8] {
    [
        style_id as u8,
        (style_id >> 8) as u8,
        (style_id >> 16) as u8,
        (style_id >> 24) as u8,
        length as u8,
        (length >> 8) as u8,
        (length >> 16) as u8,
        (length >> 24) as u8,
    ]
}

/// Decode an 8-byte little-endian `(style_id, length)` run pair.
#[must_use]
#[inline]
pub const fn decode_run_pair_le(bytes: [u8; 8]) -> (u32, u32) {
    let style_id = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let length = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    (style_id, length)
}

/// Encode a 3-word RLE span record `(start_offset, run_len, style_id)` into 12 little-endian bytes.
#[must_use]
#[inline]
pub const fn encode_rle_span_triple_le(start_offset: u32, run_len: u32, style_id: u32) -> [u8; 12] {
    [
        start_offset as u8,
        (start_offset >> 8) as u8,
        (start_offset >> 16) as u8,
        (start_offset >> 24) as u8,
        run_len as u8,
        (run_len >> 8) as u8,
        (run_len >> 16) as u8,
        (run_len >> 24) as u8,
        style_id as u8,
        (style_id >> 8) as u8,
        (style_id >> 16) as u8,
        (style_id >> 24) as u8,
    ]
}

/// Decode 12 little-endian bytes into a 3-word RLE span record `(start_offset, run_len, style_id)`.
#[must_use]
#[inline]
pub const fn decode_rle_span_triple_le(bytes: [u8; 12]) -> (u32, u32, u32) {
    let start_offset = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let run_len = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let style_id = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    (start_offset, run_len, style_id)
}

impl std::error::Error for RleCapacityError {}

impl<T: Default> Default for Run<T> {
    #[inline]
    fn default() -> Self {
        Self {
            value: T::default(),
            length: 0,
        }
    }
}

/// Run-Length Encoded sequence of attributes.
#[derive(Debug, Clone)]
pub struct Rle<T> {
    /// The runs in order.
    runs: Vec<Run<T>>,
    /// Total length (sum of all run lengths).
    total_length: u32,
}

impl<T: Copy + PartialEq + Default> Default for Rle<T> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + PartialEq + Default> Rle<T> {
    /// Create an empty RLE sequence.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self {
            runs: Vec::new(),
            total_length: 0,
        }
    }

    /// Create an RLE sequence with a single value repeated `length` times.
    #[must_use]
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
    pub const fn remaining_capacity(&self) -> u32 {
        u32::MAX - self.total_length
    }

    #[inline]
    const fn checked_run_length_sum(lhs: u32, rhs: u32) -> u32 {
        // `saturating_add` discharges the overflow obligation for the verifier;
        // `total_length <= u32::MAX` guarantees it never saturates.
        lhs.saturating_add(rhs)
    }

    /// Get the total length of the sequence.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> u32 {
        self.total_length
    }

    /// Check if the sequence is empty.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.total_length == 0
    }

    /// Get the number of runs.
    #[must_use]
    #[inline]
    pub const fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// Get a reference to the runs.
    #[must_use]
    #[inline]
    pub fn runs(&self) -> &[Run<T>] {
        &self.runs
    }

    /// Clear the sequence.
    #[inline]
    pub fn clear(&mut self) {
        self.runs.clear();
        self.total_length = 0;
    }

    /// Push a single value onto the end.
    #[inline]
    pub fn push(&mut self, value: T) {
        self.extend_with(value, 1);
    }

    /// Push a single value, returning an error if at capacity.
    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<(), RleCapacityError> {
        self.try_extend_with(value, 1)
    }

    /// Extend with multiple copies of the same value.
    #[inline]
    pub fn extend_with(&mut self, value: T, count: u32) {
        let count = count.min(self.remaining_capacity());
        self.extend_with_unclamped(value, count);
    }

    /// Extend with multiple copies, returning an error if the full count cannot fit.
    #[inline]
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
    #[inline]
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

    /// Get the value at `index`, or `None` if out of bounds (the non-panicking form of `Index`).
    #[must_use]
    #[inline]
    pub fn get(&self, index: u32) -> Option<T> {
        if index >= self.total_length {
            return None;
        }
        let (run_idx, _) = self.find_run(index)?;
        Some(self.runs[run_idx].value)
    }

    /// Set the value at a specific index.
    pub fn set(&mut self, index: u32, value: T) -> bool {
        if index >= self.total_length {
            return false;
        }

        let Some((run_idx, offset_in_run)) = self.find_run(index) else {
            return false;
        };

        if self.runs[run_idx].value == value {
            return true;
        }
        self.split_and_set(run_idx, offset_in_run, value);
        true
    }

    /// Set `start..end` (clamped to the length) to `value`.
    pub fn set_range(&mut self, start: u32, end: u32, value: T) {
        if start >= end || start >= self.total_length {
            return;
        }
        let end = end.min(self.total_length);

        if start == 0 && end == self.total_length {
            self.runs.clear();
            self.runs.push(Run {
                value,
                length: self.total_length,
            });
            return;
        }

        let Some((start_run_idx, start_offset)) = self.find_run(start) else {
            return;
        };

        let Some((end_run_idx, end_offset)) = self.find_run(end.saturating_sub(1)) else {
            return;
        };

        if start_run_idx == end_run_idx {
            let run = &self.runs[start_run_idx];
            if run.value == value {
                return;
            }
            self.split_range_single_run(
                start_run_idx,
                start_offset,
                end_offset.saturating_add(1),
                value,
            );
            return;
        }

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
            self.extend_with(T::default(), new_length - self.total_length);
        } else {
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
    #[inline]
    pub fn iter(&self) -> RleIter<'_, T> {
        RleIter {
            runs: &self.runs,
            run_idx: 0,
            offset_in_run: 0,
        }
    }
}

/// Index trait implementation for `Rle<T>`.
/// # Panics
///
/// Panics if `index` is out of bounds. For a non-panicking alternative,
/// use [`Rle::get`] which returns `Option<T>`.
impl<T: Copy + PartialEq + Default> Index<u32> for Rle<T> {
    type Output = T;

    #[inline]
    fn index(&self, index: u32) -> &Self::Output {
        let (run_idx, _) = self.find_run(index).unwrap_or((self.runs.len(), 0));
        &self.runs[run_idx].value
    }
}

impl<'a, T: Copy + PartialEq + Default> IntoIterator for &'a Rle<T> {
    type Item = T;
    type IntoIter = RleIter<'a, T>;

    #[inline]
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

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let run = *self.runs.get(self.run_idx)?;
        let value = run.value;

        self.offset_in_run = self.offset_in_run.saturating_add(1);
        if self.offset_in_run >= run.length {
            self.run_idx = self.run_idx.saturating_add(1);
            self.offset_in_run = 0;
        }

        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let mut remaining = 0usize;
        for run in self.runs.iter().skip(self.run_idx) {
            remaining = remaining.saturating_add(run.length as usize);
        }
        remaining = remaining.saturating_sub(self.offset_in_run as usize);
        (remaining, Some(remaining))
    }
}

impl<T: Copy> ExactSizeIterator for RleIter<'_, T> {}

impl<T: Copy + PartialEq + Default> FromIterator<T> for Rle<T> {
    #[inline]
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_iter_inner(iter)
    }
}

mod mutations;

#[cfg(test)]
mod tests;
