// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests and test-only helpers for the grid style module.
//!
//! Test helpers extracted from style.rs (#2487).
//! Tests extracted from style.rs (#1977).

use super::*;
use crate::PackedColors;

#[test]
fn style_id_default() {
    assert!(StyleId::DEFAULT.is_default());
    assert!(!StyleId::new(1).is_default());
}

#[test]
fn color_constructors() {
    let c = Color::new(100, 150, 200);
    assert_eq!(c.r, 100);
    assert_eq!(c.g, 150);
    assert_eq!(c.b, 200);
    assert_eq!(c.a, 255);

    let c2 = Color::with_alpha(100, 150, 200, 128);
    assert_eq!(c2.a, 128);
}

#[test]
fn color_from_rgb_tuple() {
    let c = Color::from_rgb((10, 20, 30));
    assert_eq!(c.to_rgb(), (10, 20, 30));
}

#[test]
fn color_is_default() {
    assert!(Color::DEFAULT_FG.is_default_fg());
    assert!(!Color::DEFAULT_FG.is_default_bg());
    assert!(Color::DEFAULT_BG.is_default_bg());
    assert!(!Color::DEFAULT_BG.is_default_fg());
}

#[test]
fn style_constructors() {
    let s = Style::with_fg(Color::new(255, 0, 0));
    assert_eq!(s.fg, Color::new(255, 0, 0));
    assert_eq!(s.bg, Color::DEFAULT_BG);
    assert!(s.attrs.is_empty());

    let s = Style::with_bg(Color::new(0, 0, 255));
    assert_eq!(s.fg, Color::DEFAULT_FG);
    assert_eq!(s.bg, Color::new(0, 0, 255));

    let s = Style::with_attrs(StyleAttrs::BOLD | StyleAttrs::ITALIC);
    assert!(s.attrs.contains(StyleAttrs::BOLD));
    assert!(s.attrs.contains(StyleAttrs::ITALIC));
}

#[test]
fn style_is_default() {
    assert!(Style::DEFAULT.is_default());
    assert!(!Style::with_fg(Color::new(100, 100, 100)).is_default());
    assert!(!Style::with_attrs(StyleAttrs::BOLD).is_default());
}

#[test]
fn style_setters() {
    let s = Style::DEFAULT
        .set_fg(Color::new(255, 0, 0))
        .set_bg(Color::new(0, 255, 0))
        .set_attrs(StyleAttrs::UNDERLINE);

    assert_eq!(s.fg, Color::new(255, 0, 0));
    assert_eq!(s.bg, Color::new(0, 255, 0));
    assert!(s.attrs.contains(StyleAttrs::UNDERLINE));
}

#[test]
fn intern_same_style() {
    let mut table = StyleTable::new();

    let style = Style {
        fg: Color::new(255, 0, 0),
        bg: Color::DEFAULT_BG,
        attrs: StyleAttrs::BOLD,
    };

    let id1 = table.intern(style);
    let id2 = table.intern(style);

    assert_eq!(id1, id2);
    assert_eq!(table.len(), 2); // default + our style
    assert_eq!(table.ref_count(id1), 2);
}

#[test]
fn intern_different_styles() {
    let mut table = StyleTable::new();

    let style1 = Style {
        fg: Color::new(255, 0, 0),
        ..Default::default()
    };
    let style2 = Style {
        fg: Color::new(0, 255, 0),
        ..Default::default()
    };

    let id1 = table.intern(style1);
    let id2 = table.intern(style2);

    assert_ne!(id1, id2);
    assert_eq!(table.len(), 3);
}

#[test]
fn table_default_style() {
    let table = StyleTable::new();
    assert_eq!(table.len(), 1);
    // A table with only the default style is considered "empty" (no user styles)
    assert!(table.is_empty());

    let default = table.get(StyleId::DEFAULT).unwrap();
    assert!(default.is_default());
}

