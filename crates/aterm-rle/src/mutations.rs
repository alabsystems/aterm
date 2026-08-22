// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Internal mutation helpers for RLE sequences.
//!
//! Contains the split, compact, find, and truncate operations used by
//! the public `set`, `set_range`, and `resize` methods in `lib.rs`.

use super::{Rle, Run, count_run_iteration};

impl<T: Copy + PartialEq + Default> Rle<T> {
    /// Truncate to a specific length.
    pub(super) fn truncate(&mut self, new_length: u32) {
        if new_length >= self.total_length {
            return;
        }

        if new_length == 0 {
            self.clear();
            return;
        }

        // Find the run containing the new end
        let mut accumulated = 0u32;
        for (i, run) in self.runs.iter_mut().enumerate() {
            let next_accumulated = Self::checked_run_length_sum(accumulated, run.length);
            if next_accumulated >= new_length {
                // This run contains the new end. `accumulated < new_length`
                // holds here (previous iterations returned otherwise), but the
                // verifier cannot carry that loop invariant — saturating_sub
                // is identical when the invariant holds.
                let keep_in_run = new_length.saturating_sub(accumulated);
                run.length = keep_in_run;
                // `i < runs.len() <= isize::MAX`, so `i + 1` never actually
                // saturates; this just discharges the no-overflow obligation.
                self.runs.truncate(i.saturating_add(1));
                self.total_length = new_length;
                return;
            }
            accumulated = next_accumulated;
        }
    }

    /// Find the run containing an index.
    ///
    /// O(runs) linear walk of the run lengths. Returns
    /// `(run_index, offset_within_run)`.
    ///
    /// This used to prefer a binary search over a cached prefix-sum index. The
    /// index was deleted (see the `Rle` type docs): it cost an allocation on
    /// every constructed `Rle` on the scrollback materialization path, no
    /// production caller reaches any of the four readers that consulted it, and
    /// `with_value` already produced `Rle`s that ran here permanently. Runs per
    /// line are 1-6, so the walk is short by construction — and it is the shape
    /// the Kani harnesses already prove (`proofs_saturation.rs` drove exactly
    /// this path).
    pub(super) fn find_run(&self, index: u32) -> Option<(usize, u32)> {
        self.find_run_linear(index)
    }

    /// Linear scan over the run lengths.
    fn find_run_linear(&self, index: u32) -> Option<(usize, u32)> {
        let mut accumulated = 0u32;
        for (i, run) in self.runs.iter().enumerate() {
            count_run_iteration();
            let next_accumulated = Self::checked_run_length_sum(accumulated, run.length);
            if next_accumulated > index {
                // `accumulated <= index` holds here (the loop advances only
                // while `next_accumulated <= index`), but the verifier cannot
                // carry that loop invariant — saturating_sub is identical
                // when it holds.
                return Some((i, index.saturating_sub(accumulated)));
            }
            accumulated = next_accumulated;
        }
        None
    }

    /// Split a single run to set a value at a specific offset.
    pub(super) fn split_and_set(&mut self, run_idx: usize, offset: u32, value: T) {
        let run = &self.runs[run_idx];
        let run_len = run.length;
        let old_value = run.value;

        if run_len == 1 {
            // Simple case: run of length 1
            self.runs[run_idx].value = value;
            self.compact_around(run_idx);
            return;
        }

        // Callers pass `offset < runs[run_idx].length` (find_run's contract),
        // so `run_len >= 2` past the length-1 early return, and
        // `run_idx < runs.len() <= isize::MAX`. The saturating ops below
        // therefore never actually saturate — they only discharge the
        // overflow obligations the verifier cannot derive from the opaque
        // caller contract.
        if offset == 0 {
            // At start of run (`run_len - 1 >= 1`)
            self.runs[run_idx].length = run_len.saturating_sub(1);
            self.runs.insert(run_idx, Run { value, length: 1 });
            self.compact_around(run_idx);
        } else if offset == run_len.saturating_sub(1) {
            // At end of run (`run_len - 1 >= 1`)
            self.runs[run_idx].length = run_len.saturating_sub(1);
            let next_idx = run_idx.saturating_add(1);
            self.runs.insert(next_idx, Run { value, length: 1 });
            self.compact_around(next_idx);
        } else {
            // In middle - split into 3 (`1 <= offset <= run_len - 2` here,
            // so `after_len = run_len - offset - 1 >= 1`)
            let after_len = run_len.saturating_sub(offset).saturating_sub(1);
            self.runs[run_idx].length = offset;
            self.runs
                .insert(run_idx.saturating_add(1), Run { value, length: 1 });
            self.runs.insert(
                run_idx.saturating_add(2),
                Run {
                    value: old_value,
                    length: after_len,
                },
            );
        }
    }

