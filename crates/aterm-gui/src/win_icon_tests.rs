// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{dpi_for_scale, icon_px};

const ICO: &[u8] = include_bytes!("../assets/aterm.ico");

fn frames() -> Vec<(u32, Vec<u8>)> {
    let u16_at = |o: usize| u16::from_le_bytes([ICO[o], ICO[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([ICO[o], ICO[o + 1], ICO[o + 2], ICO[o + 3]]);
    assert_eq!((u16_at(0), u16_at(2)), (0, 1), "not an .ico (ICONDIR)");
    (0..usize::from(u16_at(4)))
        .map(|i| {
            let e = 6 + 16 * i;
            let edge = if ICO[e] == 0 { 256 } else { u32::from(ICO[e]) };
            assert_eq!(ICO[e], ICO[e + 1], "frame {i} is not square");
            assert_eq!(u16_at(e + 6), 32, "frame {edge} is not 32 bpp");
            let (len, off) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
            let data = &ICO[off..off + len];
            if data.starts_with(b"\x89PNG") {
                let (rgba, w, h) = aterm_png::decode_rgba8(data, 256).expect("PNG frame decodes");
                assert_eq!(
                    (w, h),
                    (edge as usize, edge as usize),
                    "PNG frame {edge} size"
                );
                return (edge, rgba);
            }
            let d32 =
                |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
            assert_eq!(
                (d32(4), d32(8)),
                (edge, edge * 2),
                "DIB frame {edge} header"
            );
            let px = &data[40..40 + (edge * edge * 4) as usize];
            let rgba = px
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect();
            (edge, rgba)
        })
        .collect()
}

#[test]
fn every_icon_size_windows_asks_for_is_a_frame() {
    let sizes: Vec<u32> = frames().iter().map(|(edge, _)| *edge).collect();
    for pct in [100u32, 125, 150, 175, 200, 225, 250, 300] {
        let (small, big) = icon_px(96 * pct / 100);
        assert!(
            sizes.contains(&small),
            "no {small} px small icon for {pct} % in {sizes:?}"
        );
        assert!(
            sizes.contains(&big),
            "no {big} px big icon for {pct} % in {sizes:?}"
        );
    }
    for shell in [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256] {
        assert!(sizes.contains(&shell), "no {shell} px frame in {sizes:?}");
    }
}

#[test]
fn every_icon_frame_is_anti_aliased() {
    for (edge, rgba) in frames() {
        let alpha = || rgba.as_chunks::<4>().0.iter().map(|p| p[3]);
        let partial = alpha().filter(|a| (1..255).contains(a)).count();
        let clear = alpha().filter(|&a| a == 0).count();
        assert!(
            partial > 0,
            "{edge} px frame has no partial alpha (aliased edge)"
        );
        assert!(clear > 0, "{edge} px frame has no transparent corner");
    }
}

#[test]
fn icon_sizes_follow_the_window_dpi() {
    for (scale, small, big) in [
        (1.0, 16, 32),
        (1.25, 20, 40),
        (1.5, 24, 48),
        (1.75, 28, 56),
        (2.0, 32, 64),
        (2.5, 40, 80),
        (3.0, 48, 96),
    ] {
        assert_eq!(
            icon_px(dpi_for_scale(scale)),
            (small, big),
            "at scale {scale}"
        );
    }
    assert!(icon_px(dpi_for_scale(f64::MAX)).0 > 0);
}