#[test]
fn table_release() {
    let mut table = StyleTable::new();

    let style = Style::with_fg(Color::new(255, 0, 0));
    let id = table.intern(style);
    assert_eq!(table.ref_count(id), 1);

    table.add_ref(id);
    assert_eq!(table.ref_count(id), 2);

    table.release(id);
    assert_eq!(table.ref_count(id), 1);

    table.release(id);
    assert_eq!(table.ref_count(id), 0);

    // Style still exists, just zero refs
    let stored = table.get(id).expect("style should persist at zero refs");
    assert_eq!(stored.fg, Color::new(255, 0, 0), "style fg color preserved");
}

#[test]
fn table_release_default_not_decremented() {
    let mut table = StyleTable::new();

    // Default style always has ref_count >= 1
    let initial = table.ref_count(StyleId::DEFAULT);
    table.release(StyleId::DEFAULT);
    assert_eq!(table.ref_count(StyleId::DEFAULT), initial);
}

#[test]
fn table_get_id() {
    let mut table = StyleTable::new();

    let style = Style::with_fg(Color::new(255, 0, 0));
    assert!(table.get_id(&style).is_none());

    let id = table.intern(style);
    let ref_before = table.ref_count(id);

    let found_id = table.get_id(&style);
    assert_eq!(found_id, Some(id));

    // get_id shouldn't increment ref count
    assert_eq!(table.ref_count(id), ref_before);
}

#[test]
fn table_stats() {
    let mut table = StyleTable::new();

    let style1 = Style::with_fg(Color::new(255, 0, 0));
    let style2 = Style::with_bg(Color::new(0, 0, 255));

    let id1 = table.intern(style1);
    table.intern(style1); // Second ref to style1
    table.intern(style2);

    let stats = table.stats();
    assert_eq!(stats.total_styles, 3);
    assert_eq!(stats.active_styles, 3);
    assert_eq!(stats.total_refs, 4); // 1 default + 2 style1 + 1 style2

    table.release(id1);
    table.release(id1);
    let stats = table.stats();
    assert_eq!(stats.active_styles, 2); // style1 now has 0 refs
}

#[test]
fn table_compact() {
    let mut table = StyleTable::new();

    let style1 = Style::with_fg(Color::new(255, 0, 0));
    let style2 = Style::with_bg(Color::new(0, 0, 255));
    let style3 = Style::with_attrs(StyleAttrs::BOLD);

    let id1 = table.intern(style1);
    let id2 = table.intern(style2);
    let id3 = table.intern(style3);

    // Release style2
    table.release(id2);

    assert_eq!(table.len(), 4);

    let id_map = table.compact();

    // Should have removed style2
    assert_eq!(table.len(), 3);

    // Default should stay at 0
    assert_eq!(id_map[0], StyleId::DEFAULT);

    // style1 and style3 should be remapped
    assert_ne!(id_map[id1.raw() as usize], StyleId::DEFAULT);
    assert_ne!(id_map[id3.raw() as usize], StyleId::DEFAULT);
}

#[test]
fn table_clear() {
    let mut table = StyleTable::new();

    let style = Style::with_fg(Color::new(255, 0, 0));
    table.intern(style);
    table.intern(Style::with_attrs(StyleAttrs::BOLD));

    assert_eq!(table.len(), 3);

    table.clear();

    assert_eq!(table.len(), 1);
    let default = table
        .get(StyleId::DEFAULT)
        .expect("default style should survive clear");
    assert_eq!(
        *default,
        Style::DEFAULT,
        "default style should be the canonical default"
    );
}

#[test]
fn extended_style_from_cell_style_default() {
    let ext = ExtendedStyle::from_cell_style(PackedColors::DEFAULT, CellFlags::empty(), None, None);

    assert_eq!(ext.fg_type, ColorType::Default);
    assert_eq!(ext.bg_type, ColorType::Default);
    assert!(ext.style.attrs.is_empty());
}

