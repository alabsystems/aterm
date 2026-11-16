// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flying cat's glide body as the word layer draws it: its footprint is its ink box, it is
//! baked by the walker's own baker in the walker's look, a face its bake budget misses is held,
//! and it bakes once per face.

use super::*;
use crate::kitty_cursor::{CatPose, CursorBody, CursorCatPlacementFrame};
use crate::kitty_pet::{PetSpecies, glide_faces};
use crate::pet_baker::PetBakeKey;
use crate::pet_glyphs_gen::PetGlyphId;

fn geom(cell_h: u16) -> EffectGeom {
    EffectGeom {
        cell_w: (cell_h / 2).max(1),
        cell_h,
        rows: 8,
        cols: 40,
    }
}

fn glide_frame(
    g: EffectGeom,
    cursor: (u16, u16),
    face: PetGlyphId,
    colors: CatColorKey,
    pose: CatPose,
) -> KittyCursorFrame {
    KittyCursorFrame {
        geom: g,
        cursor,
        look: KittyLook {
            coat: 12,
            iris: 3,
            ..KittyLook::default()
        },
        body: CursorBody::Glide(face),
        colors,
        bob: 0.0,
        lift: 0.0,
        alpha: 255,
        pose,
        sing: 0.0,
        notes: [None; crate::kitty_sing::MAX_NOTES],
    }
}

fn layout_of(frame: &KittyCursorFrame) -> KittyCursorLayout {
    KittyCursorLayout {
        geom: frame.geom,
        cursor: frame.cursor,
        look: frame.look,
        body: frame.body,
        bob: frame.bob,
        lift: frame.lift,
    }
}

fn motion(at: Instant, facing_left: bool) -> CursorCatPlacementFrame {
    CursorCatPlacementFrame {
        sampled_at: at,
        fold: None,
        facing_left,
    }
}

/// The key the walker's baker bakes `face` under for `frame`.
fn pet_key(frame: &KittyCursorFrame, face: PetGlyphId) -> PetBakeKey {
    let (w, h) = crate::kitty_pet::glide_tile_size(frame.geom.cell_h);
    let look = frame.look.normalized();
    PetBakeKey {
        pose: face,
        coat: look.coat,
        iris: look.iris,
        colors: frame.colors,
        w,
        h,
    }
}

/// Draw `frame` facing `left`: the emitted sprite and the footprint the laws judge.
fn draw(
    wd: &mut WordDecorations,
    frame: KittyCursorFrame,
    left: bool,
    at: Instant,
) -> Option<(FreeSprite, CatFootprint)> {
    wd.cat_baker_ready = false;
    let placement =
        wd.prepare_kitty_cursor_placement(layout_of(&frame), motion(at, left), frame.pose)?;
    let footprint = placement.drawn_footprint()?;
    let mut free = Vec::new();
    wd.kitty_cursor_at_placement(frame, placement, &mut free)?;
    Some((free.pop()?, footprint))
}

/// The inked texels of `tile` (`tw` x `th`, RGBA) drawn at `sprite` that reach outside
/// `footprint`, each texel taken as the whole device-pixel area it covers.
fn escaped(
    tile: &[u8],
    (tw, th): (u16, u16),
    sprite: &FreeSprite,
    footprint: CatFootprint,
) -> usize {
    let kx = f32::from(sprite.w) / f32::from(tw);
    let ky = f32::from(sprite.h) / f32::from(th);
    let (fx0, fy0) = (footprint.x as f32, footprint.y as f32);
    let (fx1, fy1) = (fx0 + f32::from(footprint.w), fy0 + f32::from(footprint.h));
    let mut out = 0;
    for ty in 0..usize::from(th) {
        for tx in 0..usize::from(tw) {
            if tile[(ty * usize::from(tw) + tx) * 4 + 3] == 0 {
                continue;
            }
            let col = if sprite.flip_x {
                usize::from(tw) - 1 - tx
            } else {
                tx
            };
            let x0 = sprite.x as f32 + col as f32 * kx;
            let y0 = sprite.y as f32 + ty as f32 * ky;
            if x0 < fx0 - 1e-3 || x0 + kx > fx1 + 1e-3 || y0 < fy0 - 1e-3 || y0 + ky > fy1 + 1e-3 {
                out += 1;
            }
        }
    }
    out
}

