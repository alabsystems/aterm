// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Observation of history access and image placement on the current parser thread.

use std::cell::Cell;

thread_local! {
    static READS: Cell<u64> = const { Cell::new(0) };
}

#[inline]
pub(crate) fn note() {
    READS.with(|reads| reads.set(reads.get().wrapping_add(1)));
}

/// Run `action` and report history access or image placement on this thread.
/// The monotonic stamp survives nested observations and grid replacement; it is
/// not terminal state and is never reset, serialized, or shared between threads.
#[must_use]
pub fn observe_history_effects<T>(action: impl FnOnce() -> T) -> (T, bool) {
    let before = READS.with(Cell::get);
    let result = action();
    let read = READS.with(Cell::get) != before;
    (result, read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Grid;

    #[test]
    fn misses_nested_reads_and_replaced_grids_remain_observable() {
        let mut grid = Grid::new(3, 10);
        let (_, read) = observe_history_effects(|| grid.history_any_image(&|_| true));
        assert!(read);
        let (_, outer) = observe_history_effects(|| {
            let (_, inner) = observe_history_effects(|| grid.get_history_line(100));
            assert!(inner);
            grid = Grid::new(3, 10);
            assert!(grid.history_line_rev(100).is_none());
        });
        assert!(outer);
        let mut lines = grid.history_lines_from(100);
        assert!(observe_history_effects(|| lines.next()).1);
        assert!(!observe_history_effects(|| grid.scrollback_lines()).1);
        assert!(!observe_history_effects(|| grid.history_fence()).1);
    }

    #[test]
    fn reads_on_another_thread_do_not_change_this_observation() {
        let (_, read) = observe_history_effects(|| {
            std::thread::spawn(|| {
                let grid = Grid::new(3, 10);
                assert!(observe_history_effects(|| grid.get_history_line(0)).1);
            })
            .join()
            .unwrap();
        });
        assert!(!read);
    }
}
