// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    DROP_WASH_ALPHA, OverlayGlow, apply_drop_overlay, apply_drop_overlay_at, apply_host_chrome_at,
    blend_rgb, composite_tray, composite_tray_at, composite_tray_quad_at, drop_border_px,
    tray_quad_below_y,
};

#[test]
fn canonical_host_chrome_order_is_base_then_tray_then_bell_then_overlay() {
    let mut pixels = vec![0x0010_2030; 10 * 10];
    let rgba = [0xFF, 0x00, 0x00, 0xFF];
    let tray = aterm_gpu::TrayQuad {
        rgba: &rgba,
        pw: 1,
        ph: 1,
        dx: 5,
        dy: 5,
        scrim: None,
    };
    let overlay = OverlayGlow {
        accent: 0x0000_FF00,
        wash_a: 128,
        border_a: 0,
        border_scale_q4: 0,
    };
    apply_host_chrome_at(
        &mut pixels,
        10,
        10,
        0,
        0,
        10,
        10,
        0..0,
        Some(tray),
        true,
        Some(overlay),
    );
    let inverted_tray = 0x0000_FFFF;
    assert_eq!(
        pixels[5 * 10 + 5],
        blend_rgb(inverted_tray, overlay.accent, u32::from(overlay.wash_a)),
        "opaque tray pixels participate in both later host effects"
    );
    assert_ne!(
        pixels[5 * 10 + 5],
        0x00FF_0000,
        "the retired CPU order (effects before tray) is a real negative control"
    );
}

/// The CPU present places the frame with its chrome rows' bands continued,
/// then composites the tray: a full-width card over a chrome row (the stats
/// Deck in the head, whose bottom rule runs edge to edge) must reach the
/// remainder bands as the GPU blit's does, which samples the tray-composited
/// copy (a maximized `--cpu` window under weston read 5 of 26790 band pixels
/// off, first at (0,75), before this). Rows outside the chrome extent keep the
/// band colour, and the bell invert never flips a band.
#[test]
fn a_tray_over_a_chrome_row_continues_through_the_remainder_bands() {
    const BAND: u32 = 0x0011_2233;
    const RULE: u32 = 0x00C0_FFEE;
    let (fw, fh) = (10usize, 8usize);
    let src: Vec<u32> = (0..fw * fh).map(|i| 0x0040_0000 | i as u32).collect();
    let (dw, dh) = (fw + 5, fh + 2);
    let (ox, oy) = (
        aterm_render::band_offset(dw, fw),
        aterm_render::band_offset_y(dh, fh),
    );
    // The card: the frame's full width on row 3, opaque.
    let rgba: Vec<u8> = (0..fw).flat_map(|_| [0xC0, 0xFF, 0xEE, 0xFF]).collect();
    let tray = || aterm_gpu::TrayQuad {
        rgba: &rgba,
        pw: fw as u32,
        ph: 1,
        dx: 0,
        dy: 3,
        scrim: None,
    };
    for (edge_rows, invert) in [(0..5, false), (0..5, true), (0..2, false)] {
        let mut dst = vec![0u32; dw * dh];
        aterm_render::place_frame_bands(
            &mut dst,
            dw,
            dh,
            &src,
            fw,
            fh,
            false,
            BAND,
            edge_rows.clone(),
        );
        apply_host_chrome_at(
            &mut dst,
            dw,
            dh,
            ox,
            oy,
            fw,
            fh,
            edge_rows.clone(),
            Some(tray()),
            invert,
            None,
        );
        let y = (3 + oy) as usize;
        let x0 = ox as usize;
        let row = &dst[y * dw..y * dw + dw];
        let bands = row[..x0].iter().chain(&row[x0 + fw..]);
        if edge_rows.contains(&3) {
            assert!(
                bands.clone().all(|&p| p & 0x00ff_ffff == RULE),
                "{edge_rows:?} invert={invert}: the card's row reaches the window edge: {row:08x?}"
            );
        } else {
            assert!(
                bands.clone().all(|&p| p == BAND),
                "{edge_rows:?}: a terminal row's bands stay the band colour: {row:08x?}"
            );
        }
        // The row under it is a chrome row the card did not touch: its bands
        // continue the frame's own edge, as placed.
        if edge_rows.contains(&4) {
            let y = (4 + oy) as usize;
            assert_eq!(dst[y * dw] & 0x00ff_ffff, src[4 * fw]);
            assert_eq!(dst[y * dw + dw - 1] & 0x00ff_ffff, src[4 * fw + fw - 1]);
        }
    }
}

