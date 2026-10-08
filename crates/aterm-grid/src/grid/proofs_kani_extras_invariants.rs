// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for HAS_EXTRAS invariants (split from `proofs_kani_extras.rs`):
//!
//! 7. A u16 StyleId cast into PackedColors never sets bit 16 (HAS_EXTRAS).
//! 8. Cell::EMPTY, which erases write, has HAS_EXTRAS clear.
//! 9. set_has_extras(true) then (false) preserves all colour data.
//! 10. AC7: the HAS_EXTRAS flag matches extras-entry presence through a
//!     create→mutate→remove lifecycle.
//! 11. ring_extras push/pop of `Option<Box<ScrolledRowExtras>>` preserves data.

use super::scroll_convert::ScrolledRowExtras;
use crate::{Cell, CellExtra, PackedColors};

/// A StyleId (u16, highest bit 15) stored in PackedColors can never set bit 16
/// (HAS_EXTRAS_BIT).
#[kani::proof]
fn style_id_as_packed_colors_never_sets_has_extras() {
    let style_id_val: u16 = kani::any();

    // This replicates Cell::with_style_id which does:
    //   self.colors = PackedColors(style_id.raw() as u32)
    let colors = PackedColors(style_id_val as u32);

    kani::assert(
        !colors.has_extras(),
        "StyleId cast to PackedColors must never set HAS_EXTRAS bit",
    );

    // Also verify the bit is in the right place
    kani::assert(
        (style_id_val as u32) & (1u32 << 16) == 0,
        "u16 cast to u32 has bit 16 always zero",
    );
}

/// Cell::EMPTY never has HAS_EXTRAS set, so erases (`row.clear()`,
/// `row.clear_range()`) leave no stale flags.
#[kani::proof]
fn cell_empty_has_no_extras_flag() {
    let empty = Cell::EMPTY;
    kani::assert(
        !empty.has_extras(),
        "Cell::EMPTY must not have HAS_EXTRAS set",
    );
    kani::assert(
        empty.is_empty(),
        "Cell::EMPTY must report is_empty() == true",
    );

    // Also verify that clearing a cell with extras produces no extras flag
    let raw: u32 = kani::any();
    let colors = PackedColors(raw).with_extras_flag();
    kani::assert(colors.has_extras(), "precondition: extras flag set");

    let cleared = colors.without_extras_flag();
    kani::assert(
        !cleared.has_extras(),
        "clearing extras flag must produce no-extras state",
    );

    // The cleared value must have the same color data
    kani::assert(
        cleared.fg_index() == PackedColors(raw).fg_index(),
        "clearing extras must preserve fg_index",
    );
    kani::assert(
        cleared.bg_index() == PackedColors(raw).bg_index(),
        "clearing extras must preserve bg_index",
    );
}

/// set_has_extras(true) then (false) reports has_extras() == false and keeps
/// the colour data (the `remove_cell_extra` pattern).
#[kani::proof]
fn set_has_extras_roundtrip_preserves_colors() {
    let raw: u32 = kani::any();
    let colors = PackedColors(raw);
    let mut cell = Cell::from_ascii_styled(b'A', colors, super::CellFlags::empty());

    let orig_fg = cell.colors().fg_index();
    let orig_bg = cell.colors().bg_index();
    let orig_fg_mode = cell.colors().fg_mode();
    let orig_bg_mode = cell.colors().bg_mode();

    // Set HAS_EXTRAS (models cell_extra_mut path)
    cell.set_has_extras(true);
    kani::assert(cell.has_extras(), "set_has_extras(true) must set flag");
    kani::assert(
        cell.colors().fg_index() == orig_fg,
        "set_has_extras(true) must preserve fg",
    );
    kani::assert(
        cell.colors().bg_index() == orig_bg,
        "set_has_extras(true) must preserve bg",
    );

    // Clear HAS_EXTRAS (models remove_cell_extra path)
    cell.set_has_extras(false);
    kani::assert(!cell.has_extras(), "set_has_extras(false) must clear flag");
    kani::assert(
        cell.colors().fg_index() == orig_fg,
        "set_has_extras(false) must preserve fg",
    );
    kani::assert(
        cell.colors().bg_index() == orig_bg,
        "set_has_extras(false) must preserve bg",
    );
    kani::assert(
        cell.colors().fg_mode() == orig_fg_mode,
        "roundtrip must preserve fg_mode",
    );
    kani::assert(
        cell.colors().bg_mode() == orig_bg_mode,
        "roundtrip must preserve bg_mode",
    );
}