#[test]
fn extended_style_from_cell_style_indexed() {
    let colors = PackedColors::with_indexed(196, 21);
    let ext = ExtendedStyle::from_cell_style(colors, CellFlags::BOLD, None, None);

    assert_eq!(ext.fg_type, ColorType::Indexed);
    assert_eq!(ext.fg_index, 196);
    assert_eq!(ext.bg_type, ColorType::Indexed);
    assert_eq!(ext.bg_index, 21);
    assert!(ext.style.attrs.contains(StyleAttrs::BOLD));
}

#[test]
fn extended_style_from_cell_style_rgb() {
    let colors = PackedColors::DEFAULT.with_rgb_fg().with_rgb_bg();
    let ext = ExtendedStyle::from_cell_style(
        colors,
        CellFlags::ITALIC,
        Some((255, 128, 64)),
        Some((32, 64, 128)),
    );

    assert_eq!(ext.fg_type, ColorType::Rgb);
    assert_eq!(ext.style.fg.to_rgb(), (255, 128, 64));
    assert_eq!(ext.bg_type, ColorType::Rgb);
    assert_eq!(ext.style.bg.to_rgb(), (32, 64, 128));
    assert!(ext.style.attrs.contains(StyleAttrs::ITALIC));
}

#[test]
fn extended_style_roundtrip() {
    let colors = PackedColors::with_indexed(100, 200);
    let flags = CellFlags::BOLD
        .union(CellFlags::UNDERLINE)
        .union(CellFlags::STRIKETHROUGH);

    let ext = ExtendedStyle::from_cell_style(colors, flags, None, None);

    let packed = ext.to_packed_colors();
    assert!(packed.fg_is_indexed());
    assert_eq!(packed.fg_index(), 100);
    assert!(packed.bg_is_indexed());
    assert_eq!(packed.bg_index(), 200);

    let cell_flags = ExtendedStyle::attrs_to_cell_flags(ext.style.attrs);
    assert!(cell_flags.contains(CellFlags::BOLD));
    assert!(cell_flags.contains(CellFlags::UNDERLINE));
    assert!(cell_flags.contains(CellFlags::STRIKETHROUGH));
}

#[test]
fn intern_extended_style() {
    let mut table = StyleTable::new();

    let colors = PackedColors::with_indexed(100, 200);
    let flags = CellFlags::BOLD;
    let ext = ExtendedStyle::from_cell_style(colors, flags, None, None);

    let id = table.intern_extended(ext);

    let retrieved = table
        .extended(id)
        .expect("interned style must be retrievable");
    assert_eq!(retrieved.fg_type, ColorType::Indexed);
    assert_eq!(retrieved.fg_index, 100);
    assert_eq!(retrieved.bg_index, 200);
}

