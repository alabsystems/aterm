// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    KNOCK_DILATE_OFFSETS, KNOCK_INK_LEVEL, KNOCK_REACH, blit_mono_with, knock_cover, knock_ring,
    knock_stamp,
};

/// THE RING CUTS AS ITS STAMPS DO. One blit of a glyph's ring leaves the mask as the 21
/// stamps of its coverage ([`knock_stamp`]) one after another leave it, to within the
/// rounding of the stamps' own steps, on a bare mask and on one another glyph's ring
/// already cut into.
#[test]
fn one_ring_blit_cuts_as_the_stamps_do() {
    let (w, h) = (7usize, 9usize);
    let glyph: Vec<u8> = (0..w * h)
        .map(|i| match (i % w, i / w) {
            (3, _) | (_, 4) => 255,
            (2, _) => 96,
            (4, _) => 180,
            (x, y) if (x + y) % 5 == 0 => 30,
            _ => 0,
        })
        .collect();
    let reach = KNOCK_REACH as usize;
    let (mw, mh) = (w + 2 * reach + 4, h + 2 * reach + 4);
    let everywhere = ((0, mh as i32), (0, mw as i32));
    for under in [0u32, 0x0040_4040] {
        let mut stamped = vec![under; mw * mh];
        for &(dx, dy) in &KNOCK_DILATE_OFFSETS {
            let (x, y) = (2 + reach as i32 + dx, 2 + reach as i32 + dy);
            let stamp: Vec<u8> = glyph.iter().map(|&c| knock_stamp(c, dx, dy)).collect();
            blit_mono_with(
                &mut stamped,
                mw,
                x,
                y,
                w,
                h,
                &stamp,
                0x00FF_FFFF,
                255,
                1,
                1,
                everywhere.0,
                everywhere.1,
                knock_cover,
            );
        }
        let ring = knock_ring(w, h, &glyph);
        let mut rung = vec![under; mw * mh];
        blit_mono_with(
            &mut rung,
            mw,
            2,
            2,
            w + 2 * reach,
            h + 2 * reach,
            &ring,
            0x00FF_FFFF,
            255,
            1,
            1,
            everywhere.0,
            everywhere.1,
            knock_cover,
        );
        let worst = stamped
            .iter()
            .zip(&rung)
            .map(|(a, b)| ((a & 0xff) as i32 - (b & 0xff) as i32).abs())
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 2,
            "under {under:#x}: the ring is {worst} levels off its stamps"
        );
        // Negative control: the bare coverage, undilated, misses the ring's rim.
        let mut bare = vec![under; mw * mh];
        blit_mono_with(
            &mut bare,
            mw,
            2 + reach as i32,
            2 + reach as i32,
            w,
            h,
            &glyph,
            0x00FF_FFFF,
            255,
            1,
            1,
            everywhere.0,
            everywhere.1,
            knock_cover,
        );
        assert!(
            stamped
                .iter()
                .zip(&bare)
                .any(|(a, b)| ((a & 0xff) as i32 - (b & 0xff) as i32).abs() > 2),
            "control: the stamps reach past the bare coverage"
        );
    }
}

