// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Swapchain band placement: the swapchain is the window's raw physical size (a grid-quantized
// one would make CAMetalLayer rescale non-integrally), the frame sits at the centred remainder
// offset (`aterm_render::band_offset`), and the leftover bands are painted theme bg in the same
// blit pass. Drives the real blit pipeline (`vs_blit`/`fs_blit`, `BlitUniform`) via
// `blit_to_sized_for_test` against the offscreen readback and the CPU twin
// `aterm_render::place_frame_bands`. No GPU or no font: COULD NOT RUN.

use aterm_core::terminal::Terminal;
use aterm_gpu::{DropOverlay, GpuRenderer, PresentCrop};
use aterm_render::{RenderInput, Theme, band_offset, band_offset_y, place_frame_bands};

use crate::common;

const ROWS: usize = 6;
const COLS: usize = 24;

/// The blit seams read the wgpu offscreen, so this suite drives the wgpu oracle arm.
fn fresh_gpu() -> GpuRenderer {
    common::wgpu_oracle(18.0, Theme::default())
}

/// A prompt, coloured text and glyphs, so placement is pinned over real pixels, not a clear.
fn representative_input() -> RenderInput {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ band check >_\r\n");
    term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[34mBLUE\x1b[0m\r\n");
    term.process(b"\x1b[1mbold\x1b[0m plain 0123456789");
    term.cell_frame(ROWS, COLS)
}

/// Grid fit + 7 px per axis: content byte-identical at the centred offset (3 leading / 4
/// trailing), bands exactly theme bg, no scaling, and CPU placement agrees.
#[test]
fn window_seven_px_past_grid_fit_offsets_content_without_scaling() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();

    // The offscreen readback is the exact-fit content (blitted 1:1; see blit_invert.rs).
    let source = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (source.width, source.height);

    // 7 px more per axis without a new column/row: the swapchain is the raw size and the
    // frame lands at the centred band offset.
    let (dw, dh) = (fw + 7, fh + 7);
    let blit = gpu.blit_to_sized_for_test(&mut win, false, dw as u32, dh as u32);
    assert_eq!(
        (blit.width, blit.height),
        (dw, dh),
        "blit target must be the raw window size"
    );

    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    assert_eq!(
        (ox, oy),
        (3, if cfg!(target_os = "linux") { 0 } else { 3 }),
        "x: a 7px remainder splits 3 leading / 4 trailing; y: platform policy"
    );

    let bg = Theme::default().bg & 0x00ff_ffff;
    let (mut content_px, mut band_px) = (0usize, 0usize);
    for y in 0..dh {
        for x in 0..dw {
            let got = blit.pixels[y * dw + x] & 0x00ff_ffff;
            let (sx, sy) = (x as i64 - ox, y as i64 - oy);
            if sx >= 0 && sy >= 0 && (sx as usize) < fw && (sy as usize) < fh {
                let want = source.pixels[sy as usize * fw + sx as usize] & 0x00ff_ffff;
                assert_eq!(
                    got, want,
                    "content NOT byte-identical at ({x},{y}) — the present is scaling"
                );
                content_px += 1;
            } else {
                assert_eq!(
                    got, bg,
                    "band pixel at ({x},{y}) must be exactly the theme bg"
                );
                band_px += 1;
            }
        }
    }
    // Non-vacuity: both branches cover the whole surface.
    assert_eq!(content_px, fw * fh);
    assert_eq!(band_px, dw * dh - fw * fh);

    // The softbuffer twin over the same source equals the GPU blit byte-for-byte.
    let mut cpu = vec![0u32; dw * dh];
    place_frame_bands(&mut cpu, dw, dh, &source.pixels, fw, fh, false, bg, 0..0);
    for (i, (&c, &g)) in cpu.iter().zip(blit.pixels.iter()).enumerate() {
        assert_eq!(
            c & 0x00ff_ffff,
            g & 0x00ff_ffff,
            "CPU placement and GPU blit diverge at pixel {i}"
        );
    }
}

/// The bell flash inverts content only: bands are chrome and stay theme bg.
#[test]
fn bands_do_not_invert_on_bell() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();
    let source = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (source.width, source.height);
    let (dw, dh) = (fw + 5, fh + 2);
    let blit = gpu.blit_to_sized_for_test(&mut win, true, dw as u32, dh as u32);

    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    let bg = Theme::default().bg & 0x00ff_ffff;
    for y in 0..dh {
        for x in 0..dw {
            let got = blit.pixels[y * dw + x] & 0x00ff_ffff;
            let (sx, sy) = (x as i64 - ox, y as i64 - oy);
            if sx >= 0 && sy >= 0 && (sx as usize) < fw && (sy as usize) < fh {
                let want =
                    (source.pixels[sy as usize * fw + sx as usize] ^ 0x00ff_ffff) & 0x00ff_ffff;
                assert_eq!(got, want, "content must be bell-inverted at ({x},{y})");
            } else {
                assert_eq!(
                    got, bg,
                    "band at ({x},{y}) must NOT invert (chrome, not content)"
                );
            }
        }
    }
}

