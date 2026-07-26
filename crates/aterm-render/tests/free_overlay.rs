// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Free-floating overlay layer, Phase 0 (FREE_OVERLAY_LAYER_DESIGN.md §3.1/§3.3):
// the `RenderInput.free_sprites` + `free_atlas` contract plumbing, consumed by
// NO renderer yet. The contract under test:
//   * the empty free layer (sprites empty / atlas absent) is byte-identical to
//     the pre-layer path, also after `clear_overlays` (the `image plain`
//     contract), and `RenderInput::eq` compares sprites by full value (incl.
//     the `z`/`sampler` enums) and the atlas by VERSION only;
//   * dirty-ROW gate (row-union over the true pixel Y-extent, prev∪cur):
//     settled sprites gate-hit with zero rows marked; a moved rect marks
//     exactly the prev∪cur bands its `[y, y+h)` extent overlaps; a same-rect
//     z flip un-gates (the `eq` half of the z-flip guard — the fingerprint
//     half lands with `fold_free` in Phase 3); an atlas-version bump alone
//     marks the sprite's own bands;
//   * i32 off-grid: a negative-y top peek marks row 0; a below-bottom peek
//     marks the last row (the pad-strip spill force-marks).

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ, SceneAtlas};
use aterm_core::terminal::Terminal;
use aterm_render::{DirtyDecision, Renderer, Theme, compute_dirty_rows};

/// The cell height every direct `compute_dirty_rows` drive below uses: the
/// row→pixel mapping constant for the row-union assertions (row `r` spans
/// grid-interior `[r·16, (r+1)·16)`).
const CELL_H: usize = 16;

fn renderer() -> Option<Renderer> {
    Renderer::from_system(18.0, Theme::default())
}

/// A deterministic, fully-opaque patterned RGBA atlas (same shape as the cat
/// suite's) so a future consuming phase can reuse these fixtures unchanged.
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

/// Opaque untinted under-text NEAREST sprite at 1:1 (`aw==w`, `ah==h`), the
/// default regime; dest origin is SIGNED grid-interior pixels.
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
fn empty_free_fields_are_byte_identical_also_after_clear_overlays() {
    let Some(mut rend) = renderer() else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25lfree layer");

    let base = rend.render_input(&term.cell_frame(3, 12)).pixels.clone();

    // Empty sprites + NO atlas (the common off state).
    let mut input = term.cell_frame(3, 12);
    assert!(input.free_sprites.is_empty() && input.free_atlas.is_none());
    let again = rend.render_input(&input).pixels.clone();
    assert_eq!(base, again, "empty free fields must not change any pixel");

    // Empty sprites WITH an atlas set: the atlas alone draws nothing.
    input.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
    let atlas_only = rend.render_input(&input).pixels.clone();
    assert_eq!(
        base, atlas_only,
        "a free atlas with no sprites must draw nothing"
    );

    // `clear_overlays` (the `image plain` capture) strips the free layer like
    // every other bling layer: sprites cleared AND the atlas Arc nulled.
    let mut with_free = term.cell_frame(3, 12);
    with_free.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 1)));
    with_free.free_sprites = vec![free(2, 5, 16, 16)];
    with_free.clear_overlays();
    assert!(
        with_free.free_sprites.is_empty(),
        "clear_overlays must strip free sprites"
    );
    assert!(
        with_free.free_atlas.is_none(),
        "clear_overlays must null the free atlas Arc"
    );
    let cleared = rend.render_input(&with_free).pixels.clone();
    assert_eq!(
        base, cleared,
        "a cleared free layer must render the bare screen"
    );
}

#[test]
fn render_input_eq_compares_sprites_by_value_and_atlas_by_identity() {
    let mut term = Terminal::new(3, 12);
    term.process(b"\x1b[?25l");
    let mut a = term.cell_frame(3, 12);
    let mut b = term.cell_frame(3, 12);
    a.free_sprites = vec![free(2, 5, 8, 8)];
    b.free_sprites = vec![free(2, 5, 8, 8)];
    // ATLASES compare by SNAPSHOT IDENTITY (`Arc::as_ptr`), never `version`
    // (split-pane audit): baker versions are deterministic PER ENGINE INSTANCE,
    // so a rebuilt engine replays its predecessor's version sequence with
    // different texels. Two DISTINCT Arcs at the same version are exactly that
    // case — calling them equal is the stale-atlas aliasing the audit outlawed.
    a.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 7)));
    b.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 7)));
    assert_ne!(
        a, b,
        "a same-version DIFFERENT-Arc publish (rebuilt engine) compares UNEQUAL"
    );

    // The stable steady state: the SAME published snapshot re-presented.
    b.free_atlas.clone_from(&a.free_atlas);
    assert_eq!(
        a, b,
        "re-presenting the same published Arc stays EQUAL (a settled overlay is free)"
    );

    b.free_atlas = Some(Arc::new(patterned_atlas(16, 16, 8)));
    assert_ne!(a, b, "a fresh atlas publish must compare unequal");

    // SPRITES stay by VALUE — hold the atlas identity fixed so only the sprite
    // field under test differs.
    b.free_atlas.clone_from(&a.free_atlas);
    b.free_sprites[0].z = FreeZ::OverText;
    assert_ne!(a, b, "a same-rect z flip is a real content change");

    b.free_sprites[0].z = FreeZ::UnderText;
    b.free_sprites[0].sampler = FreeSampler::Linear;
    assert_ne!(a, b, "a same-rect sampler flip is a real content change");
}