#[test]
fn all_style_attrs_roundtrip() {
    // Non-underline attrs + a single underline style (UNDERLINE).
    // Underline styles are mutually exclusive in CellFlags: DOTTED_UNDERLINE
    // and DASHED_UNDERLINE are compound bit patterns (UNDERLINE|CURLY and
    // DOUBLE|CURLY respectively), so combining all underline variants creates
    // false matches. Test each underline variant independently below.
    let base_attrs = StyleAttrs::BOLD
        | StyleAttrs::DIM
        | StyleAttrs::ITALIC
        | StyleAttrs::BLINK
        | StyleAttrs::INVERSE
        | StyleAttrs::HIDDEN
        | StyleAttrs::STRIKETHROUGH
        | StyleAttrs::OVERLINE;

    let base_flags = CellFlags::BOLD
        .union(CellFlags::DIM)
        .union(CellFlags::ITALIC)
        .union(CellFlags::BLINK)
        .union(CellFlags::INVERSE)
        .union(CellFlags::HIDDEN)
        .union(CellFlags::STRIKETHROUGH)
        .union(CellFlags::OVERLINE);

    // CellFlags -> StyleAttrs
    let ext = ExtendedStyle::from_cell_style(PackedColors::DEFAULT, base_flags, None, None);
    assert_eq!(ext.style.attrs, base_attrs);

    // StyleAttrs -> CellFlags
    let recovered_flags = ExtendedStyle::attrs_to_cell_flags(base_attrs);
    assert_eq!(
        recovered_flags.bits() & base_flags.bits(),
        base_flags.bits()
    );

    // Each underline variant round-trips independently.
    for (cf, sa) in [
        (CellFlags::UNDERLINE, StyleAttrs::UNDERLINE),
        (CellFlags::DOUBLE_UNDERLINE, StyleAttrs::DOUBLE_UNDERLINE),
        (CellFlags::CURLY_UNDERLINE, StyleAttrs::CURLY_UNDERLINE),
        (CellFlags::DOTTED_UNDERLINE, StyleAttrs::DOTTED_UNDERLINE),
        (CellFlags::DASHED_UNDERLINE, StyleAttrs::DASHED_UNDERLINE),
    ] {
        let ext =
            ExtendedStyle::from_cell_style(PackedColors::DEFAULT, base_flags.union(cf), None, None);
        assert!(
            ext.style.attrs.contains(sa),
            "{sa:?} should survive CellFlags -> StyleAttrs"
        );
        let recovered = ExtendedStyle::attrs_to_cell_flags(base_attrs.union(sa));
        assert!(
            recovered.contains(cf),
            "{cf:?} should survive StyleAttrs -> CellFlags"
        );
    }
}

#[test]
fn superscript_and_subscript_roundtrip_between_cell_flags_and_style_attrs() {
    for (cell_flag, style_attr) in [
        (CellFlags::SUPERSCRIPT, StyleAttrs::SUPERSCRIPT),
        (CellFlags::SUBSCRIPT, StyleAttrs::SUBSCRIPT),
    ] {
        let ext = ExtendedStyle::from_cell_style(PackedColors::DEFAULT, cell_flag, None, None);
        assert!(
            ext.style.attrs.contains(style_attr),
            "{style_attr:?} should survive CellFlags -> StyleAttrs conversion"
        );

        let recovered_flags = ExtendedStyle::attrs_to_cell_flags(style_attr);
        assert!(
            recovered_flags.contains(cell_flag),
            "{cell_flag:?} should survive StyleAttrs -> CellFlags conversion"
        );
    }
}

/// Verify StyleTable intern is O(1) with respect to table size.
///
/// The hash-based lookup should have constant time regardless of how many
/// styles are already in the table. This verifies the claim at style.rs:681
/// "lookup: FxHashMap for O(1) intern lookups".
///
/// Addresses claim verification in #1646.
#[test]
fn style_intern_constant_time() {
    fn measure_intern_ops(num_existing: usize, num_lookups: usize) -> usize {
        // Clear counter
        take_style_intern_ops();

        let mut table = StyleTable::new();

        // Pre-populate table with `num_existing` unique styles
        for i in 0..num_existing {
            let style = Style::with_fg(Color::new(
                (i % 256) as u8,
                ((i / 256) % 256) as u8,
                ((i / 65536) % 256) as u8,
            ));
            table.intern(style);
        }

        // Clear ops from setup
        take_style_intern_ops();

        // Perform lookups of EXISTING styles (hash lookup path)
        for i in 0..num_lookups {
            let style = Style::with_fg(Color::new(
                (i % num_existing % 256) as u8,
                ((i % num_existing / 256) % 256) as u8,
                ((i % num_existing / 65536) % 256) as u8,
            ));
            table.intern(style);
        }

        take_style_intern_ops()
    }

    let lookups = 1000;

    // Measure with small table (1000 styles) vs large table (10000 styles)
    let ops_small = measure_intern_ops(1000, lookups);
    let ops_large = measure_intern_ops(10000, lookups);

    // Both should perform the same number of operations (O(1) per lookup)
    assert!(
        ops_small > 0,
        "intern on 1000-style table should perform ops, got 0"
    );
    assert!(
        ops_large > 0,
        "intern on 10000-style table should perform ops, got 0"
    );

    // O(1) = ops should be equal (both do `lookups` operations)
    assert_eq!(
        ops_small, ops_large,
        "O(1) intern: ops should be equal regardless of table size (small={ops_small}, large={ops_large})"
    );
    assert_eq!(
        ops_small, lookups,
        "Expected exactly {lookups} intern ops, got {ops_small}"
    );
}

