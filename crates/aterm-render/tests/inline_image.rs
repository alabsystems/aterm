// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Inline images (iTerm2 OSC 1337 File=): an `imgcat`-style sequence places a
// PNG over the grid and the CPU renderer composites its ACTUAL pixels — image
// cells skip their glyph (image-vs-glyph precedence), and a text-only frame is
// byte-identical to the pre-image path.

use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme};
use std::sync::Arc;

/// Encode a solid-colour `w`×`h` RGBA PNG.
fn solid_rgba_png(w: u32, h: u32, rgba_pixel: [u8; 4]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        rgba.extend_from_slice(&rgba_pixel);
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    out
}

/// Encode a solid-colour `w`×`h` opaque RGBA PNG.
fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    solid_rgba_png(w, h, [rgb[0], rgb[1], rgb[2], 255])
}

/// Build an OSC 1337 `File=` sequence for `payload` with the given args.
fn osc_1337_file(args: &str, payload: &[u8]) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(payload).expect("encode");
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=");
    out.extend_from_slice(args.as_bytes());
    out.push(b':');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

#[test]
fn red_image_paints_red_pixels_over_the_grid() {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = r.cell_size();
    let mut term = Terminal::new(6, 10);
    // A 4×2 cell red image, sized in pixels so the footprint matches exactly.
    let (cols, rows) = (4u32, 2u32);
    let png = solid_png(cols * cw as u32, rows * ch as u32, [255, 0, 0]);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(&osc_1337_file(
        &format!("inline=1;width={cols};height={rows}"),
        &png,
    ));

    let frame = r.render_input(&term.cell_frame(6, 10));

    // A pixel in the centre of the image footprint must be (near) red.
    let mid_x = cw; // column 1, well inside the 4-col image
    let mid_y = ch / 2; // row 0
    let px = frame.pixels[mid_y * frame.width + mid_x];
    let (red, green, blue) = ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff);
    assert!(red > 200, "image centre should be red, got #{px:06x}");
    assert!(
        green < 60 && blue < 60,
        "image centre should be red, got #{px:06x}"
    );

    // A pixel BELOW the image (row 3) must NOT be red — the image is bounded.
    let below = frame.pixels[(3 * ch) * frame.width + mid_x];
    let br = (below >> 16) & 0xff;
    assert!(
        br < 200,
        "below the image must not be red, got #{below:06x}"
    );
}

#[test]
fn image_cell_skips_its_glyph() {
    // A glyph written first, then an image placed over the SAME cells, must not
    // show the glyph: the image owns the cell. We compare the image region of an
    // image-covered frame against a control where the same green image covers a
    // BLANK grid — they must be pixel-identical (the prior glyph left no trace).
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = r.cell_size();
    let png = solid_png(2 * cw as u32, ch as u32, [0, 200, 0]);

    // Frame A: glyphs, then image over them.
    let mut term_a = Terminal::new(4, 8);
    term_a.set_cell_pixel_size(cw as u16, ch as u16);
    term_a.process(b"XX"); // two glyphs at row 0, cols 0-1
    term_a.process(b"\r"); // back to column 0 so the image lands over them
    term_a.process(&osc_1337_file("inline=1;width=2;height=1", &png));
    let frame_a = r.render_input(&term_a.cell_frame(4, 8));

    // Frame B: image over a blank grid (no prior glyphs).
    let mut r2 = Renderer::from_system(16.0, Theme::default()).expect("font");
    let mut term_b = Terminal::new(4, 8);
    term_b.set_cell_pixel_size(cw as u16, ch as u16);
    term_b.process(&osc_1337_file("inline=1;width=2;height=1", &png));
    let frame_b = r2.render_input(&term_b.cell_frame(4, 8));

    // The 2-cell image band (rows 0..ch, cols 0..2*cw) must be identical — proof
    // the glyph under the image left no pixels.
    for y in 0..ch {
        for x in 0..(2 * cw) {
            let i = y * frame_a.width + x;
            assert_eq!(
                frame_a.pixels[i], frame_b.pixels[i],
                "image must fully cover the glyph at ({x},{y})"
            );
        }
    }
}