/// At a band offset the band-aware overlay twins paint the same
/// frame-relative pixels as at offset 0, shifted and never touching the
/// bands; a negative (crop) offset clips instead of panicking.
#[test]
fn band_aware_overlay_twins_shift_without_touching_bands() {
    let accent = 0x0050_FA7B;
    let (fw, fh) = (40usize, 24usize);
    // The offset-0 overlay the shifted frame is compared against.
    let mut a = vec![0x0010_2030u32; fw * fh];
    apply_drop_overlay_at(&mut a, fw, fh, 0, 0, fw, fh, accent);

    // Frame at (3, 3) inside a +7px surface: the overlaid content equals the
    // offset-0 overlay shifted by the band, and every band pixel is untouched.
    let band = 0x0001_0203u32;
    let (sw, sh) = (fw + 7, fh + 7);
    let mut s = vec![band; sw * sh];
    for y in 0..fh {
        for x in 0..fw {
            s[(y + 3) * sw + (x + 3)] = 0x0010_2030;
        }
    }
    apply_drop_overlay_at(&mut s, sw, sh, 3, 3, fw, fh, accent);
    for y in 0..sh {
        for x in 0..sw {
            let got = s[y * sw + x];
            let (fx, fy) = (x as i64 - 3, y as i64 - 3);
            if fx >= 0 && fy >= 0 && (fx as usize) < fw && (fy as usize) < fh {
                let want = a[fy as usize * fw + fx as usize];
                assert_eq!(got, want, "shifted overlay at ({x},{y})");
            } else {
                assert_eq!(got, band, "band at ({x},{y}) must be untouched");
            }
        }
    }

    // composite_tray_at: a band offset shifts the card.
    let card = crate::SettingsCard {
        scrim: None,
        rgba: vec![0xAA, 0xBB, 0xCC, 0xFF],
        pw: 1,
        ph: 1,
        dx: 1,
        dy: 0,
        fp: 0,
        geom: 0,
        raster_fingerprint_once: std::sync::OnceLock::new(),
    };
    let mut t2 = vec![0u32; 4 * 3];
    composite_tray_at(&mut t2, 4, 3, 1, 2, &card);
    assert_eq!(
        t2[2 * 4 + 2],
        0x00AA_BBCC,
        "card shifted by the band offset"
    );
    assert_eq!(t2.iter().filter(|&&p| p != 0).count(), 1);
    // A negative (crop) offset clips off-surface instead of panicking.
    let mut t3 = vec![0u32; 1];
    composite_tray_at(&mut t3, 1, 1, -2, -2, &card);
    assert_eq!(t3[0], 0);
}

