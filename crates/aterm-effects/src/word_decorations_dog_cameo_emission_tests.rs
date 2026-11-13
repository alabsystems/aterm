// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn dog_cameo_emits_one_trailing_sprite() {
    let mut wd = WordDecorations::default();
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let look = crate::dog_cameo::DogLook {
        breed: crate::dog_glyphs_gen::DOG_HEADS[0],
        coat: 9,
    };
    let frame = DogCameoFrame {
        geom,
        cursor: (5, 40),
        look,
        colors: CatColorKey::default(),
        bob: 0.0,
        alpha: 255,
        now: Instant::now(),
    };
    let mut free = Vec::new();
    let mut fp = None;
    for frame_i in 0..8 {
        wd.cat_baker_ready = false;
        free.clear();
        fp = wd.dog_cameo(frame, &mut free, |_, _| Some(false), &[], (0, 0));
        if fp.is_some() {
            break;
        }
        assert!(frame_i < 7, "the dog tile never landed in 8 frames");
    }
    assert!(fp.is_some(), "the emission resolves a fingerprint");
    assert_eq!(free.len(), 1, "exactly one dog sprite");
    let s = &free[0];
    assert!(s.alpha == 255);
    let cursor_left = 40 * 10;
    assert!(
        s.x + i32::from(s.w) <= cursor_left,
        "the dog trails BEHIND the cursor cell (x {} w {} vs cursor left {cursor_left})",
        s.x,
        s.w
    );
    assert!(s.w > 0 && s.h > 0);
}

/// THE TYPED DOG COVERS NO LETTER (K10-6). Typed after `lazy dog`, the dog was drawn centred
/// on the caret's row just behind the caret, over the letters just typed (`agai` of `again`),
/// whatever lay there. It now sits where it covers no lettered cell and keeps off the caret's
/// cell: behind on the row when that is blank, else flush over or under the row; a seat stays
/// while it is clear, and the dog glides to the next. With no clear seat it trails under the
/// letters, which are drawn over it.
///
/// NEGATIVE CONTROL (`DOG_SEAT_OFF`, the seat as it was): the sprite over row 5's letters,
/// drawn over text.
#[test]
fn the_typed_dog_covers_no_letter() {
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let look = crate::dog_cameo::DogLook {
        breed: crate::dog_glyphs_gen::DOG_HEADS[0],
        coat: 9,
    };
    let t0 = Instant::now();
    let frame = |cursor: (u16, u16), ms: u64| DogCameoFrame {
        geom,
        cursor,
        look,
        colors: CatColorKey::default(),
        bob: 0.0,
        alpha: 255,
        now: t0 + Duration::from_millis(ms),
    };
    // The lines typed so far: `ink(row)` letters from column 0 on each row.
    let draw = |wd: &mut WordDecorations, f: DogCameoFrame, ink: &dyn Fn(u16) -> u16| {
        let mut free = Vec::new();
        for _ in 0..8 {
            wd.cat_baker_ready = false;
            free.clear();
            if wd
                .dog_cameo(f, &mut free, |r, c| Some(c < ink(r)), &[], (0, 0))
                .is_some()
            {
                return free.pop().expect("one sprite");
            }
        }
        panic!("the dog tile never landed");
    };
    let covers = |s: &FreeSprite, ink: &dyn Fn(u16) -> u16, caret: (u16, u16)| {
        let (r0, r1) = (s.y.max(0) / 20, (s.y + i32::from(s.h) - 1) / 20);
        let (c0, c1) = (s.x.max(0) / 10, (s.x + i32::from(s.w) - 1) / 10);
        (r0..=r1).any(|r| {
            (c0..=c1).any(|c| c < i32::from(ink(r as u16)) || (r as u16, c as u16) == caret)
        })
    };
    // A shell: the line above typed to column 60, the caret's row to the caret, nothing under.
    let shell = |r: u16| match r {
        4 => 60,
        5 => 40,
        _ => 0,
    };
    DOG_SEAT_OFF.with(|c| c.set(true));
    let old = draw(&mut WordDecorations::default(), frame((5, 40), 0), &shell);
    DOG_SEAT_OFF.with(|c| c.set(false));
    assert!(
        covers(&old, &shell, (5, 40)) && old.z == FreeZ::OverText,
        "negative control: the old seat lays the dog over the letters just typed"
    );
    let mut wd = WordDecorations::default();
    let s = draw(&mut wd, frame((5, 40), 0), &shell);
    assert!(
        !covers(&s, &shell, (5, 40)),
        "the dog over the letters at ({}, {}) {}x{}",
        s.x,
        s.y,
        s.w,
        s.h
    );
    assert_eq!(s.z, FreeZ::OverText);
    assert!(
        s.x + i32::from(s.w) <= 400,
        "it still trails behind the caret"
    );
    assert!(s.y >= 6 * 20, "under the caret's row: y {}", s.y);
    // Typed on, the seat under the row is still clear: the dog keeps it.
    let on = |r: u16| match r {
        4 => 60,
        5 => 44,
        _ => 0,
    };
    let s2 = draw(&mut wd, frame((5, 44), 500), &on);
    assert!(!covers(&s2, &on, (5, 44)));
    assert_eq!(s2.y, s.y, "the seat under the row is kept");
    // A row of words appears under the caret's row: the dog glides off them.
    let crowded = |r: u16| match r {
        4 => 0,
        5 => 44,
        6 | 7 => 80,
        _ => 0,
    };
    let mut seen = Vec::new();
    for i in 1..=20 {
        seen.push(draw(&mut wd, frame((5, 44), 500 + 16 * i), &crowded));
    }
    let landed = seen.last().expect("frames");
    assert!(
        !covers(landed, &crowded, (5, 44)),
        "it ends clear of the words"
    );
    assert!(
        landed.y < 5 * 20,
        "over the caret's row now: y {}",
        landed.y
    );
    let worst = seen
        .windows(2)
        .map(|w| (w[1].y - w[0].y).abs())
        .max()
        .unwrap_or(0);
    assert!(
        worst < 2 * 20,
        "it glides, never jumping {worst} px in a frame"
    );
    // Every seat beside the caret lettered: it trails under the letters.
    let full = |_: u16| 80;
    let mut wd = WordDecorations::default();
    let s = draw(&mut wd, frame((5, 40), 0), &full);
    assert_eq!(s.z, FreeZ::UnderText, "the letters are drawn over it");
}

