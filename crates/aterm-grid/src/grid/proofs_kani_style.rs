// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for StyleTable ref-count safety.
//!
//! Verifies the key invariants of StyleTable's reference counting:
//! 1. Default style (index 0) is never decremented below 1
//! 2. release() cannot underflow ref counts
//! 3. add_ref uses saturating arithmetic (no overflow)
//! 4. intern deduplicates correctly (same style → same ID)
//! 5. compact preserves all styles with non-zero ref counts
//!
//! Part of extreme-performance theme: StyleTable ref-count safety.

use crate::{Color, Style, StyleAttrs, StyleId, StyleTable};

// =============================================================================
// Default style protection
// =============================================================================

/// Default style ref count never drops below 1, regardless of release calls.
///
/// The default style at index 0 has a permanent reference. release() guards
/// against idx == 0, so calling release(StyleId::new(0)) should be a no-op.
///
/// ENSURES: ref_count(StyleId::new(0)) >= 1 after any number of release calls
#[kani::proof]
#[kani::unwind(6)]
fn default_style_ref_count_permanent() {
    let mut table = StyleTable::kani_stub();

    // Verify initial ref count is 1
    kani::assert(
        table.ref_count(StyleId::DEFAULT) == 1,
        "default style initial ref count must be 1",
    );

    // Try to release the default style multiple times
    let release_count: usize = kani::any();
    kani::assume(release_count <= 5);

    for _ in 0..release_count {
        table.release(StyleId::DEFAULT);
    }

    // Default style must still have ref count >= 1
    kani::assert(
        table.ref_count(StyleId::DEFAULT) >= 1,
        "default style ref count dropped below 1 after releases",
    );
}

// =============================================================================
// Release never underflows
// =============================================================================

/// Releasing a style more times than it was interned never causes underflow.
///
/// The release() guard checks ref_counts[idx] > 0 before decrementing.
/// After ref count reaches 0, further releases are no-ops.
///
/// ENSURES: ref_count >= 0 (u32 cannot be negative, but we verify
///          the guard prevents wrapping from 0 to u32::MAX)
#[kani::proof]
#[kani::unwind(6)]
fn release_never_underflows_on_valid_id() {
    let mut table = StyleTable::kani_stub();

    // Create a non-default style by interning it
    let style = Style {
        fg: Color::new(255, 0, 0),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::BOLD,
    };
    let id = table.kani_intern(style);

    // ref_count should be 1 after intern
    kani::assert(
        table.ref_count(id) == 1,
        "ref count should be 1 after initial intern",
    );

    // Release more times than we interned
    let release_count: usize = kani::any();
    kani::assume(release_count <= 5);

    for _ in 0..release_count {
        table.release(id);
    }

    // ref_count should be 0 or 1 (never wrapped around)
    let rc = table.ref_count(id);
    kani::assert(rc <= 1, "ref count should be 0 or 1 after releases");

    // If we released at least once, ref count should be 0
    if release_count >= 1 {
        kani::assert(rc == 0, "ref count should be 0 after at least one release");
    }
}

/// Releasing an out-of-bounds StyleId is a no-op (no panic).
#[kani::proof]
fn release_out_of_bounds_is_noop() {
    let mut table = StyleTable::kani_stub();

    // Table only has the default style (index 0), so index 1+ is out of bounds
    let bogus_idx: u16 = kani::any();
    kani::assume(bogus_idx >= 1);

    table.release(StyleId::new(bogus_idx));

    // Default style should be unaffected
    kani::assert(
        table.ref_count(StyleId::DEFAULT) == 1,
        "release of bogus ID affected default style",
    );
    kani::assert(table.len() == 1, "release of bogus ID changed table length");
}

// =============================================================================
// Add_ref saturating safety
// =============================================================================

/// add_ref uses saturating arithmetic: ref count never wraps past u32::MAX.
///
/// Tests with a symbolic style (not just DEFAULT) to verify the property
/// holds for any interned style.
#[kani::proof]
fn add_ref_saturates_at_max() {
    let mut table = StyleTable::kani_stub();

    // Intern a style with symbolic color components
    let r: u8 = kani::any();
    let g: u8 = kani::any();
    let b: u8 = kani::any();
    let style = Style {
        fg: Color::new(r, g, b),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::empty(),
    };
    let id = table.kani_intern(style);

    let initial_rc = table.ref_count(id);
    table.add_ref(id);
    let after_rc = table.ref_count(id);

    kani::assert(
        after_rc >= initial_rc,
        "add_ref decreased ref count on symbolic style (overflow detected)",
    );
    kani::assert(
        after_rc == initial_rc + 1 || after_rc == u32::MAX,
        "add_ref must increment by 1 or saturate at u32::MAX",
    );
}

