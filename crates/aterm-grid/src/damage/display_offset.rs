// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Row damage calculation for viewport display-offset transitions.

use super::Damage;

/// Damage result for a display-offset transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayOffsetDamage {
    None,
    Full,
    TopRows(u16),
    BottomRows { start: u16, end: u16 },
}

/// Damage for a display-offset transition over `visible_rows`.
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
    /// Apply a [`DisplayOffsetDamage`] through the matching `mark_*` calls.
    pub(crate) fn apply_display_offset_damage(&mut self, damage: DisplayOffsetDamage) {
        match damage {
            DisplayOffsetDamage::None => {}
            DisplayOffsetDamage::Full => self.mark_full(),
            DisplayOffsetDamage::TopRows(n) => self.mark_rows(0, n),
            DisplayOffsetDamage::BottomRows { start, end } => self.mark_rows(start, end),
        };
    }
}
