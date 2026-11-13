// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const GEOM: EffectGeom = EffectGeom {
    cell_w: 9,
    cell_h: 19,
    rows: 24,
    cols: 80,
};

const fn sleeper_on_the_top_row() -> crate::kitty_pet::PetFrame {
    let mut motes = [None; crate::kitty_pet::PET_MOTES_MAX];
    motes[0] = Some(crate::kitty_pet::PetMoteSprite {
        kind: crate::kitty_pet::PetMoteKind::Zee,
        col: 12.06,
        row: -0.66,
        scale: 0.58,
        rot: 0.4,
        alpha: 200,
        rgb: None,
    });
    crate::kitty_pet::PetFrame {
        alpha: 255,
        lane_alpha: 255,
        action: crate::kitty_pet::PetAction::Sleep,
        pose: crate::pet_glyphs_gen::PetGlyphId::PetSleep0,
        col: 13.0,
        row: 0.0,
        lift: 0.0,
        facing_left: false,
        scale_x: 1.0,
        scale_y: 1.0,
        stretch_from: crate::kitty_pet::StretchFrom::Centre,
        purr: 0.0,
        contented: false,
        under_ink: false,
        motes,
        departures: [None; crate::kitty_pet::PET_DEPARTURES_MAX],
    }
}

#[test]
fn a_metric_change_drops_every_held_tile() {
    let mut wd = WordDecorations::default();
    wd.drop_holds_if_metric_moved(GEOM.cell_w, GEOM.cell_h);

    let art = aterm_render::fx::bitmap(
        aterm_core::render::FxTexels::Rgba8Straight,
        1,
        1,
        vec![255u8; 4],
    )
    .expect("a 1x1 art");
    wd.pet_last_tile = Some((
        Arc::clone(&art),
        crate::pet_glyphs_gen::PetGlyphId::PetLoaf,
        0,
        0,
        0,
    ));
    wd.pet_depart_tiles[0] = Some((1, Arc::clone(&art), 0));
    wd.dog_last_tile = Some((
        art,
        0,
        crate::dog_cameo::DogLook {
            breed: crate::dog_glyphs_gen::DogGlyphId::D1Beagle,
            coat: 0,
        },
    ));

    wd.drop_holds_if_metric_moved(GEOM.cell_w, GEOM.cell_h);
    assert!(
        wd.pet_last_tile.is_some() && wd.pet_depart_tiles[0].is_some(),
        "an unchanged metric keeps the holds"
    );

    wd.drop_holds_if_metric_moved(GEOM.cell_w * 2, GEOM.cell_h * 2);
    assert!(wd.pet_last_tile.is_none(), "the pet body's hold survived");
    assert!(
        wd.pet_depart_tiles.iter().all(Option::is_none),
        "a departing ghost's hold survived"
    );
    assert!(wd.dog_last_tile.is_none(), "the dog's hold survived");
}

#[test]
fn a_pet_on_row_zero_emits_no_sprite_above_the_grid() {
    let mut wd = WordDecorations::default();
    let mut free: Vec<FreeSprite> = Vec::new();
    let pet = sleeper_on_the_top_row();
    let grid_w = i32::from(GEOM.cols) * i32::from(GEOM.cell_w);
    let grid_h = i32::from(GEOM.rows) * i32::from(GEOM.cell_h);
    let mut seen = 0usize;
    for _ in 0..12 {
        wd.begin_host_frame();
        free.clear();
        wd.pet_cursor(
            PetCursorFrame {
                geom: GEOM,
                colors: CatColorKey::default(),
                coat: 3,
                iris: 1,
                pet,
                page: None,
            },
            &mut free,
        );
        seen = seen.max(free.len());
        for s in &free {
            assert!(
                s.y >= 0,
                "a pet sprite resolved ABOVE the grid at y={} (h={}): after the \
                 tab-strip splice that is the strip, not padding",
                s.y,
                s.h
            );
            assert!(
                s.y + i32::from(s.h) <= grid_h,
                "a pet sprite resolved BELOW the grid: y={} h={} grid_h={grid_h}",
                s.y,
                s.h
            );
            assert!(
                s.x >= 0 && s.x + i32::from(s.w) <= grid_w,
                "…or off the sides"
            );
        }
    }
    assert!(
        seen >= 2,
        "fixture: the body AND the mote must both have been emitted (saw {seen} \
         sprites at most) or this test proves nothing"
    );
}

