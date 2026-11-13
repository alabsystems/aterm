// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn spec(px: f32, scale: f32, done: u32, total: u32) -> LockupSpec {
    LockupSpec {
        kitty: true,
        px,
        scale,
        alab: true,
        done,
        total,
        pose: PetGlyphId::PetSit,
    }
}

#[test]
fn lockup_values_round_trip() {
    for value in [
        spec(22.0, 2.0, 11, 16),
        LockupSpec {
            kitty: false,
            alab: false,
            pose: PetGlyphId::PetLoaf,
            ..spec(17.0, 1.0, 0, 0)
        },
    ] {
        assert_eq!(LockupSpec::parse(Some(&value.value())), value);
    }
    // Unreadable numbers fall back; a count past the total is the total.
    let read = LockupSpec::parse(Some("kitty,NaN,0,1,20,16,99"));
    assert_eq!(
        (read.px, read.scale, read.done, read.total),
        (22.0, 2.0, 16, 16)
    );
    assert_eq!(read.pose, POSES[0]);
}

#[test]
fn the_caret_runs_from_the_words_to_the_far_end_as_steps_finish() {
    let width = 852.0;
    let start = spec(22.0, 2.0, 0, 16);
    let half = spec(22.0, 2.0, 8, 16);
    let end = spec(22.0, 2.0, 16, 16);
    let (a, b, c) = (
        start.caret_x(width),
        half.caret_x(width),
        end.caret_x(width),
    );
    assert!(a < b && b < c, "{a} {b} {c}");
    assert!(a >= start.words_end(), "the caret starts past the words");
    // The kitty fits at the far end, whichever pose it is in.
    let advance = start.advance();
    assert!(c + caret_width(advance) + end.kitty_width() <= width + 0.01);
    // Halfway is halfway along the caret's path.
    assert!(((b - a) - (c - b)).abs() < 0.01);
    // Nothing known yet: the start.
    assert_eq!(spec(22.0, 2.0, 0, 0).caret_x(width), a);
}

#[test]
fn the_kitty_keeps_its_face_and_whole_device_pixels_on_every_display() {
    for (px, room) in [(17.0, 40.0), (22.0, 48.0)] {
        for scale in [1.0_f32, 2.0, 3.0] {
            let s = spec(px, scale, 0, 1);
            let device = s.kitty_height() * scale;
            assert!((device - device.round()).abs() < 0.001, "{px} {scale}");
            if scale >= 2.0 {
                assert!(device >= KITTY_MIN_DEVICE_PX - 0.01, "{px} {scale}");
            }
            assert!(
                s.needed_height() <= room,
                "the lockup keeps its height: {px} {scale}"
            );
            assert!(s.least_width() > s.words_end());
        }
    }
}

/// The first stripe's panels `paint_ribbon` draws past the words, left to right.
fn past_the_words(dark: bool, words_end: f32, caret_x: f32) -> Vec<(f32, [u8; 4])> {
    let surface = if dark { [30, 30, 30] } else { [231, 231, 231] };
    let mut prims = Vec::new();
    paint_ribbon(
        &mut prims, surface, dark, 20.0, 10.0, 0.0, words_end, caret_x,
    );
    let top = 20.0 - 5.0;
    let mut past: Vec<(f32, [u8; 4])> = prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Panel { x, y, fill, .. }
                if (*y - top).abs() < 0.01 && *x >= words_end - 0.01 =>
            {
                Some((*x, *fill))
            }
            _ => None,
        })
        .collect();
    past.sort_by(|a, b| a.0.total_cmp(&b.0));
    past
}