/// Verify StyleTable cap behavior at u16::MAX (#4548).
///
/// When the table reaches u16::MAX entries and no styles are reclaimable,
/// `insert_new_style` returns `StyleId::DEFAULT`.
#[test]
fn style_table_cap_at_u16_max() {
    let mut table = StyleTable::new();

    // Fill table to capacity. Style 0 (default) already exists.
    for i in 1..u16::MAX as usize {
        let style = Style::with_fg(Color::new(
            (i % 256) as u8,
            ((i / 256) % 256) as u8,
            ((i / 65536) % 256) as u8,
        ));
        let id = table.intern(style);
        assert_ne!(
            id,
            StyleId::DEFAULT,
            "style {i} should get a unique ID before cap"
        );
    }

    assert_eq!(table.len(), u16::MAX as usize);

    // Next unique style triggers compact (no-op: all refs > 0), then degrades.
    let overflow_style = Style::with_fg(Color::new(1, 1, 1)).set_bg(Color::new(2, 2, 2));
    let id = table.intern(overflow_style);
    assert_eq!(
        id,
        StyleId::DEFAULT,
        "at cap with no reclaimable styles, must return default ID"
    );

    assert_eq!(
        table.len(),
        u16::MAX as usize,
        "table must not grow past u16::MAX"
    );
}

/// At saturation with dead styles, new inserts degrade to DEFAULT (#7446).
///
/// Previously, `insert_new_style` called `compact()` at saturation to reclaim
/// dead style slots. However, `compact()` remaps style IDs and the remap was
/// discarded — existing grid cells would reference old IDs now pointing to
/// wrong styles. The fix disables compaction during insert; new styles degrade
/// to DEFAULT while existing cell styles remain correct.
#[test]
fn style_table_saturation_with_dead_styles_degrades_to_default() {
    let mut table = StyleTable::new();

    // Fill table to capacity, tracking a few IDs to release later.
    let mut release_targets = Vec::new();
    for i in 1..u16::MAX as usize {
        let style = Style::with_fg(Color::new(
            (i % 256) as u8,
            ((i / 256) % 256) as u8,
            ((i / 65536) % 256) as u8,
        ));
        let id = table.intern(style);
        // Release the first 100 non-default styles after filling.
        if i <= 100 {
            release_targets.push(id);
        }
    }

    assert_eq!(table.len(), u16::MAX as usize);

    // Release 100 styles — their ref counts drop to 0.
    for id in &release_targets {
        table.release(*id);
    }
    assert_eq!(table.ref_count(release_targets[0]), 0);

    // Insert a new unique style. At saturation, must degrade to DEFAULT
    // rather than compacting (which would corrupt existing cell style IDs).
    let new_style = Style::with_fg(Color::new(1, 1, 1)).set_bg(Color::new(2, 2, 2));
    let id = table.intern(new_style);
    assert_eq!(
        id,
        StyleId::DEFAULT,
        "at saturation, new styles must degrade to DEFAULT even with dead slots (#7446)"
    );

    // Table must NOT have been compacted — length unchanged.
    assert_eq!(
        table.len(),
        u16::MAX as usize,
        "table must not compact during insert (would corrupt existing cell IDs)"
    );
}