/// A swapchain smaller than the frame (transient mid-drag) crops it, centred, 1:1.
#[test]
fn undersized_swapchain_crops_centred_never_scales() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();
    let source = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (source.width, source.height);
    let (dw, dh) = (fw - 9, fh - 3);
    let blit = gpu.blit_to_sized_for_test(&mut win, false, dw as u32, dh as u32);

    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    assert!(ox < 0, "an undersized dst must crop horizontally, centred");
    if cfg!(target_os = "linux") {
        assert_eq!(oy, 0, "the top-pinned crop keeps the frame's top rows");
    } else {
        assert!(oy < 0, "an undersized dst must be a centred crop");
    }
    for y in 0..dh {
        for x in 0..dw {
            let got = blit.pixels[y * dw + x] & 0x00ff_ffff;
            let (sx, sy) = ((x as i64 - ox) as usize, (y as i64 - oy) as usize);
            let want = source.pixels[sy * fw + sx] & 0x00ff_ffff;
            assert_eq!(got, want, "crop must be a 1:1 texel fetch at ({x},{y})");
        }
    }
}

/// The present shader applies the asymmetric frontend crop before bell inversion and the drop
/// overlay: with a five-row removal, three raw rows above and two below become chrome, and
/// the overlay border follows the visible interval's edges.
#[test]
fn asymmetric_crop_keeps_raw_bands_outside_invert_and_drop_overlay() {
    let mut gpu = fresh_gpu();
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();
    let source = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (source.width, source.height);
    assert!(fh > 9, "fixture needs an interior beyond the crop border");

    let crop = PresentCrop {
        source_y: 3,
        height: (fh - 5) as u32,
    };
    let blit = gpu.blit_to_sized_cropped_for_test(
        &mut win,
        true,
        Some(DropOverlay {
            accent: 0x00ff_0000,
            wash_a: 0,
            border_a: u8::MAX,
            border_scale_q4: 0,
        }),
        crop,
        fw as u32,
        fh as u32,
    );
    let bg = Theme::default().bg & 0x00ff_ffff;
    let red = 0x00ff_0000;
    let x = fw / 2;

    // The three leading and two trailing raw rows are chrome. Bell invert makes this
    // non-vacuous though the padding is itself bg: sampling those rows would give !bg.
    for y in (0..3).chain((fh - 2)..fh) {
        for px in &blit.pixels[y * fw..(y + 1) * fw] {
            assert_eq!(
                px & 0x00ff_ffff,
                bg,
                "cropped raw row {y} must remain uninverted, unwashed chrome"
            );
        }
    }

    // The border is at least two device pixels: pin both visible edges at a centre column.
    for y in [3, 4, fh - 4, fh - 3] {
        assert_eq!(
            blit.pixels[y * fw + x] & 0x00ff_ffff,
            red,
            "drop border must follow the cropped visible edge at row {y}"
        );
    }

    // Wash alpha is zero, so the interior stays the exact bell-inverted source texel.
    let interior_y = 6;
    assert_eq!(
        blit.pixels[interior_y * fw + x] & 0x00ff_ffff,
        (source.pixels[interior_y * fw + x] ^ 0x00ff_ffff) & 0x00ff_ffff,
        "crop-local overlay must not leak into its interior when wash alpha is zero"
    );
}

