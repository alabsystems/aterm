// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for display-offset damage computation.

use super::Damage;
use super::display_offset::{DisplayOffsetDamage, compute_display_offset_damage};

/// The display-offset classification is exact for every delta and size: every
/// (old, new) over 0..=48 at every size 0..=40 and the `u16` edge, plus offset
/// pairs at the `u16::MAX` scale, give `None` exactly when the offsets agree,
/// `Full` exactly when the delta covers the screen, `TopRows(n)` (scrolled up)
/// with `0 < n < rows`, `BottomRows` (scrolled down) with
/// `0 < start < end == rows`, and `min(delta, rows)` damaged rows, with neither
/// `u16::try_from(..).expect()` firing.
#[test]
fn display_offset_damage_classifies_every_delta_and_size() {
    let check = |old: usize, new: usize, rows: u16| {
        let got = compute_display_offset_damage(old, new, rows);
        let delta = old.abs_diff(new);
        let visible = usize::from(rows);
        let at = format!("{old} -> {new} over {rows} rows: {got:?}");
        assert_eq!(got == DisplayOffsetDamage::None, old == new, "{at}");
        assert_eq!(
            got == DisplayOffsetDamage::Full,
            old != new && delta >= visible,
            "{at}"
        );
        let damaged = match got {
            DisplayOffsetDamage::None => 0,
            DisplayOffsetDamage::Full => visible,
            DisplayOffsetDamage::TopRows(n) => {
                assert!(new > old && n > 0 && n < rows, "{at}");
                usize::from(n)
            }
            DisplayOffsetDamage::BottomRows { start, end } => {
                assert!(new < old && start > 0 && start < end && end == rows, "{at}");
                usize::from(end - start)
            }
        };
        assert_eq!(damaged, delta.min(visible), "{at}");
    };
    let max = usize::from(u16::MAX);
    for rows in (0..=40).chain([u16::MAX - 1, u16::MAX]) {
        for old in 0..=48 {
            for new in 0..=48 {
                check(old, new, rows);
            }
        }
        for (old, new) in [
            (0, max),
            (max, 0),
            (max, max - 1),
            (max - 1, max),
            (max, max),
            (max - 40, max),
            (max, max - 40),
        ] {
            check(old, new, rows);
        }
    }
}

/// Each class marks exactly its rows.
#[test]
fn apply_display_offset_damage_marks_exactly_its_rows() {
    let apply = |class| {
        let mut damage = Damage::new(24);
        damage.apply_display_offset_damage(class);
        damage
    };
    assert!(!apply(DisplayOffsetDamage::None).has_damage());
    assert!(apply(DisplayOffsetDamage::Full).is_full());
    let top = apply(DisplayOffsetDamage::TopRows(3));
    assert_eq!(top.damaged_rows(24).collect::<Vec<_>>(), vec![0, 1, 2]);
    let bottom = apply(DisplayOffsetDamage::BottomRows { start: 20, end: 24 });
    assert_eq!(
        bottom.damaged_rows(24).collect::<Vec<_>>(),
        vec![20, 21, 22, 23]
    );
}