/// Existing style IDs remain valid after saturation fallback (#7446).
///
/// When the table is full and a new insert degrades to DEFAULT, all previously
/// allocated style IDs must still resolve to their original styles. This is
/// the core invariant that the compaction removal protects.
#[test]
fn style_table_saturation_preserves_existing_styles() {
    let mut table = StyleTable::new();

    // Fill table to near-capacity, recording a sample of styles.
    let mut samples: Vec<(StyleId, Style)> = Vec::new();
    for i in 1..u16::MAX as usize {
        let style = Style::with_fg(Color::new(
            (i % 256) as u8,
            ((i / 256) % 256) as u8,
            ((i / 65536) % 256) as u8,
        ));
        let id = table.intern(style);
        // Sample every 1000th style for verification.
        if i % 1000 == 0 {
            samples.push((id, style));
        }
    }

    assert_eq!(table.len(), u16::MAX as usize);
    assert!(
        !samples.is_empty(),
        "should have recorded sample styles for verification"
    );

    // Attempt to insert a new style at saturation — triggers fallback.
    let overflow_style = Style::with_fg(Color::new(1, 1, 1)).set_bg(Color::new(2, 2, 2));
    let overflow_id = table.intern(overflow_style);
    assert_eq!(
        overflow_id,
        StyleId::DEFAULT,
        "new style degrades to DEFAULT"
    );

    // Verify all sampled styles still resolve to their original values.
    for (id, expected_style) in &samples {
        let actual = table.get(*id);
        assert_eq!(
            actual,
            Some(expected_style),
            "style ID {} must still resolve to original style after saturation fallback",
            id.raw()
        );
    }
}

/// Verify compact() id_map correctness: surviving styles resolve to same content.
///
/// The id_map returned by compact() must satisfy:
/// `table.get(id_map[old_idx]) == old_style` for all surviving (ref_count > 0) entries.
/// Dead entries (ref_count == 0) must map to StyleId::DEFAULT.
///
/// Part of #4548
#[test]
fn style_table_compact_remap_preserves_content() {
    let mut table = StyleTable::new();

    // Create 10 distinct styles and record their IDs and values.
    let mut entries: Vec<(StyleId, Style)> = Vec::new();
    for i in 1..=10u8 {
        let style = Style::with_fg(Color::new(i, i.wrapping_mul(2), i.wrapping_mul(3)));
        let id = table.intern(style);
        entries.push((id, style));
    }
    assert_eq!(table.len(), 11); // 10 + default

    // Release styles at indices 2, 5, 7 (0-based in entries vec).
    let dead_indices = [2usize, 5, 7];
    for &di in &dead_indices {
        table.release(entries[di].0);
    }

    // Compact and get the remap.
    let id_map = table.compact();
    assert_eq!(table.len(), 8); // 11 - 3 dead = 8

    // Verify surviving styles resolve to same content via id_map.
    for (i, (old_id, original_style)) in entries.iter().enumerate() {
        let new_id = id_map[old_id.raw() as usize];

        if dead_indices.contains(&i) {
            // Dead entries must map to default.
            assert_eq!(
                new_id,
                StyleId::DEFAULT,
                "dead style at entry index {i} must map to default"
            );
        } else {
            // Surviving entries must resolve to the same style.
            let resolved = table.get(new_id);
            assert_eq!(
                resolved,
                Some(original_style),
                "surviving style at entry index {i} must resolve to same content after compact"
            );
        }
    }

    // Verify lookup consistency: interning a surviving style returns the new ID.
    let (_, style_0) = entries[0]; // survived
    let re_interned = table.intern(style_0);
    assert_eq!(
        re_interned,
        id_map[entries[0].0.raw() as usize],
        "re-interning a surviving style must return its compacted ID"
    );
}

