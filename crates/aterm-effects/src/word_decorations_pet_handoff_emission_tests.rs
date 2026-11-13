// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn pet_frame() -> crate::kitty_pet::PetFrame {
    crate::kitty_pet::PetFrame {
        alpha: 255,
        lane_alpha: 255,
        action: crate::kitty_pet::PetAction::Sit,
        pose: crate::pet_glyphs_gen::PetGlyphId::PetSit,
        col: 10.0,
        row: 5.0,
        lift: 0.0,
        facing_left: false,
        scale_x: 1.0,
        scale_y: 1.0,
        stretch_from: crate::kitty_pet::StretchFrom::Centre,
        purr: 0.0,
        contented: false,
        under_ink: false,
        motes: [None; crate::kitty_pet::PET_MOTES_MAX],
        departures: [None; crate::kitty_pet::PET_DEPARTURES_MAX],
    }
}

#[test]
fn a_look_flip_refuses_the_old_looks_held_tile_under_bake_pressure() {
    let mut wd = WordDecorations::default();
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let colors = CatColorKey::default();
    let mut free = Vec::new();
    let dress = |wd: &mut WordDecorations, coat: u8, iris: u8, free: &mut Vec<_>| {
        free.clear();
        wd.pet_cursor(
            PetCursorFrame {
                geom,
                colors,
                coat,
                iris,
                pet: pet_frame(),
                page: None,
            },
            free,
        );
    };
    let mut landed = false;
    for _ in 0..8 {
        wd.cat_baker_ready = false;
        dress(&mut wd, 3, 1, &mut free);
        if !free.is_empty() {
            landed = true;
            break;
        }
    }
    assert!(landed, "look A's tile never landed");
    assert!(
        wd.pet_last_tile
            .as_ref()
            .is_some_and(|(.., c, i)| (*c, *i) == (3, 1)),
        "the held tile carries look A"
    );
    wd.cat_baker.begin_frame(geom.cell_w, geom.cell_h);
    wd.animal_baker.begin_frame(geom.cell_w, geom.cell_h);
    wd.cat_baker_ready = true;
    let scratch = vec![0u8; 4 * 4 * 4];
    assert!(
        wd.cat_baker
            .host_tile(0xDEAD_0001, 4, 4, &scratch)
            .is_some()
    );
    assert!(
        wd.cat_baker
            .host_tile(0xDEAD_0002, 4, 4, &scratch)
            .is_some()
    );
    dress(&mut wd, 9, 4, &mut free);
    assert!(
        free.is_empty(),
        "the old coat's held tile must not dress the new look"
    );
    let mut landed = false;
    for _ in 0..8 {
        wd.cat_baker_ready = false;
        dress(&mut wd, 9, 4, &mut free);
        if !free.is_empty() {
            landed = true;
            break;
        }
    }
    assert!(landed, "look B's tile never landed");
    assert!(
        wd.pet_last_tile
            .as_ref()
            .is_some_and(|(.., c, i)| (*c, *i) == (9, 4)),
        "the held tile now carries look B"
    );
}

