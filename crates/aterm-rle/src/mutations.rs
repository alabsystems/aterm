// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Internal mutation helpers for RLE sequences.

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

        let mut accumulated = 0u32;
        for (i, run) in self.runs.iter_mut().enumerate() {
            let next_accumulated = Self::checked_run_length_sum(accumulated, run.length);
            if next_accumulated >= new_length {
                let keep_in_run = new_length.saturating_sub(accumulated);
                run.length = keep_in_run;
                self.runs.truncate(i.saturating_add(1));
                self.total_length = new_length;
                return;
            }
            accumulated = next_accumulated;
        }
    }

    /// Find the run containing `index`: `(run_index, offset_within_run)`.
    #[inline]
    pub(super) fn find_run(&self, index: u32) -> Option<(usize, u32)> {
        self.find_run_linear(index)
    }

    #[inline]
    fn find_run_linear(&self, index: u32) -> Option<(usize, u32)> {
        let mut accumulated = 0u32;
        for (i, run) in self.runs.iter().enumerate() {
            count_run_iteration();
            let next_accumulated = Self::checked_run_length_sum(accumulated, run.length);
            if next_accumulated > index {
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
            self.runs[run_idx].value = value;
            self.compact_around(run_idx);
            return;
        }

        if offset == 0 {
            self.runs[run_idx].length = run_len.saturating_sub(1);
            self.runs.insert(run_idx, Run { value, length: 1 });
            self.compact_around(run_idx);
        } else if offset == run_len.saturating_sub(1) {
            self.runs[run_idx].length = run_len.saturating_sub(1);
            let next_idx = run_idx.saturating_add(1);
            self.runs.insert(next_idx, Run { value, length: 1 });
            self.compact_around(next_idx);
        } else {
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
        let range_len = end_offset.saturating_sub(start_offset);

        if start_offset == 0 && end_offset >= run_len {
            self.runs[run_idx].value = value;
            self.compact_around(run_idx);
            return;
        }

        let before = if start_offset > 0 {
            Some(Run {
                value: old_value,
                length: start_offset,
            })
        } else {
            None
        };

        let mid = Run {
            value,
            length: range_len,
        };

        let after = if end_offset < run_len {
            Some(Run {
                value: old_value,
                length: run_len - end_offset,
            })
        } else {
            None
        };

        let mid_idx = run_idx.saturating_add(usize::from(before.is_some()));
        self.replace_run_span(run_idx, run_idx, before, mid, after);
        self.compact_around(mid_idx);
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
        let mut range_len = 0u32;
        for (offset, run) in self.runs[start_run_idx..=end_run_idx].iter().enumerate() {
            count_run_iteration();
            let segment_len = if offset == 0 {
                run.length.saturating_sub(start_offset)
            } else if offset == end_run_idx - start_run_idx {
                end_offset
            } else {
                run.length
            };
            range_len = Self::checked_run_length_sum(range_len, segment_len);
        }

        let start_run = &self.runs[start_run_idx];
        let before = if start_offset > 0 {
            Some(Run {
                value: start_run.value,
                length: start_offset,
            })
        } else {
            None
        };

        let mid = Run {
            value,
            length: range_len,
        };

        let end_run = &self.runs[end_run_idx];
        let after = if end_offset < end_run.length {
            Some(Run {
                value: end_run.value,
                length: end_run.length - end_offset,
            })
        } else {
            None
        };

        let mid_idx = start_run_idx.saturating_add(usize::from(before.is_some()));
        self.replace_run_span(start_run_idx, end_run_idx, before, mid, after);
        self.compact_around(mid_idx);
    }

    fn replace_run_span(
        &mut self,
        start_run_idx: usize,
        end_run_idx: usize,
        before: Option<Run<T>>,
        mid: Run<T>,
        after: Option<Run<T>>,
    ) {
        let replaced_end = end_run_idx.saturating_add(1);
        let mut write = start_run_idx;

        if let Some(b) = before {
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
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "reference full-scan compaction kept for Trust verification floor and differential equivalence tests"
        )
    )]
    pub(crate) fn compact(&mut self) {
        if self.runs.len() <= 1 {
            return;
        }

        let mut write = 0;
        for read in 1..self.runs.len() {
            count_run_iteration();
            if self.runs[write].value == self.runs[read].value {
                self.runs[write].length =
                    Self::checked_run_length_sum(self.runs[write].length, self.runs[read].length);
            } else {
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
        if idx > 0 && self.runs[idx - 1].value == self.runs[idx].value {
            self.runs[idx - 1].length =
                Self::checked_run_length_sum(self.runs[idx - 1].length, self.runs[idx].length);
            self.runs.remove(idx);
            if idx < self.runs.len() && self.runs[idx - 1].value == self.runs[idx].value {
                self.runs[idx - 1].length =
                    Self::checked_run_length_sum(self.runs[idx - 1].length, self.runs[idx].length);
                self.runs.remove(idx);
            }
            return;
        }

        let next = idx.saturating_add(1);
        if next < self.runs.len() && self.runs[idx].value == self.runs[next].value {
            self.runs[idx].length =
                Self::checked_run_length_sum(self.runs[idx].length, self.runs[next].length);
            self.runs.remove(next);
        }
    }
}

#[cfg(clean_islands)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

verify_island! {
    mod rle_local_compact_proofs {
        clean {
            section RleLocalCompact
            theorem OuterInequalityPreservedOnLeftMerge :
                forall (prev_val left_val mid_val : Nat),
                    Not (Eq prev_val left_val) ->
                    Eq left_val mid_val ->
                    Not (Eq prev_val mid_val) :=
                fun (prev_val left_val mid_val : Nat)
                    (h_neq : Not (Eq prev_val left_val))
                    (h_eq : Eq left_val mid_val) =>
                    fun (h_contra : Eq prev_val mid_val) =>
                        h_neq (Eq.trans h_contra (Eq.symm h_eq))

            theorem OuterInequalityPreservedOnRightMerge :
                forall (mid_val right_val next_val : Nat),
                    Not (Eq right_val next_val) ->
                    Eq mid_val right_val ->
                    Not (Eq mid_val next_val) :=
                fun (mid_val right_val next_val : Nat)
                    (h_neq : Not (Eq right_val next_val))
                    (h_eq : Eq mid_val right_val) =>
                    fun (h_contra : Eq mid_val next_val) =>
                        h_neq (Eq.trans (Eq.symm h_eq) h_contra)
            end RleLocalCompact
        }
    }
}