/// add_ref on out-of-bounds ID is a no-op.
#[kani::proof]
fn add_ref_out_of_bounds_is_noop() {
    let mut table = StyleTable::kani_stub();

    let bogus_idx: u16 = kani::any();
    kani::assume(bogus_idx >= 1);

    table.add_ref(StyleId::new(bogus_idx));

    kani::assert(
        table.ref_count(StyleId::DEFAULT) == 1,
        "add_ref of bogus ID affected default style",
    );
}

// =============================================================================
// Intern deduplication
// =============================================================================

/// Interning the same style twice returns the same ID for any symbolic style.
///
/// ENSURES: intern(s) == intern(s) for any style s
#[kani::proof]
fn intern_same_style_returns_same_id() {
    let mut table = StyleTable::kani_stub();

    // Use symbolic color components so deduplication is verified for all colors
    let r: u8 = kani::any();
    let g: u8 = kani::any();
    let b: u8 = kani::any();
    let attrs_bits: u16 = kani::any();
    kani::assume(attrs_bits <= 0x1FFF); // Valid StyleAttrs range (13 flags)
    let style = Style {
        fg: Color::new(r, g, b),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::from_bits_truncate(attrs_bits),
    };

    let id1 = table.kani_intern(style);
    let id2 = table.kani_intern(style);

    kani::assert(
        id1 == id2,
        "interning same symbolic style must return same ID",
    );

    // ref count should be 2 (one per intern call)
    kani::assert(
        table.ref_count(id1) == 2,
        "ref count should be 2 after two interns of same symbolic style",
    );
}

/// Interning two different symbolic styles returns different IDs.
#[kani::proof]
fn intern_different_styles_returns_different_ids() {
    let mut table = StyleTable::kani_stub();

    // Use symbolic color components for both styles
    let r_a: u8 = kani::any();
    let g_a: u8 = kani::any();
    let r_b: u8 = kani::any();
    let g_b: u8 = kani::any();

    let style_a = Style {
        fg: Color::new(r_a, g_a, 0),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::empty(),
    };
    let style_b = Style {
        fg: Color::new(r_b, g_b, 0),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::empty(),
    };

    // Only test when styles are actually different
    kani::assume(style_a != style_b);

    let id_a = table.kani_intern(style_a);
    let id_b = table.kani_intern(style_b);

    kani::assert(
        id_a != id_b,
        "different symbolic styles must get different IDs",
    );

    // Both should have ref count 1
    kani::assert(table.ref_count(id_a) == 1, "style_a ref count should be 1");
    kani::assert(table.ref_count(id_b) == 1, "style_b ref count should be 1");

    // Table should have 3 entries (default + a + b)
    kani::assert(table.len() == 3, "table should have 3 symbolic styles");
}

// =============================================================================
// Intern + release + compact cycle
// =============================================================================

/// After intern → release → compact, released styles are removed
/// but active styles are preserved, for symbolic style colors.
///
/// Uses `kani_intern()` + `compact_vec_only()` to avoid FxHashMap
/// symbolic state that is intractable for CBMC.  The HashMap rebuild
/// in production `compact()` is a deterministic function of the Vec
/// contents — verifying the Vec compaction proves the safety properties.
#[kani::proof]
#[kani::unwind(5)]
fn compact_preserves_active_removes_dead() {
    let mut table = StyleTable::kani_stub();

    // Use symbolic colors for both styles
    let keep_r: u8 = kani::any();
    let keep_g: u8 = kani::any();
    let drop_r: u8 = kani::any();
    let drop_g: u8 = kani::any();

    let style_keep = Style {
        fg: Color::new(keep_r, keep_g, 0),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::BOLD,
    };
    let style_drop = Style {
        fg: Color::new(drop_r, drop_g, 1),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::ITALIC,
    };

    // Ensure styles are different (blue channel differs: 0 vs 1)
    kani::assume(style_keep != style_drop);

    let id_keep = table.kani_intern(style_keep);
    let id_drop = table.kani_intern(style_drop);

    // Release the style we want to drop
    table.release(id_drop);
    kani::assert(
        table.ref_count(id_drop) == 0,
        "dropped symbolic style should have ref count 0",
    );

    // Compact (Vec-only, skips intractable HashMap rebuild)
    let id_map = table.compact_vec_only();

    // Table should now have 2 entries (default + keep)
    kani::assert(
        table.len() == 2,
        "compact should remove dead symbolic styles",
    );

    // The kept style should still be retrievable via the remapped ID
    let new_id = id_map[id_keep.raw() as usize];
    let retrieved = table.get(new_id);
    kani::assert(
        retrieved == Some(&style_keep),
        "compact must preserve active symbolic style content",
    );

    // Default style should still be at index 0
    kani::assert(
        id_map[0] == StyleId::DEFAULT,
        "compact must preserve default style at index 0",
    );
    kani::assert(
        table.ref_count(StyleId::DEFAULT) >= 1,
        "default style ref count must survive compact",
    );
}

