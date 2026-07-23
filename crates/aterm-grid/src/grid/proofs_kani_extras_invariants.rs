// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for HAS_EXTRAS invariants: StyleId mutual exclusion,
//! Cell::EMPTY guarantee, set/clear roundtrip, AC7 iff invariant
//! (flag matches extras map entry), and ring_extras push/pop.
//!
//! Split from `proofs_kani_extras.rs` for file-size compliance.
//!
//! ## Verification Targets
//!
//! 7. **StyleId mutual exclusion**: u16 StyleId cast to PackedColors
//!    can never accidentally set bit 16 (HAS_EXTRAS).
//! 8. **Cell::EMPTY guarantee**: erase operations via Cell::EMPTY always
//!    produce cells with HAS_EXTRAS cleared.
//! 9. **set/clear roundtrip**: set_has_extras(true) then (false) preserves
//!    all color data (models remove_cell_extra bit-level correctness).
//! 10. **AC7 iff invariant**: HAS_EXTRAS flag matches extras map entry
//!     presence through create→mutate→remove lifecycle (call-site contract).
//! 11. **ring_extras roundtrip**: Option<Box<ScrolledRowExtras>> push/pop
//!     preserves data through the ring buffer scrollback path.
//!
//! Part of memory_verification phase, #5445 epic.

use super::scroll_convert::ScrolledRowExtras;
use crate::{Cell, CellExtra, PackedColors};

/// StyleId-as-PackedColors can never set HAS_EXTRAS accidentally.
///
/// When USES_STYLE_ID is active, PackedColors stores a StyleId (u16) cast
/// to u32 in its low bits. Since u16::MAX is 65535 (bit 15 is the highest),
/// bit 16 (HAS_EXTRAS_BIT) can never be set from a StyleId value.
/// This proves the implicit mutual exclusion between the two features.
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

/// Cell::EMPTY never has HAS_EXTRAS set.
///
/// This is the foundational invariant for erase operations: clearing a cell
/// to EMPTY must also clear the HAS_EXTRAS bit. Since erase paths reset cells
/// via `row.clear()` / `row.clear_range()` which fill with `Cell::EMPTY`,
/// this proof guarantees they never leave stale HAS_EXTRAS flags.
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

/// set_has_extras(true) then set_has_extras(false) roundtrip on Cell.
///
/// Models the `remove_cell_extra` pattern: after setting has_extras and
/// then clearing it, the cell must report has_extras() == false and color
/// data must be preserved. This proves the remove path is correct at the
/// bit level.
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

/// HAS_EXTRAS flag set iff CellExtras entry exists (AC7 invariant).
///
/// Models the call-site contract from `cell_extra_mut` (accessors.rs:296) and
/// `remove_cell_extra` (accessors.rs:316). These are the two Grid accessors
/// that maintain the "flag iff entry" invariant:
///
/// - `cell_extra_mut`: sets `cell.set_has_extras(true)` then `extras.get_or_create(coord)`
/// - `remove_cell_extra`: calls `extras.remove(coord)` then `cell.set_has_extras(false)`
///
/// The proof verifies the iff invariant holds through a full create→mutate→remove
/// lifecycle using a real `CellExtra` with symbolic RGB color data.
/// The "entry exists" state is modeled as an `Option<CellExtra>` rather than a map,
/// avoiding HashMap/BTreeMap allocator complexity that causes CBMC state explosion.
///
/// Note: bulk operations (clear_row, shift_rows_up_by) may leave stale flags;
/// the rendering path's `|| is_complex()` safety net handles this. This proof
/// covers the per-cell accessor contract, not the bulk approximation.
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

/// Option<Box<ScrolledRowExtras>> roundtrip with real types.
///
/// Verifies the ring_extras push/pop pattern preserves data:
/// empty extras → None (saves allocation), non-empty → Some(Box).
/// The pop path reconstructs the original via Default::default() or unboxing.
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
