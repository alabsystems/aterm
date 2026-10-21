// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Free-floating overlay layer: the `RenderInput.free_sprites` + `free_atlas` contract
// (FREE_OVERLAY_LAYER_DESIGN.md §3.1/§3.3), i.e. `RenderInput::eq` and the row-union dirty
// gate. The empty-layer no-op is the `free_sprites` row of `tests/empty_channels.rs`.

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ, SceneAtlas};
use aterm_core::terminal::Terminal;
use aterm_render::{DirtyDecision, compute_dirty_rows};

/// The cell height every `compute_dirty_rows` call below uses (row `r` spans grid-interior
/// `[r·16, (r+1)·16)`).
const CELL_H: usize = 16;

/// A deterministic, fully opaque patterned RGBA atlas.
fn patterned_atlas(w: u32, h: u32, version: u64) -> SceneAtlas {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 37 + y * 11) as u8,
                (x * 5 + y * 53) as u8,
                (x * 29 + y * 3) as u8,
                255,
            ]);
        }
    }
    SceneAtlas {
        width: w,
        height: h,
        rgba,
        version,
    }
}

/// Opaque untinted under-text NEAREST sprite at 1:1 (`aw==w`, `ah==h`); the dest origin is
/// signed grid-interior pixels.
fn free(x: i32, y: i32, w: u16, h: u16) -> FreeSprite {
    FreeSprite {
        x,
        y,
        w,
        h,
        ax: 0,
        ay: 0,
        aw: w,
        ah: h,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    }
}

fn marked(dirty: &[bool]) -> Vec<usize> {
    dirty
        .iter()
        .enumerate()
        .filter_map(|(r, &b)| b.then_some(r))
        .collect()
}

#[test]
fn render_input_eq_compares_sprites_by_value_and_atlas_by_identity() {
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25l");
    let mut a = term.cell_frame(3, 12);
    let mut b = term.cell_frame(3, 12);
    a.free_sprites = vec![free(2, 5, 8, 8)];
    b.free_sprites = vec![free(2, 5, 8, 8)];
    // Atlases compare by snapshot identity (`Arc::as_ptr`), never `version`: baker versions
    // are per engine instance, so a rebuilt engine replays its predecessor's versions with
    // different texels.
    a.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 7)));
    b.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 7)));
    assert_ne!(
        a, b,
        "a same-version DIFFERENT-Arc publish (rebuilt engine) compares UNEQUAL"
    );

    // The stable steady state: the same published snapshot re-presented.
    b.free_atlas.clone_from(&a.free_atlas);
    assert_eq!(
        a, b,
        "re-presenting the same published Arc stays EQUAL (a settled overlay is free)"
    );

    b.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 8)));
    assert_ne!(a, b, "a fresh atlas publish must compare unequal");

    // Sprites compare by value; hold the atlas identity fixed.
    b.free_atlas.clone_from(&a.free_atlas);
    b.free_sprites[0].z = FreeZ::OverText;
    assert_ne!(a, b, "a same-rect z flip is a real content change");

    b.free_sprites[0].z = FreeZ::UnderText;
    b.free_sprites[0].sampler = FreeSampler::Linear;
    assert_ne!(a, b, "a same-rect sampler flip is a real content change");
}

/// Row-union dirty gate: settled sprites gate-hit with no rows marked; a moved rect marks
/// exactly the prev∪cur bands it overlaps; a same-rect z flip un-gates; a new atlas alone
/// (equal sprites) marks the sprite's own bands.
#[test]
fn free_dirty_gate_settled_hits_moved_marks_row_union() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let atlas_v1 = Arc::new(patterned_atlas(64, 64, 1));
    // `[20, 60)` at CELL_H=16 spans bands 1..=3, which no single-band SpriteQuad can carry.
    let settled = vec![free(4, 20, 16, 40)];

    let mut prev = term.cell_frame(6, 8);
    let mut cur = term.cell_frame(6, 8);
    prev.free_atlas = Some(atlas_v1.clone());
    prev.free_sprites = settled.clone();
    cur.free_atlas = Some(atlas_v1.clone());
    cur.free_sprites = settled.clone();
    let mut dirty = Vec::new();

    // Settled: equal non-empty sprites + same atlas ⇒ gate hit, nothing marked.
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        !d.free_changed,
        "equal sprites + same atlas must not set free_changed"
    );
    assert!(
        d.is_gate_hit(),
        "settled (non-empty but equal) free sprites must gate-hit: steady state is free"
    );
    assert!(
        dirty.iter().all(|&b| !b),
        "settled sprites must mark no rows"
    );

    // Moved: y 20 → 36 (bands 2..=4) ⇒ prev∪cur marks exactly [1, 2, 3, 4].
    cur.free_sprites = vec![free(4, 36, 16, 40)];
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.free_changed, "a moved sprite must set free_changed");
    assert!(!d.is_gate_hit(), "a moved sprite must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 2, 3, 4],
        "a moved rect must mark the prev∪cur bands its pixel extent overlaps"
    );

    // Same-rect z flip: a real content change the gate must not swallow.
    cur.free_sprites = settled.clone();
    cur.free_sprites[0].z = FreeZ::OverText;
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.free_changed, "a same-rect z flip must set free_changed");
    assert!(!d.is_gate_hit(), "a z flip must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 2, 3],
        "a z flip marks the (unmoved) sprite's own bands"
    );

    // A new atlas with byte-equal sprites: a rebake must repaint.
    cur.free_sprites = settled;
    cur.free_atlas = Some(Arc::new(patterned_atlas(64, 64, 2)));
    let DirtyDecision::Rows(d) = compute_dirty_rows(
        &prev,
        &cur,
        false,
        None,
        false,
        None,
        aterm_render::RowBands::new(0, CELL_H),
        &mut dirty,
    ) else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(
        d.free_changed,
        "an atlas-version bump alone must set free_changed (a rebake repaints)"
    );
    assert!(!d.is_gate_hit(), "a rebaked atlas must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 2, 3],
        "the rebake marks the (unmoved) sprite's bands"
    );
}