    /// Split a single run to set a range to a new value.
    pub(super) fn split_range_single_run(
        &mut self,
        run_idx: usize,
        start_offset: u32,
        end_offset: u32,
        value: T,
    ) {
        let run = &self.runs[run_idx];
        let run_len = run.length;
        let old_value = run.value;
        // `start_offset < end_offset` at the (only) call site — set_range
        // passes `end_offset = same-run offset of the last cell + 1`, which
        // is `> start_offset` — so this never actually saturates; it just
        // discharges the underflow obligation on the symbolic arguments.
        let range_len = end_offset.saturating_sub(start_offset);

        if start_offset == 0 && end_offset >= run_len {
            // Replace entire run
            self.runs[run_idx].value = value;
            self.compact_around(run_idx);
            return;
        }

        // Before part
        let before = if start_offset > 0 {
            Some(Run {
                value: old_value,
                length: start_offset,
            })
        } else {
            None
        };

        // Replaced part
        let mid = Run {
            value,
            length: range_len,
        };

        // After part
        let after = if end_offset < run_len {
            Some(Run {
                value: old_value,
                length: run_len - end_offset,
            })
        } else {
            None
        };

        // Replace the run with the new runs
        self.replace_run_span(run_idx, run_idx, before, mid, after);
        self.compact();
    }

    /// Split multiple runs to set a range to a new value.
    pub(super) fn split_range_multi_run(
        &mut self,
        start_run_idx: usize,
        start_offset: u32,
        end_run_idx: usize,
        end_offset: u32,
        value: T,
    ) {
        // Calculate total length of the range
        let mut range_len = 0u32;
        for i in start_run_idx..=end_run_idx {
            count_run_iteration();
            let run = &self.runs[i];
            let segment_len = if i == start_run_idx {
                // `start_offset < runs[start_run_idx].length` by find_run's
                // contract at the call site, so this never actually
                // saturates; it just discharges the underflow obligation.
                run.length.saturating_sub(start_offset)
            } else if i == end_run_idx {
                end_offset
            } else {
                run.length
            };
            range_len = Self::checked_run_length_sum(range_len, segment_len);
        }

        // Before part from start run
        let start_run = &self.runs[start_run_idx];
        let before = if start_offset > 0 {
            Some(Run {
                value: start_run.value,
                length: start_offset,
            })
        } else {
            None
        };

        // The new range
        let mid = Run {
            value,
            length: range_len,
        };

        // After part from end run
        let end_run = &self.runs[end_run_idx];
        let after = if end_offset < end_run.length {
            Some(Run {
                value: end_run.value,
                length: end_run.length - end_offset,
            })
        } else {
            None
        };

        // Replace the runs
        self.replace_run_span(start_run_idx, end_run_idx, before, mid, after);
        self.compact();
    }