/// THE TYPED DOG KEEPS OFF ITS WORD'S ICON. The dog its own word summoned (`dog`, typed past
/// the summon's keys) took the seat under the caret's row, where the word pass draws that
/// word's icon: two dog faces stacked under `dog` for the keys after it. A seat keeps off the
/// frame's word icons too: the dog slides along the row under it past the icon, towards the
/// caret, and as the caret goes on it trails as before, every frame clear of the icon, the
/// letters and the caret, never moving more than a cell in a frame. A pane's dog is judged
/// against the icons in the window's grid, at the pane's origin.
///
/// NEGATIVE CONTROL (`DOG_ICONS_OFF`, the seat judged by the letters and the caret alone): the
/// dog lies on the icon.
#[test]
fn the_typed_dog_keeps_off_its_words_icon() {
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let look = crate::dog_cameo::DogLook {
        breed: crate::dog_glyphs_gen::DOG_HEADS[0],
        coat: 9,
    };
    let t0 = Instant::now();
    let frame = |caret: u16, ms: u64| DogCameoFrame {
        geom,
        cursor: (5, caret),
        look,
        colors: CatColorKey::default(),
        bob: 0.0,
        alpha: 255,
        now: t0 + Duration::from_millis(ms),
    };
    // `... lazy dog` typed on row 5 to the caret, the line over it lettered to column 60, and
    // the word pass's icon for `dog` (columns 36 to 38) on the row under it.
    let lettered = |caret: u16| move |r: u16, c: u16| Some(r == 4 && c < 60 || r == 5 && c < caret);
    let icon = (360, 390, 120, 142);
    let overlaps = |s: &FreeSprite, (x0, x1, y0, y1): (i32, i32, i32, i32)| {
        s.x < x1 && s.x + i32::from(s.w) > x0 && s.y < y1 && s.y + i32::from(s.h) > y0
    };
    let draw = |wd: &mut WordDecorations, f: DogCameoFrame, origin: (i32, i32)| {
        let shifted = [(
            icon.0 + origin.0,
            icon.1 + origin.0,
            icon.2 + origin.1,
            icon.3 + origin.1,
        )];
        let mut free = Vec::new();
        for _ in 0..8 {
            wd.cat_baker_ready = false;
            free.clear();
            let caret = f.cursor.1;
            if wd
                .dog_cameo(f, &mut free, lettered(caret), &shifted, origin)
                .is_some()
            {
                return free.pop().expect("one sprite");
            }
        }
        panic!("the dog tile never landed");
    };
    DOG_ICONS_OFF.with(|c| c.set(true));
    let old = draw(&mut WordDecorations::default(), frame(40, 0), (0, 0));
    DOG_ICONS_OFF.with(|c| c.set(false));
    assert!(
        overlaps(&old, icon),
        "negative control: the seat under the row lies on the icon: ({}, {}) {}x{}",
        old.x,
        old.y,
        old.w,
        old.h
    );
    for origin in [(0, 0), (130, 60)] {
        let mut wd = WordDecorations::default();
        let mut prev: Option<FreeSprite> = None;
        for i in 0..60u64 {
            let caret = 40 + (i / 8) as u16;
            let s = draw(&mut wd, frame(caret, 16 * i), origin);
            assert!(
                !overlaps(&s, icon),
                "origin {origin:?}, frame {i}: the dog on the icon at ({}, {}) {}x{}",
                s.x,
                s.y,
                s.w,
                s.h
            );
            let letters = (0..24u16).any(|r| {
                (0..80u16).any(|c| {
                    lettered(caret)(r, c) == Some(true)
                        && overlaps(
                            &s,
                            (
                                i32::from(c) * 10,
                                i32::from(c) * 10 + 10,
                                i32::from(r) * 20,
                                i32::from(r) * 20 + 20,
                            ),
                        )
                })
            });
            assert!(!letters, "origin {origin:?}, frame {i}: the dog on letters");
            assert!(
                !overlaps(
                    &s,
                    (i32::from(caret) * 10, i32::from(caret) * 10 + 10, 100, 120)
                ),
                "origin {origin:?}, frame {i}: the dog on the caret"
            );
            if let Some(p) = prev {
                assert!(
                    (s.x - p.x).abs() <= 10 && (s.y - p.y).abs() <= 10,
                    "origin {origin:?}, frame {i}: a jump from ({}, {}) to ({}, {})",
                    p.x,
                    p.y,
                    s.x,
                    s.y
                );
            }
            prev = Some(s);
        }
    }
}

