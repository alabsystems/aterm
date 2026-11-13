// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{FirePatch, GlowQuad, HostRowPixels, RainHalo, RenderInput};
use crate::terminal::RenderCell;

const CH: usize = 10;
const TOP: usize = 6;

fn y(row: usize) -> u16 {
    u16::try_from(TOP + row * CH).expect("small")
}

fn frame() -> RenderInput {
    let blank = RenderCell {
        ch: ' ',
        ..Default::default()
    };
    RenderInput {
        rows: 4,
        cols: 3,
        cells: vec![vec![blank; 3]; 4],
        cursor_glow_add: vec![
            GlowQuad {
                row: 2,
                y: y(2) + 1,
                h: 4,
                ..Default::default()
            },
            GlowQuad {
                row: 0,
                y: 2,
                h: 3,
                ..Default::default()
            },
        ],
        glow_under: vec![GlowQuad {
            row: 2,
            y: y(2),
            h: 4,
            ..Default::default()
        }],
        fire_patch: vec![FirePatch {
            row: 2,
            y: y(2),
            h: 10,
            base_y: y(3),
            ..Default::default()
        }],
        glow_halo: vec![RainHalo {
            row: 1,
            y: y(1),
            h: 10,
            cy: y(1) + 5,
            rx: 1,
            ry: 1,
            ..Default::default()
        }],
        fx_clip: Some((0, y(0), 30, y(4))),
        ..Default::default()
    }
}

fn placed_below(mut f: RenderInput, dy: u16) -> RenderInput {
    for q in &mut f.cursor_glow_add {
        q.y += dy;
    }
    for q in &mut f.glow_under {
        q.y += dy;
    }
    for q in &mut f.fire_patch {
        q.y += dy;
        q.base_y += dy;
    }
    for h in &mut f.glow_halo {
        h.y += dy;
        h.cy += dy;
    }
    if let Some((_, y0, _, y1)) = &mut f.fx_clip {
        *y0 += dy;
        *y1 += dy;
    }
    f
}

#[test]
fn translate_is_the_native_splice_of_a_frame_placed_below_the_rows() {
    let strip = 2;
    let dy = u16::try_from(strip * CH).expect("small");
    let rows = vec![vec![RenderCell::default(); 3]; strip];
    let prepend = |mut f: RenderInput, pixels| {
        f.prepend_host_rows(
            strip,
            rows.iter().map(Vec::as_slice),
            CH,
            TOP,
            &mut Vec::new(),
            pixels,
        );
        f
    };
    let web = prepend(frame(), HostRowPixels::Translate);
    let native = prepend(placed_below(frame(), dy), HostRowPixels::Placed);
    assert!(web == native, "every channel the splice moves");
    assert_eq!(web.cursor_glow_add, native.cursor_glow_add);
    assert_eq!(web.glow_under, native.glow_under);
    assert_eq!(web.fire_patch, native.fire_patch);
    assert_eq!(web.glow_halo, native.glow_halo);
    assert_eq!(web.fx_clip, native.fx_clip);
    assert_eq!(web.cursor_glow_add[0].row, 4);
    assert_eq!(web.cursor_glow_add[1].row, 1);
    assert_eq!(web.glow_under[0].row, 4);
    assert_eq!(web.fire_patch[0].row, 4);
    assert_eq!(web.fire_patch[0].base_y, y(3) + dy);
    assert_eq!(web.glow_halo[0].row, 3);
    assert_eq!(web.glow_halo[0].cy, y(1) + 5 + dy);
    let placed = prepend(frame(), HostRowPixels::Placed);
    assert_eq!(
        placed.cursor_glow_add[0].y,
        y(2) + 1,
        "Placed moves no pixel"
    );
    assert_eq!(placed.fx_clip, frame().fx_clip);
    assert_eq!(
        placed.cursor_glow_add[0].row, 2,
        "the negative control: the web frame under Placed keeps its old row"
    );
}
#[test]
fn a_prepend_moves_each_bidi_row_map_with_its_row() {
    use crate::selection::{SelectionSide, SelectionType};
    let mut f = frame();
    f.visual_to_logical = vec![Vec::new(), vec![0, 2, 1], Vec::new(), Vec::new()];
    f.selection
        .start_selection(1, 2, SelectionSide::Left, SelectionType::Simple);
    f.selection.update_selection(1, 2, SelectionSide::Right);
    f.selection.complete_selection();
    assert!(f.selection_contains_cell(1, 1, false, false));
    assert!(!f.selection_contains_cell(1, 2, false, false));

    let strip = 2;
    let rows = vec![vec![RenderCell::default(); 3]; strip];
    f.prepend_host_rows(
        strip,
        rows.iter().map(Vec::as_slice),
        CH,
        TOP,
        &mut Vec::new(),
        HostRowPixels::Placed,
    );
    assert!(f.visual_to_logical_row(0).is_empty() && f.visual_to_logical_row(1).is_empty());
    assert_eq!(f.visual_to_logical_row(3), [0, 2, 1]);
    assert!(
        f.selection_contains_cell(3, 1, false, false),
        "the highlight moved down with its row"
    );
    assert!(!f.selection_contains_cell(3, 2, false, false));
}

#[test]
fn frames_compare_bidi_maps_by_meaning() {
    let plain = frame();
    let mut filled = frame();
    filled.visual_to_logical = vec![Vec::new(); 4];
    assert!(plain == filled);
    let mut reordered = frame();
    reordered.visual_to_logical = vec![Vec::new(), vec![1, 0]];
    assert!(plain != reordered);
}
