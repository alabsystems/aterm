// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The `wght` axis tag, in the same big-endian `u32` spelling the renderer
/// stores coords in ([`crate::variation::WGHT_TAG`]).
const WGHT: u32 = u32::from_be_bytes(*b"wght");

/// The embedded DejaVu Sans Mono (the default `embedded-font` feature), so these tests
/// never depend on system font files.
fn dejavu() -> &'static [u8] {
    crate::embedded_font()
}

fn full_instance(px: f32) -> HintingInstance {
    HintingInstance::new(dejavu(), 0, px, &[], HintMode::Full.options().unwrap()).unwrap()
}

/// Per-row peak coverage ≥ `t` — a row whose brightest texel is near-full
/// renders a solid (not washed) stroke crossing there.
fn rows_with_peak(cov: &[u8], w: usize, h: usize, t: u8) -> usize {
    (0..h)
        .filter(|&r| (0..w).any(|c| cov[r * w + c] >= t))
        .count()
}

/// At 12px the grid-fitted 'l' has a near-saturated stem texel on nearly every row, where
/// the unhinted raster's fractional stem phase splits the stem across two columns.
#[test]
fn full_hinting_snaps_the_l_stem_at_12px() {
    let hint = full_instance(12.0);
    let gid = unicode_gid(dejavu(), 0, 'l').unwrap();
    let (w, h, _, _, _, cov) = hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
    let hinted_rows = rows_with_peak(&cov, w, h, 240);
    let fd = crate::font::Font::from_bytes(dejavu(), crate::font::FontSettings::default()).unwrap();
    let (m, b) = fd.rasterize('l', 12.0);
    let fontdue_rows = rows_with_peak(&b, m.width, m.height, 240);
    assert!(
        hinted_rows >= h.saturating_sub(2),
        "hinted 'l' at 12px must peak >=240 on nearly every row ({hinted_rows}/{h} rows: {cov:?})"
    );
    assert!(
        hinted_rows > fontdue_rows,
        "grid fitting must strictly beat the unhinted raster \
         (hinted {hinted_rows}/{h} rows vs fontdue {fontdue_rows}/{})",
        m.height
    );
}

/// The reported advance is linear (scaled hmtx), not the hinted advance, and bit-identical
/// to the unhinted `Font`'s: cell geometry must not move with hinting.
#[test]
fn advance_stays_linear() {
    let jetbrains: &[u8] = include_bytes!("../tests/fixtures/jetbrains-mono.ttf");
    for bytes in [dejavu(), jetbrains] {
        let font =
            crate::font::Font::from_bytes(bytes, crate::font::FontSettings::default()).unwrap();
        for px in [12.0, 13.0] {
            let hint =
                HintingInstance::new(bytes, 0, px, &[], HintMode::Full.options().unwrap()).unwrap();
            for ch in ['M', 'i', 'W', '0'] {
                let gid = unicode_gid(bytes, 0, ch).unwrap();
                let (.., adv, _) = hinted_glyph_raster(bytes, 0, gid, px, &hint, &[]).unwrap();
                let unhinted = font.metrics(ch, px).advance_width;
                assert_eq!(
                    adv.to_bits(),
                    unhinted.to_bits(),
                    "{ch:?} @{px}px: hinted-path advance {adv} != unhinted advance {unhinted}"
                );
            }
        }
    }
}

/// A space has no outline: blank raster carrying just the advance (the
/// variation path's blank-glyph convention, which the blit treats as a
/// no-op).
#[test]
fn space_is_blank_with_advance() {
    let hint = full_instance(12.0);
    let gid = unicode_gid(dejavu(), 0, ' ').unwrap();
    let (w, h, xmin, ymin, adv, cov) =
        hinted_glyph_raster(dejavu(), 0, gid, 12.0, &hint, &[]).unwrap();
    assert_eq!((w, h, xmin, ymin), (0, 0, 0, 0));
    assert!(cov.is_empty());
    assert!(adv > 0.0);
}

/// `HintMode::Off` yields no options, so the bank hands out no instance and the unhinted
/// path runs bit-for-bit.
#[test]
fn off_mode_disables_the_bank() {
    let mut bank = HintBank::default();
    let bytes = dejavu();
    let key = bytes.as_ptr() as usize;
    assert!(
        bank.instance(key, bytes, 0, 12.0, HintMode::Off, &[], 0)
            .is_none()
    );
    assert!(
        bank.instance(key, bytes, 0, 12.0, HintMode::Full, &[], 0)
            .is_some()
    );
}