/// Dirty gate (row-union): settled free sprites — non-empty but EQUAL, same
/// atlas version — gate-hit with zero rows marked; a moved rect marks exactly
/// the prev∪cur bands its pixel Y-extent overlaps; a same-rect z flip
/// un-gates; an atlas-version bump alone (equal sprites) marks the sprite's
/// own bands.
#[test]
fn free_dirty_gate_settled_hits_moved_marks_row_union() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l"); // hidden cursor: no cursor rows in the dirty set
    let atlas_v1 = Arc::new(patterned_atlas(64, 64, 1));
    // `[20, 60)` at CELL_H=16 spans bands 1..=3 — a THREE-band rect no
    // SpriteQuad could carry.
    let settled = vec![free(4, 20, 16, 40)];

    let mut prev = term.cell_frame(6, 8);
    let mut cur = term.cell_frame(6, 8);
    prev.free_atlas = Some(atlas_v1.clone());
    prev.free_sprites = settled.clone();
    cur.free_atlas = Some(atlas_v1.clone());
    cur.free_sprites = settled.clone();
    let mut dirty = Vec::new();

    // Settled: equal non-empty sprites + same atlas ⇒ gate hit, nothing marked.
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
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
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.free_changed, "a moved sprite must set free_changed");
    assert!(!d.is_gate_hit(), "a moved sprite must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 2, 3, 4],
        "a moved rect must mark the prev∪cur bands its pixel extent overlaps"
    );

    // Same-rect z flip: the `eq` side of the z-flip guard — a real content
    // change, so the Tier-1 gate must NOT swallow it.
    cur.free_sprites = settled.clone();
    cur.free_sprites[0].z = FreeZ::OverText;
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.free_changed, "a same-rect z flip must set free_changed");
    assert!(!d.is_gate_hit(), "a z flip must NOT gate-hit");
    assert_eq!(
        marked(&dirty),
        vec![1, 2, 3],
        "a z flip marks the (unmoved) sprite's own bands"
    );

    // Atlas-version bump with byte-equal sprites: a rebake must repaint.
    cur.free_sprites = settled;
    cur.free_atlas = Some(Arc::new(patterned_atlas(64, 64, 2)));
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
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

/// i32 off-grid origins: a sprite peeking in from ABOVE the grid (negative
/// pad-relative `y`) marks row 0; one rising from BELOW the bottom edge marks
/// the last row — the pad-strip spill force-marks, so the edge scissor (which
/// already covers the pad) repaints the peek.
#[test]
fn free_dirty_gate_off_grid_extents_mark_edge_rows() {
    let mut term = Terminal::new(6, 8);
    term.process(b"\x1b[?25l");
    let atlas = Arc::new(patterned_atlas(64, 64, 1));
    let prev = term.cell_frame(6, 8);
    let mut dirty = Vec::new();

    // Top peek: `[-8, 4)` — the on-grid slice lives in band 0, the rest in the
    // top pad strip.
    let mut cur = term.cell_frame(6, 8);
    cur.free_atlas = Some(atlas.clone());
    cur.free_sprites = vec![free(0, -8, 16, 12)];
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.free_changed, "an appearing sprite must set free_changed");
    assert_eq!(
        marked(&dirty),
        vec![0],
        "a negative-y top peek must mark row 0 (and only row 0)"
    );

    // Bottom peek: `[90, 110)` at 6×16 = 96 grid px — band 5 plus the bottom
    // pad strip.
    cur.free_sprites = vec![free(0, 90, 16, 20)];
    let DirtyDecision::Rows(d) =
        compute_dirty_rows(&prev, &cur, false, None, false, None, CELL_H, &mut dirty)
    else {
        panic!("identical-geometry frames must take the row-damage path");
    };
    assert!(d.free_changed, "an appearing sprite must set free_changed");
    assert_eq!(
        marked(&dirty),
        vec![5],
        "a below-bottom peek must mark the last row (and only the last row)"
    );
}