    /// Replace the runs in `start_run_idx..=end_run_idx` with the sequence
    /// `[before?, mid, after?]`, in place.
    ///
    /// Equivalent to `self.runs.splice(start_run_idx..=end_run_idx, seq)` —
    /// same resulting run sequence, same single O(len) tail movement — but
    /// spelled with plain `Vec` writes/inserts and an explicit tail shift,
    /// because `splice`'s `Drain` internals are raw-pointer MIR the Level-0
    /// verifier reports as an unmodeled construct.
    ///
    /// Callers guarantee `start_run_idx <= end_run_idx < runs.len()` (the
    /// indices come from `find_run`), so `runs.len() <= isize::MAX` bounds
    /// every index below and none of the saturating ops actually saturate;
    /// they only discharge overflow obligations on the symbolic indices.
    fn replace_run_span(
        &mut self,
        start_run_idx: usize,
        end_run_idx: usize,
        before: Option<Run<T>>,
        mid: Run<T>,
        after: Option<Run<T>>,
    ) {
        // First slot past the replaced span.
        let replaced_end = end_run_idx.saturating_add(1);
        let mut write = start_run_idx;

        if let Some(b) = before {
            // `write == start_run_idx <= end_run_idx`: always in the span.
            self.runs[write] = b;
            write = write.saturating_add(1);
        }
        if write < replaced_end {
            self.runs[write] = mid;
        } else {
            self.runs.insert(write, mid);
        }
        write = write.saturating_add(1);
        if let Some(a) = after {
            if write < replaced_end {
                self.runs[write] = a;
            } else {
                self.runs.insert(write, a);
            }
            write = write.saturating_add(1);
        }

        // Fewer replacement runs than replaced slots: shift the tail left
        // over the leftover slots and truncate — exactly what `splice` does
        // after its drain, in one O(len) pass.
        if write < replaced_end {
            let len = self.runs.len();
            let mut dst = write;
            let mut src = replaced_end;
            while src < len {
                self.runs[dst] = self.runs[src];
                dst = dst.saturating_add(1);
                src = src.saturating_add(1);
            }
            self.runs.truncate(dst);
        }
    }

    /// Compact adjacent runs with the same value.
    pub(crate) fn compact(&mut self) {
        if self.runs.len() <= 1 {
            return;
        }

        let mut write = 0;
        for read in 1..self.runs.len() {
            count_run_iteration();
            if self.runs[write].value == self.runs[read].value {
                // Merged run lengths stay `<= u32::MAX` by the total-length
                // invariant; the shared sum's saturation cannot fire.
                self.runs[write].length =
                    Self::checked_run_length_sum(self.runs[write].length, self.runs[read].length);
            } else {
                // `write < read < runs.len() <= isize::MAX`, so these adds
                // never actually saturate; saturating_add just discharges the
                // no-overflow obligations the verifier cannot derive from the
                // loop invariant.
                write = write.saturating_add(1);
                if write != read {
                    self.runs[write] = self.runs[read];
                }
            }
        }
        self.runs.truncate(write.saturating_add(1));
    }

    /// Compact around a specific index.
    pub(crate) fn compact_around(&mut self, idx: usize) {
        // Merged run lengths stay `<= u32::MAX` by the total-length invariant;
        // the shared `checked_run_length_sum` saturation cannot fire (same
        // guarded sum the rest of the crate uses).

        // Merge with previous
        if idx > 0 && self.runs[idx - 1].value == self.runs[idx].value {
            self.runs[idx - 1].length =
                Self::checked_run_length_sum(self.runs[idx - 1].length, self.runs[idx].length);
            self.runs.remove(idx);
            // Check if we need to merge with next (now at idx-1)
            if idx < self.runs.len() && self.runs[idx - 1].value == self.runs[idx].value {
                self.runs[idx - 1].length =
                    Self::checked_run_length_sum(self.runs[idx - 1].length, self.runs[idx].length);
                self.runs.remove(idx);
            }
            return;
        }

        // Merge with next. `idx < runs.len() <= isize::MAX` at every call
        // site, so `idx + 1` never actually saturates; saturating_add just
        // discharges the no-overflow obligation on a symbolic `idx`.
        let next = idx.saturating_add(1);
        if next < self.runs.len() && self.runs[idx].value == self.runs[next].value {
            self.runs[idx].length =
                Self::checked_run_length_sum(self.runs[idx].length, self.runs[next].length);
            self.runs.remove(next);
        }
    }
}