#[test]
fn a_held_dog_tile_is_refused_to_another_breed() {
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 24,
        cols: 80,
    };
    let breeds = crate::dog_glyphs_gen::DOG_HEADS;
    assert!(breeds.len() >= 2, "fixture: the parade needs two breeds");
    let look = |i: usize| crate::dog_cameo::DogLook {
        breed: breeds[i],
        coat: 9,
    };
    let frame = |l: crate::dog_cameo::DogLook, accent: u8| DogCameoFrame {
        geom,
        cursor: (5, 40),
        look: l,
        colors: CatColorKey {
            accent,
            background: 0,
        },
        bob: 0.0,
        alpha: 255,
        now: Instant::now(),
    };
    let mut wd = WordDecorations::default();
    fn spend_budget_then(
        wd: &mut WordDecorations,
        warm: [DogCameoFrame; 2],
        ask: DogCameoFrame,
    ) -> Option<u64> {
        let mut free = Vec::new();
        wd.begin_host_frame();
        for f in warm {
            free.clear();
            wd.dog_cameo(f, &mut free, |_, _| Some(false), &[], (0, 0));
        }
        free.clear();
        wd.dog_cameo(ask, &mut free, |_, _| Some(false), &[], (0, 0))
    }
    let mut free = Vec::new();
    for accent in [0u8, 1] {
        wd.begin_host_frame();
        free.clear();
        wd.dog_cameo(
            frame(look(0), accent),
            &mut free,
            |_, _| Some(false),
            &[],
            (0, 0),
        );
    }
    assert!(
        spend_budget_then(
            &mut wd,
            [frame(look(0), 2), frame(look(0), 3)],
            frame(look(0), 7)
        )
        .is_some(),
        "a starved frame must still re-emit the held tile for its OWN breed \
         — without this the refusal below would prove nothing"
    );
    assert!(
        spend_budget_then(
            &mut wd,
            [frame(look(0), 4), frame(look(0), 5)],
            frame(look(1), 8)
        )
        .is_none(),
        "a held tile from another breed was re-stamped as the new one"
    );
}