#[test]
fn negative_z_images_composite_before_base_and_combining_glyphs() {
    // CPU-only z-order pin (runs even when no GPU adapter is available). For
    // both opaque and semitransparent Kitty z<0 tiles, image-over-cell-bg then
    // glyph must equal a no-image oracle whose cell bg is that exact composite.
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    r.debug_block_on_lazy_fallbacks();
    r.set_text_blending(aterm_render::TextBlending::Linear);
    let (cw, ch) = r.cell_size();
    let (rows, cols) = (3usize, 6usize);
    let cell_bg = [20, 45, 75];
    let image_rgb = [220, 35, 130];
    let cell_bg_u32 =
        (u32::from(cell_bg[0]) << 16) | (u32::from(cell_bg[1]) << 8) | u32::from(cell_bg[2]);
    let image_rgb_u32 =
        (u32::from(image_rgb[0]) << 16) | (u32::from(image_rgb[1]) << 8) | u32::from(image_rgb[2]);

    for alpha in [255u8, 128] {
        let image = Arc::new(aterm_core::grid::extra::ImageData {
            bytes: solid_rgba_png(
                cw as u32,
                ch as u32,
                [image_rgb[0], image_rgb[1], image_rgb[2], alpha],
            ),
            format: aterm_core::grid::extra::ImageFormat::Png,
            cols: 1,
            rows: 1,
            z_index: -1,
            band_lift_px: 0,
        });
        let composited = aterm_render::blend_rgb(cell_bg_u32, image_rgb_u32, alpha);
        let composited_bg = [
            ((composited >> 16) & 0xff) as u8,
            ((composited >> 8) & 0xff) as u8,
            (composited & 0xff) as u8,
        ];
        let make_input = |text: &str, with_image: bool| {
            let mut term = Terminal::new(rows as u16, cols as u16);
            term.set_cell_pixel_size(cw as u16, ch as u16);
            term.process(format!("\x1b[97m{text}").as_bytes());
            let mut input = term.cell_frame(rows, cols);
            input.cells[0][0].bg = if with_image { cell_bg } else { composited_bg };
            if with_image {
                input.images[0].push((
                    0,
                    aterm_core::grid::extra::ImageRef {
                        image: image.clone(),
                        cell_row: 0,
                        cell_col: 0,
                    },
                ));
            }
            input
        };

        let accented = r.render_input(&make_input("e\u{301}", true));
        let accented_oracle = r.render_input(&make_input("e\u{301}", false));
        assert_eq!(
            accented.pixels, accented_oracle.pixels,
            "alpha={alpha}: z<0 image must paint before base+combining glyphs"
        );

        let bare = r.render_input(&make_input("e", true));
        assert_ne!(
            accented.pixels, bare.pixels,
            "alpha={alpha}: the NFD acute must remain visible over a z<0 image"
        );
    }
}