#[test]
fn a_held_pet_tile_draws_its_own_texels_whatever_the_cache_evicts() {
    let mut wd = WordDecorations::default();
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let colors = CatColorKey::default();
    let mut free = Vec::new();
    let mut loaf_frame = pet_frame();
    loaf_frame.pose = crate::pet_glyphs_gen::PetGlyphId::PetLoaf;
    wd.begin_host_frame();
    wd.pet_cursor(
        PetCursorFrame {
            geom,
            colors,
            coat: 3,
            iris: 1,
            pet: loaf_frame,
            page: None,
        },
        &mut free,
    );
    let (loaf, _, held_host, ..) = wd.pet_last_tile.clone().expect("PetLoaf landed");
    let loaf_bytes = loaf.data().to_vec();
    let pet = |wd: &mut WordDecorations, frame, free: &mut Vec<FreeSprite>| {
        free.clear();
        wd.pet_cursor(
            PetCursorFrame {
                geom,
                colors,
                coat: 3,
                iris: 1,
                pet: frame,
                page: None,
            },
            free,
        )
    };
    // Spend every frame's bake budget on other host art: the sit pose never
    // bakes, so the body falls back to the held loaf each frame, and the
    // cache evicts the loaf's own entry under the pressure.
    let pressure = |wd: &mut WordDecorations, salt: u64| {
        wd.begin_host_frame();
        wd.begin_baker_frame(geom, true);
        let scratch = vec![255u8; 4 * 4 * 4];
        for sub in 0..2u64 {
            assert!(wd.cat_baker.host_tile(salt | sub, 4, 4, &scratch).is_some());
        }
    };
    let mut sit_frame = pet_frame();
    sit_frame.pose = crate::pet_glyphs_gen::PetGlyphId::PetSit;
    for step in 0..48u64 {
        pressure(&mut wd, 0xCAFE_0000 | (step << 8));
        pet(&mut wd, sit_frame, &mut free);
        assert_eq!(free.len(), 1, "held pet tile draws on step {step}");
        assert!(
            Arc::ptr_eq(&free[0].art, &loaf),
            "step {step}: the fallback draws the held art itself"
        );
    }
    assert!(
        wd.cat_baker.host_peek(held_host).is_none(),
        "fixture: the cache evicted the held identity"
    );
    assert_eq!(
        loaf.data(),
        loaf_bytes.as_slice(),
        "an evicted art is never rewritten: no other art can alias it"
    );

    // A word cat baked where the loaf lived changes nothing the body draws.
    wd.cat_baker.clear();
    wd.begin_host_frame();
    wd.begin_baker_frame(geom, true);
    let cat_tile = wd
        .cat_baker
        .get_v4(&BakeKeyV4 {
            variant: cat_variant_v4(0),
            accessory: None,
            coat: 0,
            iris: 0,
            colors,
            w: 24,
            h: 24,
            eyes: EyesFrame::Open,
        })
        .expect("word cat baked");
    assert!(
        wd.cat_baker
            .host_tile(0xDEAD_BEEF, 4, 4, &[255; 64])
            .is_some()
    );
    pet(&mut wd, sit_frame, &mut free);
    assert_eq!(free.len(), 1);
    assert_ne!(
        free[0].art.id(),
        cat_tile.art.id(),
        "never the word cat's texels"
    );
    assert!(Arc::ptr_eq(&free[0].art, &loaf), "still the held loaf");

    // Negative control: a look change refuses the old look's hold.
    wd.begin_host_frame();
    wd.begin_baker_frame(geom, true);
    assert!(
        wd.cat_baker
            .host_tile(0xF00D_0001, 4, 4, &[255; 64])
            .is_some()
    );
    assert!(
        wd.cat_baker
            .host_tile(0xF00D_0002, 4, 4, &[255; 64])
            .is_some()
    );
    free.clear();
    wd.pet_cursor(
        PetCursorFrame {
            geom,
            colors,
            coat: 9,
            iris: 4,
            pet: sit_frame,
            page: None,
        },
        &mut free,
    );
    assert!(free.is_empty(), "another look never wears the held art");
}

#[test]
fn cursor_cat_eye_transition_under_bake_pressure_reuses_any_resident_eye_frame() {
    let mut wd = WordDecorations::default();
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let look = KittyLook::default();
    let colors = CatColorKey::default();
    let mut free = Vec::new();
    let emit = |wd: &mut WordDecorations, eyes: EyesFrame, free: &mut Vec<_>| {
        free.clear();
        let mut pose = crate::kitty_cursor::CatPose::STILL;
        pose.eyes = eyes;
        wd.kitty_cursor(
            KittyCursorFrame {
                geom,
                cursor: (5, 20),
                look,
                colors,
                bob: 0.0,
                lift: 0.0,
                alpha: 255,
                pose,
                sing: 0.0,
                notes: [None; crate::kitty_sing::MAX_NOTES],
            },
            free,
        )
    };

    wd.begin_host_frame();
    assert!(emit(&mut wd, EyesFrame::Happy, &mut free).is_some());
    assert_eq!(free.len(), 1);
    let happy = free[0].art.id();

    let scratch = vec![255u8; 4 * 4 * 4];
    for (step, target_eyes) in [(0u64, EyesFrame::Blink), (1, EyesFrame::Open)] {
        wd.begin_host_frame();
        wd.begin_baker_frame(geom, true);
        for sub in 0..2u64 {
            assert!(
                wd.cat_baker
                    .host_tile(0xBEEF_0000 | (step * 2 + sub), 4, 4, &scratch)
                    .is_some()
            );
        }
        assert!(
            emit(&mut wd, target_eyes, &mut free).is_some(),
            "{target_eyes:?} must fall back to resident Happy rather than flicker out"
        );
        assert_eq!(free.len(), 1);
        assert_eq!(free[0].art.id(), happy, "the resident Happy art itself");
    }
}