/// A card's scrim composites as the window-sized card of that colour it
/// replaces: byte-identical to baking the colour into every pixel around the
/// card from the scrim's `top` row down, the banner rows above left clear.
/// Negative control: without it the frame around the card is untouched.
#[test]
fn a_tray_scrim_composites_as_the_window_sized_card_it_replaces() {
    let (w, h) = (12usize, 9usize);
    let base: Vec<u32> = (0..w * h)
        .map(|i| (i as u32).wrapping_mul(0x0003_0507) & 0x00FF_FFFF)
        .collect();
    let smoke = [0, 0, 0, 0x4D];
    let card_px = [0xE0, 0x20, 0x90, 0xFF];
    let (dx, dy, pw, ph, top) = (3u32, 4u32, 4u32, 2u32, 2u32);
    let small = card_px.repeat((pw * ph) as usize);
    let quad = |scrim| aterm_gpu::TrayQuad {
        rgba: &small,
        pw,
        ph,
        dx,
        dy,
        scrim,
    };
    let mut layered = base.clone();
    let scrim = aterm_gpu::TrayScrim { rgba: smoke, top };
    composite_tray_quad_at(&mut layered, w, h, 0, 0, quad(Some(scrim)));

    let mut full = Vec::with_capacity(w * h * 4);
    for y in 0..h as u32 {
        for x in 0..w as u32 {
            let on_card = (dx..dx + pw).contains(&x) && (dy..dy + ph).contains(&y);
            full.extend_from_slice(&if on_card {
                card_px
            } else if y >= top {
                smoke
            } else {
                [0; 4]
            });
        }
    }
    let mut baked = base.clone();
    let window_sized = aterm_gpu::TrayQuad {
        rgba: &full,
        pw: w as u32,
        ph: h as u32,
        dx: 0,
        dy: 0,
        scrim: None,
    };
    composite_tray_quad_at(&mut baked, w, h, 0, 0, window_sized);
    assert_eq!(
        layered, baked,
        "the layer is the baked card, pixel for pixel"
    );
    let banner = top as usize * w;
    assert_eq!(
        layered[..banner],
        base[..banner],
        "the banner rows stay clear"
    );

    let mut bare = base.clone();
    composite_tray_quad_at(&mut bare, w, h, 0, 0, quad(None));
    assert_ne!(bare, layered, "negative control: the scrim dims the window");
    // A crop offset past the surface clips instead of panicking.
    let mut tiny = vec![0x00FF_FFFFu32; 1];
    composite_tray_quad_at(&mut tiny, 1, 1, -5, -50, quad(Some(scrim)));
    assert_ne!(
        tiny[0], 0x00FF_FFFF,
        "a fill from above the surface still covers it"
    );
}

/// src-over math: `a==255` ⇒ src, `a==0` ⇒ dst untouched, `a==128` ⇒ rounded blend;
/// and an offset past the surface never panics / writes OOB.
#[test]
fn composite_tray_src_over_and_clamps() {
    let card = |rgba: Vec<u8>, dx: u32, dy: u32| crate::SettingsCard {
        scrim: None,
        rgba,
        pw: 1,
        ph: 1,
        dx,
        dy,
        fp: 0,
        geom: 0,
        raster_fingerprint_once: std::sync::OnceLock::new(),
    };
    // 1×1 card, full alpha → exactly the source color.
    let mut fb = vec![0x0010_2030u32; 4];
    composite_tray(&mut fb, 2, 2, &card(vec![0xAA, 0xBB, 0xCC, 0xFF], 0, 0));
    assert_eq!(fb[0], 0x00AA_BBCC);
    // Fully transparent → dst preserved (the live terminal shows through).
    let mut fb2 = vec![0x0012_3456u32; 1];
    composite_tray(&mut fb2, 1, 1, &card(vec![0xFF, 0xFF, 0xFF, 0x00], 0, 0));
    assert_eq!(fb2[0], 0x0012_3456);
    // Half alpha over black: src-over reduces to `(s*a + 127) / 255` per channel.
    let mut fb3 = vec![0u32; 1];
    composite_tray(&mut fb3, 1, 1, &card(vec![0xFF, 0x00, 0x80, 0x80], 0, 0));
    let exp = |s: u32, a: u32| (s * a + 127) / 255;
    assert_eq!(fb3[0], (exp(0xFF, 0x80) << 16) | exp(0x80, 0x80));
    // The same half-alpha source over a half-transparent renderer
    // destination composes alpha instead of forcing opaque:
    // T' = round(128 * 127 / 255) = 64.
    let mut translucent = vec![0x8010_2030u32];
    composite_tray(
        &mut translucent,
        1,
        1,
        &card(vec![0xFF, 0x00, 0x80, 0x80], 0, 0),
    );
    assert_eq!(translucent[0] >> 24, 64);
    // Offset entirely off-surface: no panic, no change.
    let mut fb4 = vec![0x00FF_FFFFu32; 1];
    composite_tray(&mut fb4, 1, 1, &card(vec![0, 0, 0, 0xFF], 5, 5));
    assert_eq!(fb4[0], 0x00FF_FFFF);
}