/// `compact` on a table with mixed live and released styles preserves all
/// live style content and removes dead entries. After compaction, re-interning
/// a surviving style must return its new compacted ID.
#[test]
fn compact_preserves_content_with_released_styles() {
    let mut table = StyleTable::new();

    let s1 = Style::with_fg(Color::new(10, 20, 30));
    let s2 = Style::with_fg(Color::new(40, 50, 60));
    let s3 = Style::with_fg(Color::new(70, 80, 90));

    let id1 = table.intern(s1);
    let id2 = table.intern(s2);
    let id3 = table.intern(s3);

    // Release the middle style to create a dead gap.
    table.release(id2);
    assert_eq!(table.stats().total_styles, 4); // default + 3

    let id_map = table.compact();

    // After compaction: default + 2 live = 3 entries.
    assert_eq!(table.stats().total_styles, 3);

    // Dead style maps to DEFAULT.
    assert_eq!(id_map[id2.raw() as usize], StyleId::DEFAULT);

    // Live styles get dense IDs and content is preserved.
    let new_id1 = id_map[id1.raw() as usize];
    let new_id3 = id_map[id3.raw() as usize];
    assert_eq!(table.get(new_id1), Some(&s1), "style 1 content");
    assert_eq!(table.get(new_id3), Some(&s3), "style 3 content");

    // Re-interning a surviving style returns its compacted ID.
    let re_interned = table.intern(s1);
    assert_eq!(re_interned, new_id1, "re-intern returns compacted ID");
}

/// `build_compaction_map` must produce the same mapping as `compact` without
/// mutating the table. Dead styles (refcount 0) map to DEFAULT; live styles
/// receive dense indices starting at 1.
#[test]
fn build_compaction_map_matches_compact() {
    let mut table = StyleTable::new();

    let mut entries: Vec<(StyleId, Style)> = Vec::new();
    for i in 1..=6u8 {
        let style = Style::with_fg(Color::new(i, i.wrapping_mul(3), i.wrapping_mul(7)));
        let id = table.intern(style);
        entries.push((id, style));
    }
    assert_eq!(table.stats().active_styles, 7); // 6 + default

    // Release styles at indices 1, 3 (0-based in entries vec).
    table.release(entries[1].0);
    table.release(entries[3].0);

    // build_compaction_map is read-only — table length must not change.
    let len_before = table.stats().active_styles;
    let (id_map, live_count) = table.build_compaction_map();
    assert_eq!(
        table.stats().active_styles,
        len_before,
        "build_compaction_map must not mutate the table"
    );
    assert_eq!(
        live_count, 4,
        "4 live styles expected (6 interned - 2 released)"
    );

    // Dead styles map to DEFAULT.
    assert_eq!(
        id_map[entries[1].0.raw() as usize],
        StyleId::DEFAULT,
        "dead style at entry 1 must map to DEFAULT"
    );
    assert_eq!(
        id_map[entries[3].0.raw() as usize],
        StyleId::DEFAULT,
        "dead style at entry 3 must map to DEFAULT"
    );

    // Live styles must receive dense IDs 1..=4 in order.
    let live_indices: Vec<u16> = [0usize, 2, 4, 5]
        .iter()
        .map(|&i| id_map[entries[i].0.raw() as usize].raw())
        .collect();
    assert_eq!(
        live_indices,
        vec![1, 2, 3, 4],
        "live styles must get dense IDs"
    );

    // Index 0 (default) always maps to DEFAULT.
    assert_eq!(id_map[0], StyleId::DEFAULT, "default slot maps to DEFAULT");
}

/// `build_compaction_map` on a table with no dead styles produces an identity
/// mapping (each ID maps to itself).
#[test]
fn build_compaction_map_all_live_is_identity() {
    let mut table = StyleTable::new();

    let id1 = table.intern(Style::with_fg(Color::new(10, 20, 30)));
    let id2 = table.intern(Style::with_bg(Color::new(40, 50, 60)));

    let (id_map, live_count) = table.build_compaction_map();
    assert_eq!(live_count, 2);
    assert_eq!(
        id_map[id1.raw() as usize],
        id1,
        "live style 1 maps to itself"
    );
    assert_eq!(
        id_map[id2.raw() as usize],
        id2,
        "live style 2 maps to itself"
    );
}