#[test]
fn note_bake_budget_pet_own_glass_fp_and_dog_first_animal_baker_stay_tight() {
    let mut wd = WordDecorations::default();
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let look = KittyLook::default();
    let colors = CatColorKey::default();
    let mut free = Vec::new();

    wd.begin_host_frame();
    assert!(
        wd.kitty_cursor(
            KittyCursorFrame {
                geom,
                cursor: (5, 20),
                look,
                colors,
                bob: 0.0,
                lift: 0.0,
                alpha: 255,
                pose: crate::kitty_cursor::CatPose::STILL,
                sing: 0.0,
                notes: [None; crate::kitty_sing::MAX_NOTES],
            },
            &mut free,
        )
        .is_some()
    );
    let ver_before = wd.cat_baker.version();
    let mut notes = [None; crate::kitty_sing::MAX_NOTES];
    notes[0] = Some(crate::kitty_sing::NoteSprite {
        kind: crate::kitty_sing::NoteKind::Eighth,
        dx: 0.5,
        dy: -0.5,
        alpha: 0,
        tint: 0x00FF_AA44,
    });
    free.clear();
    wd.begin_host_frame();
    assert!(
        wd.kitty_cursor(
            KittyCursorFrame {
                geom,
                cursor: (5, 20),
                look,
                colors,
                bob: 0.0,
                lift: 0.0,
                alpha: 255,
                pose: crate::kitty_cursor::CatPose::STILL,
                sing: 1.0,
                notes,
            },
            &mut free,
        )
        .is_some()
    );
    assert_eq!(free.len(), 1, "invisible note is not emitted");
    assert_eq!(
        wd.cat_baker.version(),
        ver_before,
        "an alpha-0 note must not spend bake budget or bump cat_baker.version()"
    );
    assert!(
        wd.note_cache[0].is_none(),
        "an alpha-0 note must not rasterize into note_cache"
    );

    let mut slot = None;
    let scratch = vec![255u8; 4 * 4 * 4];
    wd.begin_host_frame();
    wd.begin_baker_frame(geom, true);
    assert!(wd.cat_baker.host_tile(0xAA01, 4, 4, &scratch).is_some());
    assert!(wd.cat_baker.host_tile(0xAA02, 4, 4, &scratch).is_some());
    let mut painted = false;
    assert!(
        NotePaintCache::tile(&mut slot, &mut wd.cat_baker, 0xAA03, (8, 8), |_| {
            painted = true;
        })
        .is_none()
    );
    assert!(
        !painted && slot.is_none(),
        "NotePaintCache::tile must not run paint() when cat_baker budget is spent"
    );
    wd.begin_host_frame();
    wd.begin_baker_frame(geom, true);
    assert!(
        NotePaintCache::tile(&mut slot, &mut wd.cat_baker, 0xAA03, (8, 8), |tile| {
            tile.disc(4.0, 4.0, 2.0, (1.0, 1.0, 1.0), 1.0);
        })
        .is_some()
    );
    let ptr_8 = slot
        .as_ref()
        .expect("cached 8x8 tile")
        .tile
        .pixels()
        .as_ptr();
    assert!(
        NotePaintCache::tile(&mut slot, &mut wd.cat_baker, 0xAA04, (6, 6), |tile| {
            tile.disc(3.0, 3.0, 1.5, (1.0, 1.0, 1.0), 1.0);
        })
        .is_some()
    );
    assert_eq!(
        slot.as_ref()
            .expect("cached 6x6 tile")
            .tile
            .pixels()
            .as_ptr(),
        ptr_8,
        "NotePaintCache::tile must reuse its Tile buffer when a new host fits existing capacity"
    );

    for (cols, rows, scale) in [(1, geom.rows, 1.0), (geom.cols, 1, 3.0)] {
        let mut wd_mote = WordDecorations::default();
        let mut pet = pet_frame();
        let mut frame = PetCursorFrame {
            geom,
            colors,
            coat: 3,
            iris: 1,
            pet,
            page: None,
        };
        wd_mote.begin_host_frame();
        free.clear();
        assert!(wd_mote.pet_cursor(frame, &mut free).is_some());
        assert_eq!(free.len(), 1, "fixture: the body tile is warm");
        let mote = crate::kitty_pet::PetMoteSprite {
            kind: crate::kitty_pet::PetMoteKind::Heart,
            col: 10.0,
            row: 5.0,
            scale,
            rot: 0.4,
            alpha: 255,
            rgb: None,
        };
        let side = crate::kitty_pet::mote_tile_side(geom.cell_h);
        let host = WordDecorations::pet_mote_host_id(
            mote.kind,
            WordDecorations::pet_mote_rot_bucket(mote.rot),
            side,
        );
        pet.motes[0] = Some(mote);
        frame.pet = pet;
        frame.geom.cols = cols;
        frame.geom.rows = rows;
        assert!(
            crate::kitty_pet::mote_box_px(
                (mote.col, mote.row),
                mote.scale,
                (geom.cell_w, geom.cell_h),
                (cols, rows),
            )
            .is_none(),
            "fixture: the mote cannot fit this grid"
        );
        let version = wd_mote.cat_baker.version();
        wd_mote.begin_host_frame();
        free.clear();
        assert!(wd_mote.pet_cursor(frame, &mut free).is_some());
        assert_eq!(wd_mote.cat_baker.version(), version);
        assert!(wd_mote.cat_baker.host_peek(host).is_none());

        frame.geom = geom;
        wd_mote.begin_host_frame();
        free.clear();
        assert!(wd_mote.pet_cursor(frame, &mut free).is_some());
        assert!(wd_mote.cat_baker.version() > version);
        assert!(wd_mote.cat_baker.host_peek(host).is_some());
        assert_eq!(free.len(), 2, "the body and the fitting mote both draw");
    }

    let mut pet = pet_frame();
    let mut dep = crate::kitty_pet::PetDepartureSprite {
        id: 7,
        pose: crate::pet_glyphs_gen::PetGlyphId::PetLoaf,
        coat: 3,
        iris: 1,
        colors: Some(colors),
        col: 10.0,
        row: 5.0,
        lift: 0.0,
        stretch_from: crate::kitty_pet::StretchFrom::Centre,
        facing_left: false,
        scale_x: 1.0,
        scale_y: 1.0,
        alpha: 200,
        under_ink: true,
        own_glass: true,
        glass: 0,
    };
    pet.departures[0] = Some(dep);
    wd.begin_host_frame();
    free.clear();
    assert!(
        wd.pet_cursor(
            PetCursorFrame {
                geom,
                colors,
                coat: 3,
                iris: 1,
                pet,
                page: None,
            },
            &mut free,
        )
        .is_some()
    );
    let mut glass_free = Vec::new();
    let fp_a = wd
        .pet_own_glass(
            PetCursorFrame {
                geom,
                colors,
                coat: 3,
                iris: 1,
                pet,
                page: None,
            },
            geom,
            (0, 0),
            0,
            &mut glass_free,
        )
        .expect("own_glass departure drew");
    dep.colors = Some(CatColorKey {
        accent: colors.accent ^ 1,
        ..colors
    });
    pet.departures[0] = Some(dep);
    wd.begin_host_frame();
    free.clear();
    let _ = wd.pet_cursor(
        PetCursorFrame {
            geom,
            colors,
            coat: 3,
            iris: 1,
            pet,
            page: None,
        },
        &mut free,
    );
    glass_free.clear();
    let fp_b = wd
        .pet_own_glass(
            PetCursorFrame {
                geom,
                colors,
                coat: 3,
                iris: 1,
                pet,
                page: None,
            },
            geom,
            (0, 0),
            0,
            &mut glass_free,
        )
        .expect("own_glass departure drew with updated palette");
    assert_ne!(
        fp_a, fp_b,
        "pet_own_glass fingerprint must distinguish departure palettes and baker versions"
    );

    let mut wd_dog_first = WordDecorations::default();
    wd_dog_first.begin_host_frame();
    free.clear();
    let _ = wd_dog_first.dog_cameo(
        DogCameoFrame {
            geom,
            cursor: (5, 40),
            look: crate::dog_cameo::DogLook {
                breed: crate::dog_glyphs_gen::DOG_HEADS[0],
                coat: 9,
            },
            colors,
            bob: 0.0,
            alpha: 255,
            now: Instant::now(),
        },
        &mut free,
        |_, _| Some(false),
        &[],
        (0, 0),
    );
    assert!(
        wd_dog_first
            .animal_baker
            .tile(&crate::animal_baker::AnimalBakeKey {
                species: crate::animal_glyphs_gen::AnimalGlyphId::Cow,
                colors,
                w: 24,
                h: 24,
            })
            .is_some(),
        "animal_baker must have its per-frame bake budget even when dog_cameo ran first"
    );

    let mut wd_dep_only = WordDecorations::default();
    wd_dep_only.begin_host_frame();
    free.clear();
    let mut pet_dep_only = pet;
    pet_dep_only.alpha = 0;
    pet_dep_only.lane_alpha = 255;
    pet_dep_only.departures[0] = Some(crate::kitty_pet::PetDepartureSprite {
        id: 1,
        own_glass: false,
        pose: crate::pet_glyphs_gen::PetGlyphId::PetRun0,
        coat: 1,
        iris: 0,
        colors: Some(colors),
        col: 10.0,
        row: 5.0,
        alpha: 220,
        ..dep
    });
    pet_dep_only.departures[1] = Some(crate::kitty_pet::PetDepartureSprite {
        id: 2,
        own_glass: false,
        pose: crate::pet_glyphs_gen::PetGlyphId::PetRun1,
        coat: 4,
        iris: 2,
        colors: Some(colors),
        col: 14.0,
        row: 5.0,
        facing_left: true,
        alpha: 200,
        ..dep
    });
    let _ = wd_dep_only.pet_cursor(
        PetCursorFrame {
            geom,
            colors,
            coat: 3,
            iris: 1,
            pet: pet_dep_only,
            page: None,
        },
        &mut free,
    );
    assert_eq!(
        free.len(),
        2,
        "an alpha == 0 body must not steal a bake from two cold departing ghosts"
    );
    assert!(
        wd_dep_only.pet_last_tile.is_none(),
        "an alpha == 0 body must not populate pet_last_tile"
    );

    let mut wd_narrow = WordDecorations::default();
    wd_narrow.begin_host_frame();
    free.clear();
    assert!(
        wd_narrow
            .dog_cameo(
                DogCameoFrame {
                    geom: EffectGeom { cols: 1, ..geom },
                    cursor: (0, 0),
                    look: crate::dog_cameo::DogLook {
                        breed: crate::dog_glyphs_gen::DOG_HEADS[0],
                        coat: 9,
                    },
                    colors,
                    bob: 0.0,
                    alpha: 255,
                    now: Instant::now(),
                },
                &mut free,
                |_, _| Some(false),
                &[],
                (0, 0),
            )
            .is_none()
    );
    assert_eq!(wd_narrow.dog_baker.version(), 0);
    assert_eq!(wd_narrow.cat_baker.version(), 0);
    assert!(
        wd_narrow
            .pet_own_glass(
                PetCursorFrame {
                    geom,
                    colors,
                    coat: 3,
                    iris: 1,
                    pet,
                    page: None,
                },
                EffectGeom { cols: 0, ..geom },
                (0, 0),
                0,
                &mut free,
            )
            .is_none()
    );
    assert_eq!(wd_narrow.pet_baker.version(), 0);
    assert_eq!(wd_narrow.cat_baker.version(), 0);
}