/// THE MASK HOLDS THE SHARE CUT. Under a glyph, each pixel of the knock-out's mask holds
/// the share its 21 stamps ([`knock_stamp`]) cut, `1 - Π(1 - coverage)`, as a linear byte: the cut reads it
/// as an alpha factor ([`crate::fx_raster::cut_sprite_art`]), so a body cut over the page
/// is the old page stamp up to rounding. Mixed with [`super::blend`] toward white, the
/// mask held that share sRGB-encoded and a letter's soft rim cut the body by
/// `srgb(share)`: a tenth's share cut a third of it. Both paths: the ring blit and a
/// doubled line's stamps.
#[test]
fn the_mask_holds_the_share_cut() {
    let mut rend =
        super::Renderer::from_system(18.0, super::Theme::default()).expect(super::NO_FONT);
    let key = rend.glyph_key('o');
    let baseline = rend.baseline;
    let reach = KNOCK_REACH;
    for xs in [1usize, 2] {
        let super::GlyphImage::Mono {
            width: w,
            height: h,
            xmin,
            ymin,
            ref bytes,
            ..
        } = rend.cached_glyph(key).img
        else {
            panic!("fixture: 'o' rasterizes as a mono coverage");
        };
        let bytes = bytes.clone();
        let scale = super::Scale {
            xs,
            ys: xs,
            ..super::Scale::NORMAL
        };
        let (s, gx0, gy0) = (
            xs as i32,
            xmin * xs as i32,
            xs as i32 * (baseline - h as i32 - ymin),
        );
        let area = [
            gx0 - reach - 1,
            gy0 - reach - 1,
            gx0 + w as i32 * s + reach + 1,
            gy0 + h as i32 * s + reach + 1,
        ];
        let stride = (area[2] - area[0]) as usize;
        let mut mask = vec![0u32; stride * (area[3] - area[1]) as usize];
        rend.blit_glyph_ring(&mut mask, area, 0, 0, key, scale);
        let cov = |x: i32, y: i32| {
            let (i, j) = ((x - gx0).div_euclid(s), (y - gy0).div_euclid(s));
            if (0..w as i32).contains(&i) && (0..h as i32).contains(&j) {
                f64::from(bytes[j as usize * w + i as usize]) / 255.0
            } else {
                0.0
            }
        };
        let (mut worst, mut soft) = ((0.0f64, 0u32, 0.0f64), 0usize);
        for y in area[1]..area[3] {
            for x in area[0]..area[2] {
                let left: f64 = KNOCK_DILATE_OFFSETS
                    .iter()
                    .map(|&(dx, dy)| {
                        let stamped = (255.0 * cov(x - dx, y - dy)).round() as u8;
                        1.0 - f64::from(knock_stamp(stamped, dx, dy)) / 255.0
                    })
                    .product();
                let share = 255.0 * (1.0 - left);
                let m = mask[(y - area[1]) as usize * stride + (x - area[0]) as usize] & 0xff;
                soft += usize::from(share > 8.0 && share < 128.0);
                if (f64::from(m) - share).abs() > worst.0 {
                    worst = ((f64::from(m) - share).abs(), m, share);
                }
            }
        }
        assert!(soft > 0, "fixture: the ring at scale {xs} has soft pixels");
        assert!(
            worst.0 <= 2.5,
            "scale {xs}: a mask pixel holds {} where its stamps cut {:.1} of 255",
            worst.1,
            worst.2
        );
    }
}

/// BESIDE A PIXEL THAT READS AS INK THE CUT IS WHOLE. A glyph pixel of [`KNOCK_INK_LEVEL`]
/// or more cuts every pixel beside it whole, so the ground a letter's faintest ink is read
/// against holds none of the coat; two pixels away it cuts its own share. Negative
/// control: a pixel a level under it cuts its own share beside it too, and leaves coat
/// there. With no level (macOS, Linux) every stamp is the coverage itself, so the ring is
/// its coverage's shares alone.
#[test]
fn beside_a_pixel_that_reads_as_ink_the_cut_is_whole() {
    let Some(level) = KNOCK_INK_LEVEL else {
        for cov in [0u8, 1, 112, 143, 254, 255] {
            for &(dx, dy) in &KNOCK_DILATE_OFFSETS {
                assert_eq!(knock_stamp(cov, dx, dy), cov, "{cov} at ({dx}, {dy})");
            }
        }
        assert!(super::knock_near_stamp(&[0, 112, 143, 254, 255]).is_none());
        return;
    };
    let reach = KNOCK_REACH as usize;
    let side = 1 + 2 * reach;
    let at = |ring: &[u8], dx: i32, dy: i32| {
        ring[(reach as i32 + dy) as usize * side + (reach as i32 + dx) as usize]
    };
    let ink = knock_ring(1, 1, &[level]);
    for dy in -1..=1 {
        for dx in -1..=1 {
            assert_eq!(at(&ink, dx, dy), 255, "({dx}, {dy}) of an ink pixel");
        }
    }
    assert_eq!(at(&ink, 2, 0), level, "two pixels away: its share");
    assert_eq!(at(&ink, 2, 2), 0, "past the disc");
    let under = level - 1;
    let soft = knock_ring(1, 1, &[under]);
    for (dx, dy) in [(0, 0), (1, 0), (-1, 1), (2, 0), (0, -2)] {
        assert_eq!(at(&soft, dx, dy), under, "control at ({dx}, {dy})");
    }
    // A whole pixel cuts whole and a bare one not at all, whatever the level.
    for dx in -1..=1 {
        assert_eq!(knock_stamp(255, dx, 0), 255);
        assert_eq!(knock_stamp(0, dx, 0), 0);
        assert_eq!(knock_stamp(under, dx, 0), under);
    }
    assert_eq!(knock_stamp(level, 2, 0), level);
    assert_eq!(knock_stamp(level, 0, -2), level);
    assert_eq!(
        super::knock_near_stamp(&[0, under, level, 255]),
        Some(vec![0, under, 255, 255])
    );
    assert_eq!(super::knock_near_stamp(&[0, under, 255]), None);
}