/// A topmost cell banner crops the selected tray as a borrowed row view:
/// the cached bytes stay untouched, a partial card resumes at the exact
/// first uncovered row, and a fully covered card disappears.
#[test]
fn tray_floor_crop_preserves_banner_and_exact_rgba_suffix() {
    let card = crate::SettingsCard {
        scrim: None,
        rgba: vec![
            0x10, 0, 0, 0xFF, 0x11, 0, 0, 0xFF, // row 0
            0x20, 0, 0, 0xFF, 0x21, 0, 0, 0xFF, // row 1
            0x30, 0, 0, 0xFF, 0x31, 0, 0, 0xFF, // row 2
            0x40, 0, 0, 0xFF, 0x41, 0, 0, 0xFF, // row 3
        ],
        pw: 2,
        ph: 4,
        dx: 0,
        dy: 1,
        fp: 0,
        geom: 0,
        raster_fingerprint_once: std::sync::OnceLock::new(),
    };
    let original = card.rgba.clone();
    let quad = tray_quad_below_y(&card, 3).expect("two uncovered rows");
    assert_eq!((quad.pw, quad.ph, quad.dx, quad.dy), (2, 2, 0, 3));
    assert_eq!(quad.rgba, &original[16..], "borrowed exact row suffix");
    assert_eq!(card.rgba, original, "the resident card is never mutated");

    let mut frame = vec![0x0001_0203; 2 * 5];
    composite_tray_quad_at(&mut frame, 2, 5, 0, 0, quad);
    assert!(
        frame[..2 * 3].iter().all(|&pixel| pixel == 0x0001_0203),
        "banner pixels above the floor stay untouched"
    );
    assert_eq!(frame[3 * 2], 0x0030_0000);
    assert_eq!(frame[4 * 2 + 1], 0x0041_0000);
    assert!(
        tray_quad_below_y(&card, 5).is_none(),
        "a card wholly inside the banner contributes no later pixels"
    );
}

fn channel_dist(a: u32, b: u32) -> u32 {
    let d = |s: u32| (((a >> s) & 0xff) as i32 - ((b >> s) & 0xff) as i32).unsigned_abs();
    d(16) + d(8) + d(0)
}

/// The border pixels land much closer to the accent than the interior wash,
/// and the interior is exactly the faint accent blend over the background.
#[test]
fn border_is_accent_heavy_interior_is_faint() {
    let (w, h) = (400usize, 300usize);
    let accent = 0x0050_FA7B;
    let mut px = vec![0x0000_0000u32; w * h]; // black background
    apply_drop_overlay(&mut px, w, h, accent);

    let corner = px[0]; // on the border
    let interior = px[(h / 2) * w + w / 2]; // far from any edge
    assert_ne!(corner, 0, "border pixel must be tinted");
    assert_ne!(interior, 0, "interior pixel must be washed");
    assert!(
        channel_dist(corner, accent) < channel_dist(interior, accent),
        "border should be nearer the accent than the interior"
    );
    assert_eq!(interior, blend_rgb(0, accent, DROP_WASH_ALPHA));
    assert!(drop_border_px(w, h) >= 2);
}

/// Zero dimensions are a no-op, never a panic.
#[test]
fn zero_dims_is_noop() {
    let mut px = vec![0x0011_2233u32; 4];
    apply_drop_overlay(&mut px, 0, 0, 0x00ff_ffff);
    assert_eq!(px, vec![0x0011_2233u32; 4]);
}

/// The packed format is preserved: the renderer's top transmittance byte
/// survives the presentation-only tint exactly, matching the GPU blit.
#[test]
fn overlay_preserves_transmittance() {
    let mut px = vec![0x7fab_cdefu32; 10 * 10];
    apply_drop_overlay(&mut px, 10, 10, 0x00ff_ffff);
    assert!(px.iter().all(|p| p >> 24 == 0x7f));
}