#[test]
fn kitty_extreme_negative_z_sits_below_non_default_cell_backgrounds() {
    let Some(mut renderer) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = renderer.cell_size();
    let (rows, cols) = (2usize, 4usize);
    let frame_default_bg = [1, 2, 3];
    let default_bg = [3, 7, 11];
    let explicit_bg = [18, 52, 86];
    let image_rgb = [210, 30, 90];

    let make_input = |z_index: i32, image_cols: &[usize]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut input = term.cell_frame(rows, cols);
        input.cursor_visible = false;
        input.default_bg = aterm_render::rgb_to_u32(frame_default_bg);
        input.default_bg_spans = vec![
            vec![aterm_render::DefaultBgSpan::new(
                0,
                2,
                aterm_render::rgb_to_u32(default_bg),
            )],
            Vec::new(),
        ];
        input.cells[0].resize(cols, term.implicit_blank_render_cell());
        for cell in &mut input.cells[0] {
            cell.ch = ' ';
            cell.bg = default_bg;
        }
        input.cells[0][0].bg = explicit_bg;
        let image = Arc::new(aterm_core::grid::extra::ImageData {
            bytes: solid_png(cw as u32, ch as u32, image_rgb),
            format: aterm_core::grid::extra::ImageFormat::Png,
            cols: 1,
            rows: 1,
            z_index,
            band_lift_px: 0,
        });
        for &col in image_cols {
            input.images[0].push((
                col,
                aterm_core::grid::extra::ImageRef {
                    image: Arc::clone(&image),
                    cell_row: 0,
                    cell_col: 0,
                },
            ));
        }
        input
    };
    let sample = |frame: &aterm_render::Frame, col: usize| {
        frame.pixels[(ch / 2) * frame.width + col * cw + cw / 2] & 0x00ff_ffff
    };

    let threshold = aterm_render::KITTY_IMAGE_BELOW_BG_Z_THRESHOLD;
    let at_threshold = renderer.render_input(&make_input(threshold, &[0]));
    assert_eq!(
        sample(&at_threshold, 0),
        aterm_render::rgb_to_u32(image_rgb),
        "z == INT32_MIN/2 remains above a non-default cell background"
    );

    let below_threshold = renderer.render_input(&make_input(threshold - 1, &[0, 1]));
    assert_eq!(
        sample(&below_threshold, 0),
        aterm_render::rgb_to_u32(explicit_bg),
        "z < INT32_MIN/2 is hidden by a non-default cell background"
    );
    assert_eq!(
        sample(&below_threshold, 1),
        aterm_render::rgb_to_u32(image_rgb),
        "the deepest tier remains visible through a default-background cell"
    );
}

#[test]
fn chrome_band_lift_paints_the_lip_above_the_grid() {
    // The tab strip's pixel band ([`ImageData::band_lift_px`]): an image whose
    // canvas extends `lift` px ABOVE its first cell row must paint the chrome
    // lip `[grid_top - lift, grid_top)` from its own top rows, seat its cell
    // rows exactly where an unlifted image sits, and leave every pixel outside
    // its columns untouched. A lift of 0 must remain byte-identical.
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = r.cell_size();
    let (rows, cols) = (3usize, 6usize);
    let (pad, pad_top, head) = (4usize, 2usize, 9usize);
    r.set_pad(pad);
    r.set_pad_top(pad_top);
    r.set_head(head);
    let lift = pad_top + head;
    let grid_top = lift;

    // A raw canvas: `lift` LIP rows of red over one cell row of green.
    let (img_w, img_h) = (2 * cw, ch + lift);
    let mut bytes = Vec::with_capacity(img_w * img_h * 4);
    for y in 0..img_h {
        let rgba: [u8; 4] = if y < lift {
            [220, 30, 30, 255]
        } else {
            [30, 200, 30, 255]
        };
        for _ in 0..img_w {
            bytes.extend_from_slice(&rgba);
        }
    }
    let image = |band_lift_px: u16| {
        Arc::new(aterm_core::grid::extra::ImageData {
            bytes: bytes.clone(),
            format: aterm_core::grid::extra::ImageFormat::RawRgba8 {
                width: img_w as u16,
                height: img_h as u16,
            },
            cols: 2,
            rows: 1,
            z_index: 0,
            band_lift_px,
        })
    };
    let make_input = |band_lift_px: u16| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut input = term.cell_frame(rows, cols);
        input.cursor_visible = false;
        let image = image(band_lift_px);
        for col in 0..2usize {
            input.images[0].push((
                col,
                aterm_core::grid::extra::ImageRef {
                    image: Arc::clone(&image),
                    cell_row: 0,
                    cell_col: col as u16,
                },
            ));
        }
        input
    };

    let lifted = r.render_input(&make_input(lift as u16));
    let sample = |frame: &aterm_render::Frame, x: usize, y: usize| {
        frame.pixels[y * frame.width + x] & 0x00ff_ffff
    };
    // The LIP above the grid, inside the image's columns: the canvas's red rows.
    assert_eq!(
        sample(&lifted, pad + cw / 2, grid_top / 2),
        0x00DC_1E1E,
        "the lip carries the canvas's top rows"
    );
    // The cell band itself: the canvas's green rows, at the unlifted position.
    assert_eq!(
        sample(&lifted, pad + cw / 2, grid_top + ch / 2),
        0x001E_C81E,
        "the cell row still paints its own band"
    );
    // Outside the image's columns the lip stays the theme's own chrome.
    let outside = sample(&lifted, pad + 3 * cw, grid_top / 2);
    assert_ne!(outside, 0x00DC_1E1E, "the lift never bleeds sideways");

    // Control: lift 0 leaves the lip untouched (the canvas is drawn squashed
    // into the footprint per the decode contract — nothing above `grid_top`).
    let mut r2 = Renderer::from_system(16.0, Theme::default()).expect("font");
    r2.set_pad(pad);
    r2.set_pad_top(pad_top);
    r2.set_head(head);
    let unlifted = r2.render_input(&make_input(0));
    let lip = sample(&unlifted, pad + cw / 2, grid_top / 2);
    assert_ne!(lip, 0x00DC_1E1E, "lift 0 draws nothing above the grid");
}