/// Every texel the flier's glide inks, at the rect it is drawn at, lies inside the footprint
/// every seat and letter law judges: at every cell height, for both species and every face, in
/// either facing, squashed and stretched, on every background band (band 4 draws the outline's
/// halo). Control: with no pad round the ink box, some texel of the halo or the antialiasing
/// escapes it.
#[test]
fn the_glide_footprint_holds_every_inked_texel() {
    let t0 = Instant::now();
    let pose = CatPose {
        scale_x: 1.17,
        scale_y: 0.90,
        ..CatPose::STILL
    };
    let sweep = |pad_off: bool| {
        let mut escapes = 0;
        let mut drawn = 0;
        for cell_h in 12..=48_u16 {
            let g = geom(cell_h);
            for species in [PetSpecies::Cat, PetSpecies::Dog] {
                for face in glide_faces(species).all() {
                    for background in [0, 3, 4] {
                        let colors = CatColorKey {
                            accent: 12,
                            background,
                        };
                        for left in [false, true] {
                            let mut wd = WordDecorations::default();
                            wd.set_glide_pad_off(pad_off);
                            let frame = glide_frame(g, (3, 10), face, colors, pose);
                            let (sprite, footprint) = draw(&mut wd, frame, left, t0)
                                .unwrap_or_else(|| panic!("{face:?} at {cell_h} px drew"));
                            let key = pet_key(&frame, face);
                            assert_eq!(
                                (sprite.art.w(), sprite.art.h()),
                                (key.w, key.h),
                                "the tile is the glide's"
                            );
                            let tile = key.bake();
                            escapes += escaped(tile.pixels(), (key.w, key.h), &sprite, footprint);
                            drawn += 1;
                        }
                    }
                }
            }
        }
        (escapes, drawn)
    };
    let (escapes, drawn) = sweep(false);
    assert_eq!(drawn, 37 * 2 * 4 * 3 * 2);
    assert_eq!(
        escapes, 0,
        "{escapes} inked texels lie outside the glide's footprint"
    );
    let (bare, _) = sweep(true);
    assert!(
        bare > 0,
        "control: with no pad the halo and the antialiasing must escape the ink box"
    );
}

/// A glide face the bake budget misses is drawn as the last face was, at full alpha, never
/// dropped: a dip reads as a blink. Control: `GLIDE_HOLD_OFF` drops it.
#[test]
fn a_glide_face_swap_never_drops_a_frame() {
    let t0 = Instant::now();
    let g = geom(26);
    let colors = CatColorKey::default();
    let faces = glide_faces(PetSpecies::Cat);
    let swap = |hold: bool| {
        GLIDE_HOLD_OFF.with(|c| c.set(!hold));
        let mut wd = WordDecorations::default();
        let (fly, _) = draw(
            &mut wd,
            glide_frame(g, (3, 10), faces.fly, colors, CatPose::STILL),
            false,
            t0,
        )
        .expect("the cruise face is drawn");
        // A new frame whose bake budget other art has spent.
        wd.cat_baker_ready = false;
        wd.begin_baker_frame(g, true);
        let spent = [0u8; 4];
        while wd.cat_baker.can_bake() {
            let id = 0x7000 + u64::from(wd.cat_baker.version() as u32);
            let _ = wd.cat_baker.host_tile(id, 1, 1, &spent);
        }
        let frame = glide_frame(g, (3, 10), faces.oof, colors, CatPose::STILL);
        let placement = wd
            .prepare_kitty_cursor_placement(layout_of(&frame), motion(t0, false), frame.pose)
            .expect("placed");
        let mut free = Vec::new();
        let drew = wd.kitty_cursor_at_placement(frame, placement, &mut free);
        GLIDE_HOLD_OFF.with(|c| c.set(false));
        (fly, drew.and(free.pop()))
    };
    let (fly, held) = swap(true);
    let held = held.expect("the swap frame drew a sprite");
    assert_eq!(held.alpha, 255, "at full alpha");
    assert!(
        Arc::ptr_eq(&held.art, &fly.art),
        "the missed face is drawn as the last face was"
    );
    let (_, dropped) = swap(false);
    assert!(dropped.is_none(), "control: GLIDE_HOLD_OFF drops the frame");
}

/// The flier is the walker's art through the walker's baker: the tile it draws is the one
/// `PetBakeKey` names for its face, in the coat and iris its look wears. Control: a head draws
/// the cat baker's head tile, which is not it.
#[test]
fn the_flier_draws_the_glide_in_the_walkers_coat() {
    let t0 = Instant::now();
    let g = geom(26);
    let colors = CatColorKey {
        accent: 9,
        background: 0,
    };
    let frame = glide_frame(g, (3, 10), PetGlyphId::PetFly, colors, CatPose::STILL);
    let mut wd = WordDecorations::default();
    let (sprite, _) = draw(&mut wd, frame, false, t0).expect("drawn");
    let key = pet_key(&frame, PetGlyphId::PetFly);
    let resident = wd
        .cat_baker
        .host_peek(key.host_id())
        .expect("the pet key's tile");
    assert!(
        Arc::ptr_eq(&sprite.art, &resident.art),
        "the glide is PetBakeKey's tile"
    );
    assert_eq!(
        (key.coat, key.iris),
        (12, 3),
        "in the walker's coat and iris"
    );
    let head = KittyCursorFrame {
        body: CursorBody::Head,
        ..frame
    };
    let mut wd = WordDecorations::default();
    let (head_sprite, _) = draw(&mut wd, head, false, t0).expect("a head drawn");
    assert!(
        wd.cat_baker
            .host_peek(key.host_id())
            .is_none_or(|t| !Arc::ptr_eq(&t.art, &head_sprite.art)),
        "control: a head is not the glide's tile"
    );
}