/// THE RIBBON IS THE BRAND'S TRAIL, NOT A NYAN BAND (§4.5, D4): past the words it rises from
/// the trail's own strength toward the caret, like a comet's tail, and at the caret it is
/// never half again as strong as the trail; and it draws nothing past the words until the
/// caret stands 4 pt clear of them. NEGATIVE CONTROL: with room, it does draw there.
#[test]
fn the_ribbon_rises_like_a_comet_from_the_trail_and_never_piles_up_at_the_start() {
    for dark in [true, false] {
        let (under, filled) = if dark {
            (UNDER_WORDS_DARK, FILLED_DARK)
        } else {
            (UNDER_WORDS_LIGHT, FILLED_LIGHT)
        };
        assert!(filled > under && filled <= under * 1.5, "dark: {dark}");
        assert!(past_the_words(dark, 200.0, 200.0 + LEAST_PAST - 0.5).is_empty());
        let past = past_the_words(dark, 200.0, 600.0);
        assert!(past.len() > 4, "the ribbon past the words draws");
        let surface = if dark { [30, 30, 30] } else { [231, 231, 231] };
        let palette = aterm_effects::harness_card::CardPalette::Cruise;
        let rgb = palette.art(palette.stripe_t(0), dark);
        let rgb = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8];
        let apart = |fill: [u8; 4]| {
            (0..3)
                .map(|i| (i32::from(fill[i]) - i32::from(surface[i])).abs())
                .sum::<i32>()
        };
        let rises: Vec<i32> = past.iter().map(|(_, fill)| apart(*fill)).collect();
        assert!(
            rises.windows(2).all(|pair| pair[0] <= pair[1]) && rises[0] < rises[rises.len() - 1],
            "dark {dark}: it rises toward the caret: {rises:?}"
        );
        let at = |a: f32| {
            apart(rgba(
                crate::native_appearance::mix_srgb(surface, rgb, a),
                255,
            ))
        };
        let (first, last) = (past[0].1, past[past.len() - 1].1);
        assert!(
            apart(first) <= at(under + 0.02),
            "dark {dark}: it starts at the trail's"
        );
        assert!(
            apart(last) <= at(filled),
            "dark {dark}: and ends at its full strength"
        );
    }
}

/// HIGH CONTRAST AND SERIOUS MODE SHOW ONE PROGRESS (§4.9's rubric): the caret stays after
/// the words, and the capsule after it fills as steps finish, full at done. NEGATIVE CONTROL:
/// with the kitty the caret is what moves.
#[test]
fn the_words_alone_show_one_progress_and_a_full_track_at_done() {
    let width = 852.0;
    let text = |done| LockupSpec {
        kitty: false,
        ..spec(22.0, 2.0, done, 16)
    };
    let carets: Vec<f32> = [0, 8, 16].map(|done| text(done).caret_x(width)).to_vec();
    assert!(
        carets.iter().all(|x| (x - carets[0]).abs() < 0.01),
        "{carets:?}"
    );
    let kitty: Vec<f32> = [0, 16]
        .map(|done| spec(22.0, 2.0, done, 16).caret_x(width))
        .to_vec();
    assert!(
        kitty[1] > kitty[0] + 100.0,
        "with the kitty the caret moves"
    );
    let roles = crate::settings::Roles::from_theme(aterm_render::Theme::default());
    let capsule = |done| {
        let mut prims = Vec::new();
        paint_lockup(
            &mut prims,
            LogicalRect::new(0.0, 0.0, width, 48.0),
            Some(&text(done).value()),
            &roles,
        );
        let capsules: Vec<(f32, f32, f32)> = prims
            .iter()
            .filter_map(|prim| match prim {
                DrawPrim::Capsule { x, w, frac, .. } => Some((*x, *w, *frac)),
                _ => None,
            })
            .collect();
        assert_eq!(capsules.len(), 1, "one progress");
        capsules[0]
    };
    let (start, half, done) = (capsule(0), capsule(8), capsule(16));
    assert_eq!(
        (start.0, start.1),
        (done.0, done.1),
        "the track stands still"
    );
    assert_eq!((start.2, half.2, done.2), (0.0, 0.5, 1.0), "and fills");
    assert!(done.0 + done.1 >= width - 0.01, "to the end of the lockup");
}

#[test]
fn a_mac_outside_alab_says_aterm_alone() {
    let alab = spec(22.0, 2.0, 0, 1);
    let elsewhere = LockupSpec {
        alab: false,
        ..alab
    };
    assert_eq!(alab.words(), "aterm · ALab");
    assert_eq!(elsewhere.words(), "aterm");
    assert!(elsewhere.least_width() < alab.least_width());
}