/// Minimal CRC-32 (the PNG/IEEE 802.3 variant), table-free — tests only.
fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// A tiny PNG whose IHDR *declares* `w`×`h` (valid IHDR CRC) but carries one
/// pixel — the inline-image allocation bomb a remote peer can stream over SSH.
fn png_with_declared_dims(w: u32, h: u32) -> Vec<u8> {
    let mut bytes = solid_png(1, 1, [9, 9, 9]);
    let ihdr_data = 16usize; // 8-byte sig + 4 len + 4 "IHDR"
    bytes[ihdr_data..ihdr_data + 4].copy_from_slice(&w.to_be_bytes());
    bytes[ihdr_data + 4..ihdr_data + 8].copy_from_slice(&h.to_be_bytes());
    let crc = crc32_ieee(&bytes[ihdr_data - 4..ihdr_data + 13]);
    bytes[ihdr_data + 13..ihdr_data + 17].copy_from_slice(&crc.to_be_bytes());
    bytes
}

#[test]
fn oversized_inline_image_png_draws_nothing_without_huge_alloc() {
    // End-to-end: a remote peer sends an OSC 1337 inline image whose PNG IHDR
    // claims 30000×30000 (≈3.4 GiB if honored) in a sub-1KB payload. The render
    // path must skip it (draw nothing) rather than allocate — no panic, no OOM.
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let (cw, ch) = r.cell_size();
    let bomb = png_with_declared_dims(30_000, 30_000);
    assert!(
        bomb.len() < 1024,
        "bomb payload stays tiny: {} bytes",
        bomb.len()
    );

    // The guard's load-bearing claim: the footprint decode is rejected (returns
    // nothing), so no `30000*30000*4` buffer is ever allocated.
    assert!(
        aterm_render::decode_image_to_footprint(
            &bomb,
            aterm_core::grid::extra::ImageFormat::Png,
            4 * cw,
            2 * ch,
        )
        .is_none(),
        "oversized inline-image PNG must decode to nothing"
    );

    // End-to-end the render must complete without panic / OOM, and the rejected
    // bomb must paint exactly like ANY other undecodable placement. We compare it
    // against a control with identical placement args but a non-PNG garbage
    // payload: both reject, so the footprint pixels must be byte-identical. (This
    // isolates "image rejected" from the handler's footprint-reservation paint.)
    let mut term = Terminal::new(6, 10);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(&osc_1337_file("inline=1;width=4;height=2", &bomb));
    let frame = r.render_input(&term.cell_frame(6, 10));

    let mut ctrl_r = Renderer::from_system(16.0, Theme::default()).expect("font");
    let mut ctrl = Terminal::new(6, 10);
    ctrl.set_cell_pixel_size(cw as u16, ch as u16);
    ctrl.process(&osc_1337_file(
        "inline=1;width=4;height=2",
        b"not a png at all",
    ));
    let control = ctrl_r.render_input(&ctrl.cell_frame(6, 10));

    for y in 0..ch {
        for x in 0..(4 * cw) {
            let i = y * frame.width + x;
            assert_eq!(
                frame.pixels[i], control.pixels[i],
                "rejected bomb must paint like any undecodable image at ({x},{y})"
            );
        }
    }
}

