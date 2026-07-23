// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Display-offset damage computation.
//!
//! Derives which rows need redrawing when the terminal's display offset
//! (scroll position into history) changes. All four scroll functions that
//! modify `display_offset` delegate here instead of carrying bespoke
//! row-marking loops.
//!
//! See #6072 and `designs/2026-03-13-6072-display-offset-damage-deduplication.md`.

use super::Damage;

/// Damage result for a display-offset transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayOffsetDamage {
    /// No damage — offsets unchanged.
    None,
    /// Full screen changed — delta >= visible_rows.
    Full,
    /// Top N rows are new content (scrolled up into history).
    TopRows(u16),
    /// Bottom N rows are new content (scrolled down toward live).
    BottomRows {
        /// First damaged row (inclusive).
        start: u16,
        /// Row count (exclusive upper bound for mark_rows).
        end: u16,
    },
}

/// Compute damage for an arbitrary display-offset transition.
///
/// Given old and new display offsets plus visible row count, returns which
/// rows need redrawing.
///
/// - Scrolling up (new > old) → top rows are new content from scrollback.
/// - Scrolling down (new < old) → bottom rows are newly exposed live content.
/// - Delta ≥ visible rows → full damage.
#[must_use]
pub(crate) fn compute_display_offset_damage(
    old_offset: usize,
    new_offset: usize,
    visible_rows: u16,
) -> DisplayOffsetDamage {
    if old_offset == new_offset {
        return DisplayOffsetDamage::None;
    }
    let rows = usize::from(visible_rows);
    let (delta, scrolled_up) = if new_offset > old_offset {
        (new_offset - old_offset, true)
    } else {
        (old_offset - new_offset, false)
    };
    if delta >= rows {
        return DisplayOffsetDamage::Full;
    }
    if scrolled_up {
        let delta = u16::try_from(delta).expect("display-offset delta fits in u16");
        DisplayOffsetDamage::TopRows(delta)
    } else {
        let start = u16::try_from(rows - delta).expect("display-offset bottom row fits in u16");
        DisplayOffsetDamage::BottomRows {
            start,
            end: visible_rows,
        }
    }
}

impl Damage {
    /// Apply display-offset damage to this damage tracker.
    ///
    /// Convenience method that maps [`DisplayOffsetDamage`] to the
    /// appropriate `mark_*` calls.
    pub(crate) fn apply_display_offset_damage(&mut self, damage: DisplayOffsetDamage) {
        match damage {
            DisplayOffsetDamage::None => {}
            DisplayOffsetDamage::Full => self.mark_full(),
            DisplayOffsetDamage::TopRows(n) => self.mark_rows(0, n),
            DisplayOffsetDamage::BottomRows { start, end } => self.mark_rows(start, end),
        };
    }
}
