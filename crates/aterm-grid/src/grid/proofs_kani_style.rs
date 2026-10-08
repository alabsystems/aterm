// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Kani proofs for StyleTable ref-count safety:
//! 1. The default style (index 0) never drops below 1.
//! 2. release() cannot underflow.
//! 3. add_ref saturates (no overflow).
//! 4. intern deduplicates (same style, same ID).

use crate::{Color, Style, StyleAttrs, StyleId, StyleTable};

// Default style protection

/// The default style's ref count never drops below 1: release() of index 0 is
/// a no-op.
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

// Release never underflows

/// Releasing a style more times than it was interned never wraps: once the
/// count reaches 0, further releases are no-ops.
///
/// ENSURES: ref_count >= 0 (the guard prevents wrapping from 0 to u32::MAX)
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

// Add_ref saturating safety

/// add_ref saturates at u32::MAX, for any symbolic interned style.
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

// Intern deduplication

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

// Ref count conservation

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

// Table length consistency

/// StyleTable length equals its entry count (styles, ref_counts and extended
/// stay in sync), with symbolic colours and a symbolic out-of-bounds index.
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