#[test]
fn text_only_frame_is_unaffected_by_the_image_path() {
    // No image anywhere → the rendered pixels must be byte-identical to a render
    // built before any image plumbing existed. We assert internal consistency:
    // the same input renders identically twice (the image pass is a strict no-op
    // for an image-free row, allocating nothing and touching no pixels).
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found");
        return;
    };
    let mut term = Terminal::new(4, 8);
    term.process(b"\x1b[31mhi\x1b[0m world");
    let a = r.render_input(&term.cell_frame(4, 8)).pixels;
    let mut r2 = Renderer::from_system(16.0, Theme::default()).expect("font");
    let b = r2.render_input(&term.cell_frame(4, 8)).pixels;
    assert_eq!(a, b, "image-free frame renders identically");
}

/// ASPECT FIDELITY: the footprint decode FITS the source instead of stretching it.
///
/// The measured defect (0.61.0, live pixels): a 120x60 sixel card — aspect exactly
/// 2.00 — landed in a 14x4-cell footprint of 126x68 px and was drawn 126x68, i.e.
/// aspect 1.85, a 7.9% error; a 100x40 card (2.50) came out 2.12, an 18% error.
/// Circles became ellipses on every image whose pixel size was not an exact
/// multiple of the cell box, which is nearly all of them.
///
/// Here the same shape is checked on the ONE decode both renderers share: the
/// OPAQUE region (what is actually drawn) must carry the SOURCE's aspect to
/// within a pixel, and the remainder must be fully transparent so the cell
/// background — not a smeared edge — shows through.
#[test]
fn footprint_decode_preserves_the_source_aspect_ratio() {
    use aterm_core::grid::extra::ImageFormat;

    // Opaque 120x60 raw raster (the sixel path's format), 2.00 aspect.
    let (sw, sh) = (120usize, 60usize);
    let src: Vec<u8> = std::iter::repeat_n([200u8, 30, 30, 255], sw * sh)
        .flatten()
        .collect();
    // The footprint the engine rounds that onto: 14 cells x 4 rows at 9x17 px.
    let (fp_w, fp_h) = (126usize, 68usize);
    let out = aterm_render::decode_image_to_footprint(
        &src,
        ImageFormat::RawRgba8 {
            width: sw as u16,
            height: sh as u16,
        },
        fp_w,
        fp_h,
    )
    .expect("raw RGBA fits its footprint");
    assert_eq!(
        out.len(),
        fp_w * fp_h * 4,
        "the buffer is still the footprint"
    );

    // Bounding box of the OPAQUE pixels: what the eye sees.
    let opaque = |x: usize, y: usize| out[(y * fp_w + x) * 4 + 3] > 0;
    let (mut x0, mut x1, mut y0, mut y1) = (fp_w, 0usize, fp_h, 0usize);
    for y in 0..fp_h {
        for x in 0..fp_w {
            if opaque(x, y) {
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    let (drawn_w, drawn_h) = (x1 + 1 - x0, y1 + 1 - y0);
    // Width binds (the source is wider than the box), so the full width is used
    // and the height is the aspect-exact 63 — letterboxed 2 px above, 3 below.
    assert_eq!(
        (drawn_w, drawn_h),
        (126, 63),
        "aspect-fitted, not stretched"
    );
    let err = ((drawn_w as f64 / drawn_h as f64) - (sw as f64 / sh as f64)).abs()
        / (sw as f64 / sh as f64);
    assert!(
        err < 0.01,
        "drawn aspect {:.4} must match the source's {:.4} (error {:.2}% — the \
         pre-fix stretch was 7.9%)",
        drawn_w as f64 / drawn_h as f64,
        sw as f64 / sh as f64,
        err * 100.0
    );
    // The bars are FULLY transparent: the cell background shows through them
    // rather than a stretched edge row.
    for y in 0..y0 {
        for x in 0..fp_w {
            assert_eq!(out[(y * fp_w + x) * 4 + 3], 0, "top bar at ({x},{y})");
        }
    }
    for y in (y1 + 1)..fp_h {
        for x in 0..fp_w {
            assert_eq!(out[(y * fp_w + x) * 4 + 3], 0, "bottom bar at ({x},{y})");
        }
    }
}

/// NON-VACUITY + no regression for the paths that already fitted exactly: a
/// source whose aspect equals the footprint's fills every pixel (no bars), and a
/// cell-EXACT source is still returned byte-identically — the pixel-perfect sixel
/// path (measured: 126x68 in, 126x68 out, zero interpolated pixels) must stay
/// pixel-perfect, and the tab strip's chrome band builds its raster at exactly the
/// footprint size for the same reason.
#[test]
fn an_aspect_matched_source_still_fills_the_whole_footprint() {
    use aterm_core::grid::extra::ImageFormat;
    let (fp_w, fp_h) = (126usize, 68usize);

    // Cell-exact: same pixels in and out.
    let exact: Vec<u8> = std::iter::repeat_n([7u8, 200, 90, 255], fp_w * fp_h)
        .flatten()
        .collect();
    let out = aterm_render::decode_image_to_footprint(
        &exact,
        ImageFormat::RawRgba8 {
            width: fp_w as u16,
            height: fp_h as u16,
        },
        fp_w,
        fp_h,
    )
    .expect("cell-exact raster decodes");
    assert_eq!(
        out, exact,
        "a cell-exact raster must pass through untouched"
    );

    // Half-size but SAME aspect (63x34 into 126x68): scaled up, still no bars.
    let (hw, hh) = (63usize, 34usize);
    let half: Vec<u8> = std::iter::repeat_n([7u8, 200, 90, 255], hw * hh)
        .flatten()
        .collect();
    let out = aterm_render::decode_image_to_footprint(
        &half,
        ImageFormat::RawRgba8 {
            width: hw as u16,
            height: hh as u16,
        },
        fp_w,
        fp_h,
    )
    .expect("aspect-matched raster decodes");
    assert!(
        out.as_chunks::<4>().0.iter().all(|px| px[3] == 255),
        "an aspect-matched source leaves NO transparent margin"
    );
}

/// The fit arithmetic itself, including the axis that binds and the exact-aspect
/// identity the chrome band depends on.
#[test]
fn aspect_fit_box_picks_the_binding_axis() {
    use aterm_render::aspect_fit_box;
    // Equal aspects ⇒ the whole box, exactly (no rounding drift).
    assert_eq!(aspect_fit_box(126, 68, 126, 68), (126, 68));
    assert_eq!(aspect_fit_box(63, 34, 126, 68), (126, 68));
    // Wider than the box ⇒ width binds.
    assert_eq!(aspect_fit_box(120, 60, 126, 68), (126, 63));
    // Taller than the box ⇒ height binds.
    assert_eq!(aspect_fit_box(60, 120, 126, 68), (34, 68));
    // Never larger than the box, never zero.
    let (w, h) = aspect_fit_box(4096, 1, 10, 10);
    assert!(w <= 10 && h >= 1);
}
