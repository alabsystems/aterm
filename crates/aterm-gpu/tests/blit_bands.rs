// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// W1 READBACK PIN — kill the compositor stretch (audit sin 1).
//
// The sin: the swapchain was sized to the grid-quantized frame (`cols*cell_w +
// 2*pad`), never the window's raw physical pixels, so CAMetalLayer's default
// resize gravity non-integrally rescaled the whole frame after nearly any
// drag/tile/zoom (~1.005x, permanent softness). The fix sizes the swapchain to
// the RAW window pixels and places the frame at the centred remainder offset
// (`aterm_render::band_offset`), painting the leftover bands theme-bg in the
// same blit pass.
//
// THE PIN (from the brief): a window of exactly grid-fit + 7px must render
// BYTE-IDENTICAL to the exact-fit content, offset by the pad split, with ZERO
// scaling — and every band pixel must be exactly the theme background.
//
// Drives the REAL present blit pipeline (`vs_blit`/`fs_blit` + the real
// `BlitUniform`) into a readable swapchain stand-in via the test-only
// `blit_to_sized_for_test` (the swapchain itself isn't readable headless), and
// asserts against the offscreen readback (the single source of truth). Also
// pins CPU/GPU agreement: `aterm_render::place_frame_bands` (the softbuffer
// twin) over the same source must equal the GPU blit output byte-for-byte.
//
// Gated: no GPU or no system font => the test no-ops (returns).

use aterm_core::terminal::Terminal;
use aterm_gpu::{DropOverlay, GpuRenderer, PresentCrop};
use aterm_render::{RenderInput, Theme, band_offset, band_offset_y, place_frame_bands};

const ROWS: usize = 6;
const COLS: usize = 24;

fn fresh_gpu() -> Option<GpuRenderer> {
    match GpuRenderer::new(18.0, Theme::default()) {
        Ok(mut g) => {
            // THE FLIP: this suite drives the WGPU ORACLE arm's blit seams
            // (they read the wgpu offscreen); post-flip the oracle must be
            // asked for by name.
            #[cfg(target_os = "macos")]
            g.disarm_metal_for_oracle();
            Some(g)
        }
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
}

/// A representative changed frame: a prompt, coloured text and a glyph, so the
/// placement is pinned over real glyph + colour pixels (not a flat clear).
fn representative_input() -> RenderInput {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ band check >_\r\n");
    term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[34mBLUE\x1b[0m\r\n");
    term.process(b"\x1b[1mbold\x1b[0m plain 0123456789");
    term.cell_frame(ROWS, COLS)
}

/// THE PIN: exact grid fit + 7px per axis => content byte-identical at the
/// centred offset (3 leading / 4 trailing), bands exactly the theme bg, zero
/// scaling — and the CPU placement twin agrees with the GPU blit byte-for-byte.
#[test]
fn window_seven_px_past_grid_fit_offsets_content_without_scaling() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();

    // The offscreen readback IS the exact-fit content (the single source of
    // truth the exact-fit present blits 1:1 — proven byte-identical by
    // tests/blit_invert.rs).
    let source = gpu.render_input(&mut win, &input, None);
    let (fw, fh) = (source.width, source.height);

    // The window grew 7px per axis without gaining a column/row: the swapchain
    // is the RAW size; the frame must land at the centred band offset.
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
    // NON-VACUITY: both branches genuinely exercised, over the whole surface.
    assert_eq!(content_px, fw * fh);
    assert_eq!(band_px, dw * dh - fw * fh);

    // CPU/GPU AGREEMENT: the softbuffer twin over the same source must equal
    // the GPU blit byte-for-byte — both backends absorb the remainder the same.
    let mut cpu = vec![0u32; dw * dh];
    place_frame_bands(&mut cpu, dw, dh, &source.pixels, fw, fh, false, bg);
    for (i, (&c, &g)) in cpu.iter().zip(blit.pixels.iter()).enumerate() {
        assert_eq!(
            c & 0x00ff_ffff,
            g & 0x00ff_ffff,
            "CPU placement and GPU blit diverge at pixel {i}"
        );
    }
}

/// The bell flash inverts CONTENT only: bands are chrome and stay theme-bg.
#[test]
fn bands_do_not_invert_on_bell() {
    let Some(mut gpu) = fresh_gpu() else { return };
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

/// A swapchain SMALLER than the frame (transient mid-drag) crops the frame,
/// centred — still 1:1, never scaled.
#[test]
fn undersized_swapchain_crops_centred_never_scales() {
    let Some(mut gpu) = fresh_gpu() else { return };
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

/// The asymmetric frontend crop is enforced by the REAL present shader before
/// bell inversion and the drop overlay.  With an odd five-row removal, three
/// raw rows above and two below become chrome; the overlay border is measured
/// against the shorter visible interval, including its trailing edge.
#[test]
fn asymmetric_crop_keeps_raw_bands_outside_invert_and_drop_overlay() {
    let Some(mut gpu) = fresh_gpu() else { return };
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
        }),
        crop,
        fw as u32,
        fh as u32,
    );
    let bg = Theme::default().bg & 0x00ff_ffff;
    let red = 0x00ff_0000;
    let x = fw / 2;

    // The three leading and two trailing raw rows are now chrome. Bell invert
    // makes this non-vacuous even though the renderer's padding is itself bg:
    // sampling those rows would produce !bg, while the crop branch emits bg.
    for y in (0..3).chain((fh - 2)..fh) {
        for px in &blit.pixels[y * fw..(y + 1) * fw] {
            assert_eq!(
                px & 0x00ff_ffff,
                bg,
                "cropped raw row {y} must remain uninverted, unwashed chrome"
            );
        }
    }

    // The production border is at least two device pixels. Pin both the new
    // visible top and bottom edges at a centre column, away from the X border.
    for y in [3, 4, fh - 4, fh - 3] {
        assert_eq!(
            blit.pixels[y * fw + x] & 0x00ff_ffff,
            red,
            "drop border must follow the cropped visible edge at row {y}"
        );
    }

    // Wash alpha is zero, so a point beyond every border remains the exact
    // bell-inverted source texel rather than being spuriously highlighted.
    let interior_y = 6;
    assert_eq!(
        blit.pixels[interior_y * fw + x] & 0x00ff_ffff,
        (source.pixels[interior_y * fw + x] ^ 0x00ff_ffff) & 0x00ff_ffff,
        "crop-local overlay must not leak into its interior when wash alpha is zero"
    );
}