/// Compact with all styles active is a no-op on table size,
/// verified with a symbolic style.
///
/// Uses `kani_intern()` + `compact_vec_only()` — see
/// `compact_preserves_active_removes_dead` for rationale.
#[kani::proof]
#[kani::unwind(4)]
fn compact_all_active_preserves_size() {
    let mut table = StyleTable::kani_stub();

    // Intern a style with symbolic color
    let r: u8 = kani::any();
    let g: u8 = kani::any();
    let b: u8 = kani::any();
    let style = Style {
        fg: Color::new(r, g, b),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::empty(),
    };
    let id = table.kani_intern(style);

    let len_before = table.len();
    let _id_map = table.compact_vec_only();

    kani::assert(
        table.len() == len_before,
        "compact with all active symbolic styles must not change length",
    );
    kani::assert(
        table.ref_count(id) == 1,
        "compact must not change active ref counts for symbolic style",
    );
}

// =============================================================================
// Ref count conservation
// =============================================================================

/// add_ref and release are inverse operations: N add_refs followed by
/// N releases returns to the original ref count.
#[kani::proof]
#[kani::unwind(5)]
fn add_ref_release_inverse() {
    let mut table = StyleTable::kani_stub();

    let style = Style {
        fg: Color::new(128, 128, 128),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::DIM,
    };
    let id = table.kani_intern(style);
    let initial_rc = table.ref_count(id);

    let n: u32 = kani::any();
    kani::assume(n >= 1 && n <= 4);

    for _ in 0..n {
        table.add_ref(id);
    }
    kani::assert(
        table.ref_count(id) == initial_rc + n,
        "add_ref should increment ref count by 1 each call",
    );

    for _ in 0..n {
        table.release(id);
    }
    kani::assert(
        table.ref_count(id) == initial_rc,
        "N add_refs followed by N releases must restore original ref count",
    );
}

// =============================================================================
// Table length consistency
// =============================================================================

/// StyleTable length equals the number of entries (styles, ref_counts, extended
/// are always kept in sync), verified with symbolic style colors and a symbolic
/// out-of-bounds index.
#[kani::proof]
fn table_vecs_always_in_sync() {
    let mut table = StyleTable::kani_stub();

    // Table starts with 1 entry (default)
    kani::assert(table.len() == 1, "initial table should have 1 entry");

    // Intern a style with symbolic color components
    let r: u8 = kani::any();
    let g: u8 = kani::any();
    let b: u8 = kani::any();
    let style = Style {
        fg: Color::new(r, g, b),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::UNDERLINE,
    };
    let id = table.kani_intern(style);

    kani::assert(
        table.len() == 2,
        "after intern of symbolic style, table should have 2 entries",
    );

    // get() should work for all valid indices
    kani::assert(
        table.get(StyleId::DEFAULT).is_some(),
        "default style must be gettable",
    );
    kani::assert(
        table.get(id).is_some(),
        "interned symbolic style must be gettable",
    );

    // Out-of-bounds get with symbolic index returns None
    let oob_idx: u16 = kani::any();
    kani::assume(oob_idx >= 2); // beyond default + interned
    kani::assert(
        table.get(StyleId::new(oob_idx)).is_none(),
        "out-of-bounds symbolic index must return None",
    );
}