/// Chrome rows (here the second is a metered row with fill and track gutters) continue their
/// edge pixels through the leftover bands; every other band pixel stays theme bg, and
/// `place_frame_bands` with the same `edge_rows` agrees.
#[test]
fn chrome_rows_continue_their_edge_pixels_through_the_leftover_bands() {
    let mut gpu = fresh_gpu();
    const FILL: u32 = 0x0050_FA7B;
    const TRACK: u32 = 0x0044_4750;
    const BAND: u32 = 0x0030_3135;
    gpu.set_pad(9);
    gpu.set_chrome_bleed(Some(aterm_render::ChromeBleed {
        rows: 2,
        first: 0,
        color: BAND,
        seam: None,
        top_extends_cells: false,
        row_edges: [
            Some(aterm_render::ChromeRowEdges {
                row: 1,
                left: FILL,
                right: TRACK,
            }),
            None,
            None,
        ],
    }));
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();
    let source = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (source.width, source.height);
    let edge_rows = gpu.chrome_extent_px(fh);
    assert!(
        edge_rows.start == 0 && edge_rows.end > 0 && edge_rows.end < fh,
        "two chrome rows from the top: {edge_rows:?} of {fh}"
    );
    let (dw, dh) = (fw + 7, fh + 7);
    let blit = gpu.blit_to_sized_for_test(&mut win, false, dw as u32, dh as u32);
    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    let bg = Theme::default().bg & 0x00ff_ffff;
    let mut saw = [false; 3]; // band tone, fill, track continued
    for y in 0..dh {
        for x in 0..dw {
            let got = blit.pixels[y * dw + x] & 0x00ff_ffff;
            let (sx, sy) = (x as i64 - ox, y as i64 - oy);
            let in_rows = sy >= 0 && (sy as usize) < fh;
            let in_cols = sx >= 0 && (sx as usize) < fw;
            if in_rows && in_cols {
                continue;
            }
            if in_rows && edge_rows.contains(&(sy as usize)) {
                let row = sy as usize * fw;
                let edge = if sx < 0 {
                    source.pixels[row]
                } else {
                    source.pixels[row + fw - 1]
                } & 0x00ff_ffff;
                assert_eq!(
                    got, edge,
                    "chrome band pixel at ({x},{y}) continues the edge"
                );
                for (i, tone) in [BAND, FILL, TRACK].into_iter().enumerate() {
                    saw[i] |= got == tone;
                }
            } else {
                assert_eq!(got, bg, "band pixel at ({x},{y}) is the theme bg");
            }
        }
    }
    assert_eq!(
        saw, [true; 3],
        "the band tone, the fill and the track all reached the edge"
    );
    let mut cpu = vec![0u32; dw * dh];
    place_frame_bands(
        &mut cpu,
        dw,
        dh,
        &source.pixels,
        fw,
        fh,
        false,
        bg,
        edge_rows,
    );
    for (i, (&c, &g)) in cpu.iter().zip(blit.pixels.iter()).enumerate() {
        assert_eq!(
            c & 0x00ff_ffff,
            g & 0x00ff_ffff,
            "CPU placement and GPU blit diverge at pixel {i}"
        );
    }
}

/// `ChromeBleed::first` reaches the presenters: beside a strip row with no surface of its own
/// (macOS's in-grid strip) the leftover bands stay theme bg even where that row's edge pixel
/// differs; GPU (`chrome_y0`) and CPU (`place_frame_bands`) agree.
#[test]
fn rows_above_the_chrome_start_keep_the_flat_leftover_band() {
    let mut gpu = fresh_gpu();
    const BAND: u32 = 0x0030_3135;
    gpu.set_pad(0);
    gpu.set_chrome_bleed(Some(aterm_render::ChromeBleed {
        rows: 2,
        first: 1,
        color: BAND,
        seam: None,
        top_extends_cells: false,
        row_edges: [None; aterm_render::CHROME_ROW_EDGES],
    }));
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    // Row 0 — the strip — wears red cells at both edges, so continuing it
    // would be visible; row 1 is the band.
    term.process(format!("\x1b[41m \x1b[0m\x1b[1;{COLS}H\x1b[41m \x1b[0m\r\nband").as_bytes());
    let input = term.cell_frame(ROWS, COLS);
    let mut win = aterm_gpu::WindowGpu::new();
    let source = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (source.width, source.height);
    let edge_rows = gpu.chrome_extent_px(fh);
    assert!(
        edge_rows.start > 0 && edge_rows.end > edge_rows.start && edge_rows.end < fh,
        "the band row alone: {edge_rows:?} of {fh}"
    );
    let bg = Theme::default().bg & 0x00ff_ffff;
    let (dw, dh) = (fw + 7, fh + 7);
    let blit = gpu.blit_to_sized_for_test(&mut win, false, dw as u32, dh as u32);
    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    let mut strip_edge_differs = false;
    for y in 0..dh {
        for x in 0..dw {
            let got = blit.pixels[y * dw + x] & 0x00ff_ffff;
            let (sx, sy) = (x as i64 - ox, y as i64 - oy);
            let in_rows = sy >= 0 && (sy as usize) < fh;
            let in_cols = sx >= 0 && (sx as usize) < fw;
            if in_rows && in_cols {
                continue;
            }
            if in_rows && (sy as usize) < edge_rows.start {
                let row = sy as usize * fw;
                let edge = if sx < 0 {
                    source.pixels[row]
                } else {
                    source.pixels[row + fw - 1]
                } & 0x00ff_ffff;
                strip_edge_differs |= edge != bg;
                assert_eq!(got, bg, "beside the strip row ({x},{y}): the flat band");
            }
        }
    }
    assert!(
        strip_edge_differs,
        "non-vacuous: the strip row's own edge pixel is not the band colour"
    );
    let mut cpu = vec![0u32; dw * dh];
    place_frame_bands(
        &mut cpu,
        dw,
        dh,
        &source.pixels,
        fw,
        fh,
        false,
        bg,
        edge_rows,
    );
    for (i, (&c, &g)) in cpu.iter().zip(blit.pixels.iter()).enumerate() {
        assert_eq!(
            c & 0x00ff_ffff,
            g & 0x00ff_ffff,
            "CPU placement and GPU blit diverge at pixel {i}"
        );
    }
}