/// HAS_EXTRAS is set iff a CellExtras entry exists (AC7), through a
/// create→mutate→remove lifecycle with symbolic RGB data.
///
/// Models the accessor contract: `cell_extra_mut` sets the flag then
/// `get_or_create`s the entry; `remove_cell_extra` removes the entry then
/// clears the flag. The entry is an `Option<CellExtra>` rather than a map to
/// avoid allocator state explosion. Bulk operations (clear_row,
/// shift_rows_up_by) may leave stale flags, which the render path's
/// `|| is_complex()` tolerates.
#[kani::proof]
fn has_extras_iff_entry_exists() {
    let mut cell = Cell::EMPTY;
    // Model the extras map entry for one cell as Option<CellExtra>.
    // This is equivalent to map.contains_key(&col) without allocator overhead.
    let mut entry: Option<CellExtra> = None;

    // Use symbolic RGB values for the extras data
    let fg_r: u8 = kani::any();
    let fg_g: u8 = kani::any();
    let fg_b: u8 = kani::any();

    // === Initial state: both false ===
    kani::assert(
        !cell.has_extras(),
        "initial: Cell::EMPTY has no extras flag",
    );
    kani::assert(entry.is_none(), "initial: no entry exists");
    kani::assert(
        cell.has_extras() == entry.is_some(),
        "initial: iff invariant holds (both false)",
    );

    // === Simulate cell_extra_mut: set flag + insert with symbolic RGB data ===
    cell.set_has_extras(true);
    let extra = entry.get_or_insert_with(CellExtra::default);
    extra.set_fg_rgb(Some([fg_r, fg_g, fg_b]));
    kani::assert(
        cell.has_extras() && entry.is_some(),
        "after insert: both true with symbolic RGB",
    );
    kani::assert(
        cell.has_extras() == entry.is_some(),
        "after insert: iff invariant holds (both true)",
    );

    // === Verify extra has the symbolic data we wrote ===
    let stored = entry.as_ref().expect("entry must exist after insert");
    kani::assert(
        stored.fg_rgb() == Some([fg_r, fg_g, fg_b]),
        "entry must preserve symbolic RGB data",
    );
    kani::assert(
        stored.has_data(),
        "entry reports has_data() = true for symbolic RGB",
    );

    // === Simulate remove_cell_extra: remove entry + clear flag ===
    entry = None;
    cell.set_has_extras(false);
    kani::assert(
        !cell.has_extras() && entry.is_none(),
        "after remove: both false",
    );
    kani::assert(
        cell.has_extras() == entry.is_some(),
        "after remove: iff invariant holds (both false)",
    );
}

/// ring_extras push/pop round trip: empty extras become None, non-empty
/// `Some(Box)`, and pop reconstructs the original.
#[kani::proof]
fn ring_extras_option_box_real_roundtrip() {
    let has_data: bool = kani::any();

    // Construct a real ScrolledRowExtras
    let mut extras = ScrolledRowExtras::default();
    if has_data {
        extras.rgb_fg.push((0, [128, 128, 128]));
    }

    // Push side (replicates scroll.rs)
    let stored: Option<Box<ScrolledRowExtras>> = if extras.is_empty() {
        None
    } else {
        Some(Box::new(extras))
    };

    // Pop side (replicates scroll.rs)
    let retrieved = stored.map_or_else(ScrolledRowExtras::default, |b| *b);

    // Verify roundtrip
    if has_data {
        kani::assert(
            !retrieved.rgb_fg.is_empty(),
            "non-empty extras must survive roundtrip",
        );
        kani::assert(
            retrieved.rgb_fg[0].1 == [128, 128, 128],
            "RGB data must be preserved through Box roundtrip",
        );
    } else {
        kani::assert(retrieved.is_empty(), "empty extras must roundtrip to empty");
    }
}