/// No typing cost: over a 6 s burst at fixed colours the walker's baker bakes once per face
/// the flier shows, and never again. Control: the same count over a run whose cell height
/// changes every frame bakes every frame, so the count counts.
#[test]
fn the_flier_bakes_once_per_face() {
    let t0 = Instant::now();
    let colors = CatColorKey::default();
    let run = |resize: bool| {
        let mut cat = crate::kitty_cursor::CursorCat::default();
        cat.set_body(Some(PetSpecies::Cat));
        let mut wd = WordDecorations::default();
        let mut faces = Vec::new();
        let mut bakes = 0;
        let mut last = wd.pet_baker.version();
        for frame_i in 0..(6000 / 16) {
            let at = t0 + Duration::from_millis(16 * frame_i);
            if frame_i % 2 == 0 {
                cat.on_key(at, true);
            }
            if cat.is_active() && frame_i % 61 == 60 {
                cat.on_key(at, false);
            }
            let f = cat.frame(at);
            let CursorBody::Glide(face) = f.body else {
                continue;
            };
            let cell_h = if resize {
                20 + (frame_i % 2) as u16
            } else {
                26
            };
            let frame = KittyCursorFrame {
                bob: f.bob,
                ..glide_frame(geom(cell_h), (3, 10), face, colors, f.pose)
            };
            if draw(&mut wd, frame, false, at).is_some() && !faces.contains(&face) {
                faces.push(face);
            }
            let now = wd.pet_baker.version();
            if now != last {
                bakes += 1;
                last = now;
            }
        }
        (faces.len(), bakes)
    };
    let (faces, bakes) = run(false);
    assert!(
        faces >= 2,
        "the burst must show more than one face: {faces}"
    );
    assert!(
        bakes <= faces,
        "{bakes} frames baked for {faces} faces: a bake on a frame that showed no new face"
    );
    let (_, churn) = run(true);
    assert!(
        churn > faces * 4,
        "control: a cell that changes every frame bakes every frame ({churn})"
    );
}

/// A face swap never moves or resizes the flier: every glide face of a species lays out the one
/// footprint, at every cell height and either facing, so the reaction that swaps a face cannot
/// step the pet onto a letter or the caret. Control: the heads the companion wears (S121, S115,
/// the maneki) lay out footprints that differ, so the comparison reads the art.
#[test]
fn every_glide_face_lays_out_one_footprint() {
    let t0 = Instant::now();
    let footprint = |frame: KittyCursorFrame, left: bool| {
        let mut wd = WordDecorations::default();
        wd.prepare_kitty_cursor_placement(layout_of(&frame), motion(t0, left), frame.pose)
            .and_then(|p| p.drawn_footprint())
            .map(|f| (f.x, f.y, f.w, f.h))
    };
    let colors = CatColorKey::default();
    for cell_h in 12..=48_u16 {
        let g = geom(cell_h);
        for left in [false, true] {
            for species in [PetSpecies::Cat, PetSpecies::Dog] {
                let faces = glide_faces(species).all();
                let one = footprint(
                    glide_frame(g, (3, 10), faces[0], colors, CatPose::STILL),
                    left,
                )
                .expect("laid out");
                for face in faces {
                    assert_eq!(
                        footprint(glide_frame(g, (3, 10), face, colors, CatPose::STILL), left),
                        Some(one),
                        "{face:?} at {cell_h} px moves the flier"
                    );
                }
            }
        }
    }
    let heads: Vec<_> = [CatGlyphId::S121, CatGlyphId::S115, CatGlyphId::SpecManeki]
        .into_iter()
        .map(|variant| {
            let frame = glide_frame(
                geom(26),
                (3, 10),
                PetGlyphId::PetFly,
                colors,
                CatPose::STILL,
            );
            footprint(
                KittyCursorFrame {
                    body: CursorBody::Head,
                    look: KittyLook {
                        variant,
                        ..frame.look
                    },
                    ..frame
                },
                false,
            )
        })
        .collect();
    assert!(
        heads.windows(2).any(|w| w[0] != w[1]),
        "control: the heads' footprints differ: {heads:?}"
    );
}