#[test]
fn a_pet_in_open_grid_is_untouched_and_still_matches_body_px() {
    let mut wd = WordDecorations::default();
    let mut free: Vec<FreeSprite> = Vec::new();
    let pet = crate::kitty_pet::PetFrame {
        row: 9.0,
        motes: [None; crate::kitty_pet::PET_MOTES_MAX],
        ..sleeper_on_the_top_row()
    };
    let body = pet
        .body_px(GEOM.cell_w, GEOM.cell_h, GEOM.cols, GEOM.rows)
        .expect("a visible pet has a body");
    assert_eq!(
        body.2,
        ((pet.row + 1.0) * f32::from(GEOM.cell_h)).round() as i32
            - (crate::kitty_pet::ART_ROWS * f32::from(GEOM.cell_h)).round() as i32,
        "an interior body keeps its exact unclamped top"
    );
    let mut drawn = None;
    for _ in 0..12 {
        wd.begin_host_frame();
        free.clear();
        wd.pet_cursor(
            PetCursorFrame {
                geom: GEOM,
                colors: CatColorKey::default(),
                coat: 3,
                iris: 1,
                pet,
                page: None,
            },
            &mut free,
        );
        if let Some(s) = free.first() {
            drawn = Some((s.x, s.y, s.w, s.h));
            break;
        }
    }
    let (x, y, w, h) = drawn.expect("the body tile lands within the retry window");
    assert_eq!(
        (x, y, i32::from(w), i32::from(h)),
        (body.0, body.2, body.1 - body.0, body.3 - body.2),
        "the emitted sprite must still BE `body_px`"
    );
}

/// A pane at the window's top leaves its top edge open (a sprite may overhang the pad, and
/// after the tab-strip splice that overhang is the strip), and a pane below another clips
/// hard at its divider: with the caret on either of the first two rows, both cursor
/// companions must land whole inside either pane.
#[test]
fn both_companions_on_the_top_rows_land_whole_in_a_top_pane_and_a_lower_pane() {
    use crate::kitty_cursor::CatPose;
    use crate::pane::{PanePlace, translate_free_into_pane};

    let ch = i32::from(GEOM.cell_h);
    let pane = |row_off: u16| PanePlace {
        row_off,
        col_off: 0,
        rows: GEOM.rows,
        cols: GEOM.cols,
        win_rows: GEOM.rows + 16,
        win_cols: GEOM.cols,
        cell_w: u32::from(GEOM.cell_w),
        cell_h: u32::from(GEOM.cell_h),
    };
    for row in [0u16, 1] {
        let mut cases: Vec<(String, Vec<FreeSprite>)> = Vec::new();
        let pet = crate::kitty_pet::PetFrame {
            row: f32::from(row),
            ..sleeper_on_the_top_row()
        };
        let mut wd = WordDecorations::default();
        let mut free = Vec::new();
        for _ in 0..12 {
            wd.begin_host_frame();
            free.clear();
            wd.pet_cursor(
                PetCursorFrame {
                    geom: GEOM,
                    colors: CatColorKey::default(),
                    coat: 3,
                    iris: 1,
                    pet,
                    page: None,
                },
                &mut free,
            );
            if free.len() >= 2 {
                break;
            }
        }
        assert!(
            free.len() >= 2,
            "fixture: the pet's body and mote on row {row}"
        );
        cases.push((format!("pet on row {row}"), free));
        let lifts: &[f32] = if row == 0 { &[0.0] } else { &[0.0, 1.0] };
        for &lift in lifts {
            let mut wd = WordDecorations::default();
            let mut free = Vec::new();
            wd.kitty_cursor(
                KittyCursorFrame {
                    geom: GEOM,
                    cursor: (row, 30),
                    look: KittyLook::default(),
                    colors: CatColorKey::default(),
                    bob: 0.0,
                    lift,
                    alpha: 255,
                    pose: CatPose::STILL,
                    sing: 0.0,
                    notes: [None; crate::kitty_sing::MAX_NOTES],
                },
                &mut free,
            )
            .expect("the flying head emitted");
            cases.push((format!("flying head on row {row} lift {lift}"), free));
        }
        for (label, free) in cases {
            for row_off in [0u16, 16] {
                let mut placed = free.clone();
                translate_free_into_pane(&mut placed, pane(row_off));
                assert_eq!(
                    placed.len(),
                    free.len(),
                    "{label}, pane at row {row_off}: a sprite was cut away whole"
                );
                let top = i32::from(row_off) * ch;
                for s in &placed {
                    assert!(
                        s.y >= top,
                        "{label}, pane at row {row_off}: a sprite starts {} px above the \
                         pane's grid, in the {}",
                        top - s.y,
                        if row_off == 0 {
                            "window pad or the tab strip"
                        } else {
                            "pane above"
                        }
                    );
                    assert_eq!(
                        s.visible(),
                        PxRect::at(s.x, s.y, u32::from(s.w), u32::from(s.h)),
                        "{label}, pane at row {row_off}: the pane's edge cut the sprite"
                    );
                }
            }
        }
    }
}