/// Re-fill the same recorded outline with a 4 px slack ring (four times production's) and
/// crop the ink box back out.
fn generous_slack_raster(pen: &PathPen, w: usize, h: usize) -> Vec<u8> {
    const SLACK: usize = 4;
    let (x_min, y_max) = (pen.min_x.floor(), pen.max_y.ceil());
    let mut ras = crate::raster::Rasterizer::new(w + 2 * SLACK, h + 2 * SLACK);
    pen.fill(&mut ras, x_min - SLACK as f32, y_max + SLACK as f32);
    let gw = w + 2 * SLACK;
    let mut cov = vec![0u8; w * h];
    ras.for_each_pixel(|i, a| {
        let (gx, gy) = (i % gw, i / gw);
        let (Some(x), Some(y)) = (gx.checked_sub(SLACK), gy.checked_sub(SLACK)) else {
            return;
        };
        if x < w && y < h {
            cov[y * w + x] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    cov
}

/// `(area, perimeter)` of the fitted outline in px² / px, by the shoelace
/// formula over a fine flattening.
fn area_and_perimeter(pen: &PathPen) -> (f32, f32) {
    const SUB: usize = 32;
    let (mut area, mut perim) = (0.0f32, 0.0f32);
    let mut poly: Vec<(f32, f32)> = Vec::new();
    let (mut start, mut last) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let close = |poly: &mut Vec<(f32, f32)>, area: &mut f32, perim: &mut f32| {
        for i in 0..poly.len() {
            let ((x0, y0), (x1, y1)) = (poly[i], poly[(i + 1) % poly.len()]);
            *area += x0 * y1 - x1 * y0;
            *perim += ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        }
        poly.clear();
    };
    for c in &pen.cmds {
        match *c {
            Cmd::Move(x, y) => {
                close(&mut poly, &mut area, &mut perim);
                start = (x, y);
                last = start;
                poly.push(start);
            }
            Cmd::Line(x, y) => {
                last = (x, y);
                poly.push(last);
            }
            Cmd::Quad(cx, cy, x, y) => {
                for i in 1..=SUB {
                    let t = i as f32 / SUB as f32;
                    let m = 1.0 - t;
                    poly.push((
                        m * m * last.0 + 2.0 * m * t * cx + t * t * x,
                        m * m * last.1 + 2.0 * m * t * cy + t * t * y,
                    ));
                }
                last = (x, y);
            }
            Cmd::Curve(ax, ay, bx, by, x, y) => {
                for i in 1..=SUB {
                    let t = i as f32 / SUB as f32;
                    let m = 1.0 - t;
                    let (m2, t2) = (m * m, t * t);
                    poly.push((
                        m2 * m * last.0 + 3.0 * m2 * t * ax + 3.0 * m * t2 * bx + t2 * t * x,
                        m2 * m * last.1 + 3.0 * m2 * t * ay + 3.0 * m * t2 * by + t2 * t * y,
                    ));
                }
                last = (x, y);
            }
            Cmd::Close => {
                close(&mut poly, &mut area, &mut perim);
                last = start;
                poly.push(start);
            }
        }
    }
    close(&mut poly, &mut area, &mut perim);
    ((area * 0.5).abs(), perim)
}

/// The ppem sweep's glyphs: printable ASCII, the box-drawing and block run, a Nerd Font PUA
/// sample (all the bundled symbols face carries), and a CJK sample for a face that has one.
fn sweep_chars() -> Vec<char> {
    let mut v: Vec<char> = (' '..='~').collect();
    v.extend("─│┌┐└┘├┤┬┴┼━┃█▀▄▌▐░▒▓".chars());
    v.extend("\u{e0b0}\u{e0b2}\u{e5ff}\u{e62b}\u{e706}\u{e7a8}".chars());
    v.extend("\u{f005}\u{f00c}\u{f00d}\u{f011}\u{f015}\u{f09b}".chars());
    v.extend("\u{f120}\u{f121}\u{f0c9}\u{f1d3}\u{f269}\u{f4a0}".chars());
    v.extend("一二日本語漢字".chars());
    v
}

/// No glyph rasterizes as a filled block at any desktop ppem, in any mode. A fitted outline
/// sitting exactly on the grid's left edge can lose a scanline's area in [`crate::raster`]
/// and smear the glyph into a block (see `variation::RASTER_PAD`; on the default face `'2'`
/// at ppem 19, a 125% desktop, and `'?'` at ppem 17).
#[test]
fn ppem_sweep_no_glyph_rasterizes_as_a_filled_block() {
    let faces: [(&str, &[u8]); 2] = [
        ("DejaVuSansMono (embedded)", dejavu()),
        (
            "SymbolsNerdFontMono (embedded)",
            crate::embedded_symbols_font(),
        ),
    ];
    let chars = sweep_chars();
    let modes = [HintMode::Full, HintMode::Light];
    let mut checked = 0usize;
    for (name, bytes) in faces {
        for mode in modes {
            for px in 12..=40 {
                let target = mode.options().expect("the hinting modes carry a target");
                let px = px as f32;
                let hint = HintingInstance::new(bytes, 0, px, &[], target)
                    .expect("a hinting instance at every desktop ppem");
                for &ch in &chars {
                    let Some(gid) = unicode_gid(bytes, 0, ch) else {
                        continue;
                    };
                    let (w, h, _, _, _, cov) = hinted_glyph_raster(bytes, 0, gid, px, &hint, &[])
                        .expect("the hinted raster never fails on a bundled face");
                    if cov.is_empty() {
                        // blank glyph (space): advance only
                        continue;
                    }
                    checked += 1;
                    let mut pen = PathPen::default();
                    hint.draw(bytes, 0, gid, &mut pen)
                        .expect("the outline we just drew still draws");

                    let reference = generous_slack_raster(&pen, w, h);
                    let worst = cov
                        .iter()
                        .zip(&reference)
                        .map(|(a, b)| i32::from(*a) - i32::from(*b))
                        .map(i32::abs)
                        .max()
                        .unwrap_or(0);
                    assert!(
                        worst <= 1,
                        "{name} {mode:?} {px}px {ch:?} (U+{:04X}): coverage moved by \
                         {worst}/255 when the fill grid gained slack — the outline is \
                         sitting on the grid boundary and the rasterizer lost a scanline",
                        ch as u32
                    );

                    let ink = cov.iter().map(|&v| f32::from(v)).sum::<f32>() / 255.0;
                    let (area, perim) = area_and_perimeter(&pen);
                    let slack = 0.06 * perim + 0.75;
                    assert!(
                        (ink - area).abs() <= slack,
                        "{name} {mode:?} {px}px {ch:?} (U+{:04X}): {w}x{h} raster carries \
                         {ink:.2}px² of ink for an outline enclosing {area:.2}px² \
                         (perimeter {perim:.1}px, allowed drift {slack:.2}px²)",
                        ch as u32
                    );
                }
            }
        }
    }
    assert!(
        checked > 5_000,
        "the ppem sweep must actually rasterize thousands of glyphs, got {checked}"
    );
}

/// At a variable instance the hinted path's advance is the instantiated unhinted raster's,
/// to the bit: both read `hmtx` + `HVAR` at the same coords through the same reader.
#[test]
fn advance_at_an_instance_is_the_unhinted_one() {
    let bytes = crate::display_face_bytes("engraved").expect("the bundled engraved face");
    let bold = [(WGHT, 700.0)];
    let mut moved = 0usize;
    for px in [12.0f32, 13.0, 16.0] {
        let hint =
            HintingInstance::new(bytes, 0, px, &bold, HintMode::Full.options().unwrap()).unwrap();
        for ch in ['M', 'i', 'W', '0', '&'] {
            let gid = unicode_gid(bytes, 0, ch).unwrap();
            let (.., adv, _) = hinted_glyph_raster(bytes, 0, gid, px, &hint, &bold).unwrap();
            let (.., unhinted, _) =
                crate::variation::varied_glyph_raster(bytes, 0, &bold, gid, px).unwrap();
            assert_eq!(
                adv.to_bits(),
                unhinted.to_bits(),
                "{ch:?} @{px}px wght 700: hinted advance {adv} != unhinted {unhinted}"
            );
            let (.., default_cut, _) = hinted_glyph_raster(bytes, 0, gid, px, &hint, &[]).unwrap();
            moved += usize::from(default_cut.to_bits() != adv.to_bits());
        }
    }
    assert!(
        moved > 0,
        "control: no probed advance differs between wght 700 and the default cut, \
         so the equality above cannot tell the two instances apart"
    );
}

/// The bank keys on the variation slot, so two instantiations of the same bytes at one px
/// are distinct instances: the regular and `wght`≈700 bold cuts of a variable face with no
/// Bold sibling (Windows' Cascadia Mono) coexist in one atlas.
#[test]
fn bank_keys_on_the_variation_slot() {
    let mut bank = HintBank::default();
    let bytes = dejavu();
    let ptr = bytes.as_ptr() as usize;
    let a = bank
        .instance(ptr, bytes, 0, 12.0, HintMode::Full, &[], 0)
        .unwrap();
    let b = bank
        .instance(ptr, bytes, 0, 12.0, HintMode::Full, &[(WGHT, 700.0)], 2)
        .unwrap();
    assert!(
        !std::sync::Arc::ptr_eq(&a, &b),
        "slot 0 and slot 2 must not share one hinting instance"
    );
    let a2 = bank
        .instance(ptr, bytes, 0, 12.0, HintMode::Full, &[], 0)
        .unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &a2));
}
