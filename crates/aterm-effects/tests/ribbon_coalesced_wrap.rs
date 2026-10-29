// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Coalesced space-and-glyph wrap tests for the rainbow ribbon.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{Geom, GlowConfig};
use aterm_effects::rainbow_kitty::TypedClass;

struct Wrapped {
    columns: BTreeSet<u16>,
    old_edge: Option<u16>,
    old_leaving: Vec<u16>,
}

fn wrapped_columns(coalesced: bool, glyph_width: u16, boundary_class: TypedClass) -> BTreeSet<u16> {
    wrap(coalesced, glyph_width, boundary_class).columns
}

fn wrap(coalesced: bool, glyph_width: u16, boundary_class: TypedClass) -> Wrapped {
    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 30,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 480,
        head: 0,
    };
    let cfg = GlowConfig {
        theme_fg: 0x00c8_d3f5,
        theme_bg: 0x001a_1b26,
        accent: 0x007a_a2f7,
        duration: Duration::from_millis(400),
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    };
    let mut glow = crate::common::quads_glow();
    let mut quads = Vec::new();
    let mut now = Instant::now();
    const ROW: u16 = 20;
    glow.tick(Some((ROW, 2)), now, &cfg, geom, &mut quads);
    let prefix_end = 77 - glyph_width;
    for col in 3..=prefix_end {
        now += Duration::from_millis(70);
        glow.note_typed_glyph(now, 1, false, TypedClass::Glyph);
        glow.tick(Some((ROW, col)), now, &cfg, geom, &mut quads);
    }
    now += Duration::from_millis(70);
    glow.note_typed_glyph(now, 1, false, boundary_class);
    if !coalesced {
        glow.tick(Some((ROW, prefix_end + 1)), now, &cfg, geom, &mut quads);
    }
    now += Duration::from_millis(4);
    glow.note_typed_glyph(now, glyph_width, false, TypedClass::Glyph);
    now += Duration::from_millis(12);
    glow.tick(Some((ROW, 78)), now, &cfg, geom, &mut quads);

    now += Duration::from_millis(70);
    glow.note_typed_glyph(now, 1, false, TypedClass::Glyph);
    glow.note_scroll(1);
    let landing = 3 + glyph_width;
    glow.tick(Some((ROW, landing)), now, &cfg, geom, &mut quads);
    now += Duration::from_millis(40);
    glow.tick(Some((ROW, landing)), now, &cfg, geom, &mut quads);
    let ribbon = glow.v2_ribbon().expect("rainbow engine active");
    let columns = ribbon
        .cells()
        .iter()
        .filter(|cell| cell.row == ROW && cell.typing && !cell.leaving())
        .map(|cell| cell.col)
        .collect();
    let old = ribbon
        .cohorts()
        .iter()
        .find(|k| k.row == ROW - 1 && k.flow.is_some())
        .copied();
    let old_leaving = old.map_or_else(Vec::new, |k| {
        ribbon
            .cells()
            .iter()
            .filter(|cell| cell.cohort == k.id && cell.leaving())
            .map(|cell| cell.col)
            .collect()
    });
    let old_edge = old.and_then(|k| k.retract_col);
    let admission: Vec<_> = glow
        .admission_log()
        .map(|record| record.line(now))
        .collect();
    let tail = &admission[admission.len().saturating_sub(4)..];
    println!(
        "coalesced={coalesced}, glyph_width={glyph_width}, lower-row columns={columns:?}, \
         old edge={old_edge:?}, old leaving={old_leaving:?}, admission={tail:?}"
    );
    Wrapped {
        columns,
        old_edge,
        old_leaving,
    }
}

#[test]
fn a_space_and_glyph_wrap_keeps_the_moved_word_lit_and_the_old_row_flows_into_its_kept_end() {
    for (coalesced, width) in [(false, 1u16), (true, 1), (true, 2)] {
        let what = format!("coalesced={coalesced} width={width}");
        let w = wrap(coalesced, width, TypedClass::Space);
        let want: BTreeSet<u16> = (2..4 + width - 1).collect();
        assert!(
            w.columns.is_superset(&want),
            "{what}: the moved word is dark: {:?}",
            w.columns
        );
        if width == 2 {
            assert_eq!(w.columns, want, "{what}: both moved cells, and only them");
        }
        let kept_end = 76 - width;
        assert!(
            w.old_leaving.iter().any(|&col| col > kept_end),
            "{what}: the fixture's point — the vacated cells are still leaving in the \
             flowing band: {:?}",
            w.old_leaving
        );
        assert_eq!(
            w.old_edge,
            Some(kept_end),
            "{what}: the old row flows into its kept end"
        );
    }
}

#[test]
fn the_space_class_is_required_to_relocate_the_word() {
    let spaced = wrapped_columns(true, 1, TypedClass::Space);
    let plain = wrapped_columns(true, 1, TypedClass::Glyph);
    assert!(spaced.contains(&2), "the moved word must be lit");
    assert!(
        !plain.contains(&2),
        "plain Glyph cannot invent a word boundary"
    );
    assert!(plain.contains(&3), "the new key itself still lights");
}
