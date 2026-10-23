// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::common;
use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, Theme};
use std::sync::Arc;

use crate::common::{backends, bb, gg, max_channel_delta_frame as max_channel_delta, rr};

/// Solid-colour `w`×`h` RGBA PNG.
fn solid_rgba_png(w: u32, h: u32, rgba_pixel: [u8; 4]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..(w * h) {
        rgba.extend_from_slice(&rgba_pixel);
    }
    let mut out = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut out, w, h);
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    out
}

/// Solid-colour `w`×`h` opaque RGBA PNG.
fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    solid_rgba_png(w, h, [rgb[0], rgb[1], rgb[2], 255])
}

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

/// The pixels of cell `(row, col)` from a frame.
fn cell_pixels(f: &Frame, cw: usize, ch: usize, row: usize, col: usize) -> Vec<u32> {
    let mut out = Vec::with_capacity(cw * ch);
    for y in row * ch..(row * ch + ch).min(f.height) {
        for x in col * cw..(col * cw + cw).min(f.width) {
            out.push(f.pixels[y * f.width + x]);
        }
    }
    out
}

#[test]
fn gpu_skips_glyph_under_image_like_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    let png = solid_png(2 * cw as u32, ch as u32, [255, 255, 0]);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"\x1b[37mWW\x1b[0m"); // bright glyphs at (0,0),(0,1)
    term.process(b"\r"); // carriage return so the image lands over them
    term.process(&osc_1337_file("inline=1;width=2;height=1", &png));

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    let white_glyph = |px: &[u32]| -> usize {
        px.iter()
            .filter(|&&p| rr(p) > 200 && gg(p) > 200 && bb(p) > 200)
            .count()
    };
    let cpu_white = white_glyph(&cell_pixels(&cpu_frame, cw, ch, 0, 0));
    let gpu_white = white_glyph(&cell_pixels(&gpu_frame, cw, ch, 0, 0));
    assert_eq!(cpu_white, 0, "CPU must not draw the glyph under the image");
    assert_eq!(gpu_white, 0, "GPU must not draw the glyph under the image");

    let mut term2 = Terminal::new(rows as u16, cols as u16);
    term2.set_cell_pixel_size(cw as u16, ch as u16);
    term2.process(b"\x1b[2;1H\x1b[37mW");
    let input2 = term2.cell_frame(rows, cols);
    let cpu2 = cpu.render_input(&input2);
    let gpu2 = gpu.render_input(&mut win, &input2, None);
    assert!(
        white_glyph(&cell_pixels(&cpu2, cw, ch, 1, 0)) > 0,
        "CPU draws an uncovered glyph"
    );
    assert!(
        white_glyph(&cell_pixels(&gpu2, cw, ch, 1, 0)) > 0,
        "GPU draws an uncovered glyph"
    );
}

#[test]
fn gpu_skips_emoji_under_image_like_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    let png = solid_png(2 * cw as u32, ch as u32, [40, 40, 40]);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process("\u{1F534}".as_bytes()); // red circle emoji
    term.process(b"\r");
    term.process(&osc_1337_file("inline=1;width=2;height=1", &png));

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);

    let red_emoji = |px: &[u32]| -> usize {
        px.iter()
            .filter(|&&p| rr(p) > 150 && gg(p) < 80 && bb(p) < 80)
            .count()
    };
    let cpu_red = red_emoji(&cell_pixels(&cpu_frame, cw, ch, 0, 0));
    let gpu_red = red_emoji(&cell_pixels(&gpu_frame, cw, ch, 0, 0));
    assert_eq!(cpu_red, 0, "CPU must not draw the emoji under the image");
    assert_eq!(gpu_red, 0, "GPU must not draw the emoji under the image");
}

#[test]
fn combining_mark_draws_over_negative_z_image_on_cpu_and_gpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    cpu.set_text_blending(aterm_render::TextBlending::Linear);
    gpu.set_text_blending(aterm_render::TextBlending::Linear);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (3usize, 6usize);
    let image_rgb = [12, 30, 90];
    let image = Arc::new(aterm_core::grid::extra::ImageData {
        bytes: solid_png(cw as u32, ch as u32, image_rgb),
        format: aterm_core::grid::extra::ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: -1,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: aterm_core::grid::extra::ImageScaling::Fit,
        source_rect: None,
    });

    let make_input = |text: &str, with_image: bool| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.set_cell_pixel_size(cw as u16, ch as u16);
        term.process(format!("\x1b[97m{text}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        if with_image {
            input.images[0].push((
                0,
                aterm_core::grid::extra::ImageRef {
                    image: image.clone(),
                    cell_row: 0,
                    cell_col: 0,
                    kitty: None,
                },
            ));
        } else {
            input.cells[0][0].bg = image_rgb;
        }
        input
    };
    let render = |input: &aterm_render::RenderInput,
                  cpu: &mut Renderer,
                  gpu: &mut aterm_gpu::GpuRenderer|
     -> (Frame, Frame) {
        if input.image_at(0, 0).is_some() {
            assert_eq!(
                input.image_at(0, 0).map(|image| image.image.z_index),
                Some(-1),
                "fixture must place a behind-text image"
            );
            assert!(
                !input.image_hides_glyph_at(0, 0),
                "negative z must leave text visible"
            );
        }
        let cpu_frame = cpu.render_input(input);
        let mut win = aterm_gpu::WindowGpu::new();
        let gpu_frame = gpu.render_input(&mut win, input, None);
        (cpu_frame, gpu_frame)
    };

    let accented_input = make_input("e\u{301}", true);
    assert_eq!(
        accented_input.combining_at(0, 0),
        Some(&['\u{301}'][..]),
        "fixture must retain the NFD acute as a combining overlay"
    );
    let bare_input = make_input("e", true);
    let accented_oracle = make_input("e\u{301}", false);
    let bare_oracle = make_input("e", false);

    let (cpu_accented, gpu_accented) = render(&accented_input, &mut cpu, &mut gpu);
    let (cpu_bare, gpu_bare) = render(&bare_input, &mut cpu, &mut gpu);
    let (cpu_accented_oracle, gpu_accented_oracle) = render(&accented_oracle, &mut cpu, &mut gpu);
    let (cpu_bare_oracle, gpu_bare_oracle) = render(&bare_oracle, &mut cpu, &mut gpu);

    assert_eq!(
        cpu_accented.pixels, cpu_accented_oracle.pixels,
        "CPU must stamp an opaque z<0 tile before the base glyph and NFD mark"
    );
    assert_eq!(
        cpu_bare.pixels, cpu_bare_oracle.pixels,
        "CPU must stamp an opaque z<0 tile before the base glyph"
    );
    let gpu_accent_oracle_delta = max_channel_delta(&gpu_accented, &gpu_accented_oracle);
    assert!(
        gpu_accent_oracle_delta <= 8,
        "GPU z<0 accented output differs from the under-image oracle by \
         {gpu_accent_oracle_delta} > 8"
    );
    let gpu_bare_oracle_delta = max_channel_delta(&gpu_bare, &gpu_bare_oracle);
    assert!(
        gpu_bare_oracle_delta <= 8,
        "GPU z<0 base output differs from the under-image oracle by \
         {gpu_bare_oracle_delta} > 8"
    );

    assert_ne!(
        cell_pixels(&cpu_accented, cw, ch, 0, 0),
        cell_pixels(&cpu_bare, cw, ch, 0, 0),
        "CPU must paint the NFD acute over an opaque z<0 image"
    );
    assert_ne!(
        cell_pixels(&gpu_accented, cw, ch, 0, 0),
        cell_pixels(&gpu_bare, cw, ch, 0, 0),
        "GPU must paint the NFD acute over an opaque z<0 image"
    );

    let delta = max_channel_delta(&cpu_accented, &gpu_accented);
    assert!(
        delta <= 8,
        "NFD combining mark over an opaque z<0 image diverges CPU/GPU by {delta} > 8"
    );
}

#[test]
fn semitransparent_negative_z_image_composites_before_text_cpu_and_gpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    cpu.set_text_blending(aterm_render::TextBlending::Linear);
    gpu.set_text_blending(aterm_render::TextBlending::Linear);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (3usize, 6usize);
    let cell_bg = [20, 45, 75];
    let glow = 0x0010_2008;
    let image_rgb = [220, 35, 130];
    let alpha = 128;
    let cell_bg_u32 =
        (u32::from(cell_bg[0]) << 16) | (u32::from(cell_bg[1]) << 8) | u32::from(cell_bg[2]);
    let lit_bg = aterm_render::add_sat(cell_bg_u32, glow);
    let lit_bg_rgb = [
        ((lit_bg >> 16) & 0xff) as u8,
        ((lit_bg >> 8) & 0xff) as u8,
        (lit_bg & 0xff) as u8,
    ];
    let blended = aterm_render::blend_rgb(
        lit_bg,
        (u32::from(image_rgb[0]) << 16) | (u32::from(image_rgb[1]) << 8) | u32::from(image_rgb[2]),
        alpha,
    );
    let blended_bg = [
        ((blended >> 16) & 0xff) as u8,
        ((blended >> 8) & 0xff) as u8,
        (blended & 0xff) as u8,
    ];
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
        cell_offset_px: (0, 0),
        scaling: aterm_core::grid::extra::ImageScaling::Fit,
        source_rect: None,
    });

    let make_input = |bg: [u8; 3], with_image: bool| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.set_cell_pixel_size(cw as u16, ch as u16);
        term.process(b"M");
        let mut input = term.cell_frame(rows, cols);
        input.cells[0][0].fg = [255, 255, 255];
        input.cells[0][0].bg = bg;
        if with_image {
            input.glow_under.push(aterm_render::GlowQuad {
                row: 0,
                x: 0,
                y: 0,
                w: cw as u16,
                h: ch as u16,
                color: glow,
                alpha: 0,
                color2: glow,
                alpha2: 0,
            });
            input.images[0].push((
                0,
                aterm_core::grid::extra::ImageRef {
                    image: image.clone(),
                    cell_row: 0,
                    cell_col: 0,
                    kitty: None,
                },
            ));
        }
        input
    };
    let image_input = make_input(cell_bg, true);
    let oracle_input = make_input(blended_bg, false);
    let text_on_lit_bg = make_input(lit_bg_rgb, false);

    let cpu_image = cpu.render_input(&image_input);
    let cpu_oracle = cpu.render_input(&oracle_input);
    let cpu_text_first = cpu.render_input(&text_on_lit_bg);
    assert_eq!(
        cpu_image.pixels, cpu_oracle.pixels,
        "CPU must alpha-composite a z<0 image before drawing text"
    );

    let cpu_wrong_order: Vec<u32> = cpu_text_first
        .pixels
        .iter()
        .map(|&p| {
            aterm_render::blend_rgb(
                p,
                (u32::from(image_rgb[0]) << 16)
                    | (u32::from(image_rgb[1]) << 8)
                    | u32::from(image_rgb[2]),
                alpha,
            )
        })
        .collect();
    assert_ne!(
        cell_pixels(&cpu_oracle, cw, ch, 0, 0),
        cell_pixels(
            &Frame {
                width: cpu_text_first.width,
                height: cpu_text_first.height,
                pixels: cpu_wrong_order,
            },
            cw,
            ch,
            0,
            0,
        ),
        "fixture must distinguish image-before-text from image-after-text"
    );

    let mut image_win = aterm_gpu::WindowGpu::new();
    let gpu_image = gpu.render_input(&mut image_win, &image_input, None);
    let mut oracle_win = aterm_gpu::WindowGpu::new();
    let gpu_oracle = gpu.render_input(&mut oracle_win, &oracle_input, None);
    let gpu_oracle_delta = max_channel_delta(&gpu_image, &gpu_oracle);
    assert!(
        gpu_oracle_delta <= 8,
        "GPU semitransparent z<0 output differs from the under-image oracle by \
         {gpu_oracle_delta} > 8"
    );
    let parity_delta = max_channel_delta(&cpu_image, &gpu_image);
    assert!(
        parity_delta <= 8,
        "semitransparent z<0 CPU/GPU output diverges by {parity_delta} > 8"
    );
}

#[test]
fn kitty_z_threshold_and_below_cell_background_tier_match_cpu_gpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (2usize, 4usize);
    let frame_default_bg = [1, 2, 3];
    let default_bg = [3, 7, 11];
    let explicit_bg = [18, 52, 86];
    let image_rgb = [210, 30, 90];

    let make_input = |z_index: i32| {
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
            cell_offset_px: (0, 0),
            scaling: aterm_core::grid::extra::ImageScaling::Fit,
            source_rect: None,
        });
        for col in 0..=1 {
            input.images[0].push((
                col,
                aterm_core::grid::extra::ImageRef {
                    image: Arc::clone(&image),
                    cell_row: 0,
                    cell_col: 0,
                    kitty: None,
                },
            ));
        }
        input
    };
    let render = |input: &aterm_render::RenderInput,
                  cpu: &mut Renderer,
                  gpu: &mut aterm_gpu::GpuRenderer|
     -> (Frame, Frame) {
        let cpu_frame = cpu.render_input(input);
        let mut win = aterm_gpu::WindowGpu::new();
        let gpu_frame = gpu.render_input(&mut win, input, None);
        (cpu_frame, gpu_frame)
    };
    let sample = |frame: &Frame, col: usize| {
        frame.pixels[(ch / 2) * frame.width + col * cw + cw / 2] & 0x00ff_ffff
    };
    let assert_near = |actual: u32, expected: u32, message: &str| {
        let delta = (rr(actual) - rr(expected))
            .abs()
            .max((gg(actual) - gg(expected)).abs())
            .max((bb(actual) - bb(expected)).abs());
        assert!(
            delta <= 8,
            "{message}: actual=#{actual:06x}, expected=#{expected:06x}, delta={delta}"
        );
    };

    let threshold = aterm_render::KITTY_IMAGE_BELOW_BG_Z_THRESHOLD;
    let (cpu_at, gpu_at) = render(&make_input(threshold), &mut cpu, &mut gpu);
    assert_eq!(
        sample(&cpu_at, 0),
        aterm_render::rgb_to_u32(image_rgb),
        "z == INT32_MIN/2 remains above a non-default cell background"
    );
    assert_near(
        sample(&gpu_at, 0),
        aterm_render::rgb_to_u32(image_rgb),
        "GPU z == INT32_MIN/2 remains above a non-default cell background",
    );
    let at_delta = max_channel_delta(&cpu_at, &gpu_at);
    assert!(
        at_delta <= 8,
        "z == INT32_MIN/2 CPU/GPU output diverges by {at_delta} > 8"
    );

    let (cpu_below, gpu_below) = render(&make_input(threshold - 1), &mut cpu, &mut gpu);
    assert_eq!(
        sample(&cpu_below, 0),
        aterm_render::rgb_to_u32(explicit_bg),
        "z < INT32_MIN/2 is hidden by a non-default cell background"
    );
    assert_eq!(
        sample(&cpu_below, 1),
        aterm_render::rgb_to_u32(image_rgb),
        "z < INT32_MIN/2 remains visible through a default-background cell"
    );
    assert_near(
        sample(&gpu_below, 0),
        aterm_render::rgb_to_u32(explicit_bg),
        "GPU deepest tier is hidden by a non-default cell background",
    );
    assert_near(
        sample(&gpu_below, 1),
        aterm_render::rgb_to_u32(image_rgb),
        "GPU deepest tier remains visible through a default-background cell",
    );
    let below_delta = max_channel_delta(&cpu_below, &gpu_below);
    assert!(
        below_delta <= 8,
        "z < INT32_MIN/2 CPU/GPU output diverges by {below_delta} > 8"
    );
}

#[test]
fn image_pixels_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    let (iw, ih) = (3u32 * cw as u32, 2u32 * ch as u32);
    let mut rgba = Vec::with_capacity((iw * ih * 4) as usize);
    for _y in 0..ih {
        for x in 0..iw {
            let a = if x >= 2 * cw as u32 { 0 } else { 255 };
            rgba.extend_from_slice(&[200, 30, 180, a]);
        }
    }
    let mut png = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut png, iw, ih);
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"\x1b[42m"); // green background
    term.process(&osc_1337_file("inline=1;width=3;height=2", &png));

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(delta <= 8, "image-pixel CPU/GPU diverge by {delta} > 8");

    let magenta = |f: &Frame, row: usize, col: usize| -> usize {
        cell_pixels(f, cw, ch, row, col)
            .iter()
            .filter(|&&p| rr(p) > 120 && gg(p) < 90 && bb(p) > 120)
            .count()
    };
    assert!(
        magenta(&gpu_frame, 0, 0) > 0,
        "GPU must paint the opaque image pixels"
    );
    assert!(
        magenta(&cpu_frame, 0, 0) > 0,
        "CPU must paint the opaque image pixels"
    );
}

#[test]
fn sixel_rawrgba8_pixels_gpu_match_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let (mut cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    let mut dcs: Vec<u8> = Vec::new();
    dcs.extend_from_slice(format!("\x1bP0;0;8q\"1;1;{};6#1;2;100;0;0#1", 2 * cw).as_bytes());
    dcs.extend(std::iter::repeat_n(b'~', cw)); // opaque red columns (all 6 rows)
    dcs.extend(std::iter::repeat_n(b'?', cw)); // empty (transparent) columns
    dcs.extend_from_slice(b"$-\x1b\\"); // graphics CR + NL, then ST

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"\x1b[44m"); // blue cell background (shows through the transparent cell)
    term.process(&dcs);

    assert!(
        !term.images_row(0).is_empty(),
        "sixel DCS must place a RawRgba8 inline image on row 0"
    );

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(delta <= 8, "sixel RawRgba8 CPU/GPU diverge by {delta} > 8");

    let red = |f: &Frame, row: usize, col: usize| -> usize {
        cell_pixels(f, cw, ch, row, col)
            .iter()
            .filter(|&&p| rr(p) > 150 && gg(p) < 90 && bb(p) < 90)
            .count()
    };
    assert!(
        red(&gpu_frame, 0, 0) > 0,
        "GPU must paint the opaque sixel red pixels"
    );
    assert!(
        red(&cpu_frame, 0, 0) > 0,
        "CPU must paint the opaque sixel red pixels"
    );
}

#[test]
fn image_scissored_present_byte_identical_to_full() {
    let theme = aterm_render::Theme::default();
    let px = 18.0;
    let (cpu, mut gpu) = backends(px, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 16usize);
    let png = solid_png(2 * cw as u32, 2 * ch as u32, [220, 60, 160]);

    let fresh = |input: &aterm_render::RenderInput| -> Vec<u32> {
        let mut g = common::independent_gpu(px, theme);
        let mut w = aterm_gpu::WindowGpu::new();
        g.render_input(&mut w, input, None).pixels
    };

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(&osc_1337_file("inline=1;width=2;height=2", &png));
    term.process(b"\x1b[4;1Hhi"); // text on row 3, clear of the image
    let mut win = aterm_gpu::WindowGpu::new();
    let input1 = term.cell_frame(rows, cols);
    let f1 = gpu.present_input_readback(&mut win, &input1);
    assert_eq!(
        f1.pixels,
        fresh(&input1),
        "image present frame 1 must match a full render"
    );

    term.process(b"\x1b[4;3HX");
    let input2 = term.cell_frame(rows, cols);
    let before = gpu.scissor_taken();
    let f2 = gpu.present_input_readback(&mut win, &input2);
    assert!(
        gpu.scissor_taken() > before,
        "a single-cell change must take the scissor path"
    );
    assert_eq!(
        f2.pixels,
        fresh(&input2),
        "scissored image frame must match a full render"
    );

    term.process(b"\x1b[H\x1b[2Jdone");
    let input3 = term.cell_frame(rows, cols);
    let f3 = gpu.present_input_readback(&mut win, &input3);
    assert_eq!(
        f3.pixels,
        fresh(&input3),
        "image-removed frame must match a full render"
    );
}

#[test]
fn stretched_image_fills_its_footprint_on_cpu_and_gpu() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (3usize, 8usize);
    let png = solid_png(ch as u32, ch as u32, [220, 20, 20]);
    for (args, fills) in [
        ("inline=1;width=4;height=1;preserveAspectRatio=0", true),
        ("inline=1;width=4;height=1", false),
    ] {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.set_cell_pixel_size(cw as u16, ch as u16);
        term.process(&osc_1337_file(args, &png));
        let input = term.cell_frame(rows, cols);
        let cpu_frame = cpu.render_input(&input);
        let mut win = aterm_gpu::WindowGpu::new();
        let gpu_frame = gpu.render_input(&mut win, &input, None);
        let delta = max_channel_delta(&cpu_frame, &gpu_frame);
        assert!(delta <= 8, "{args}: CPU/GPU diverge by {delta} > 8");
        let edge_red = |f: &Frame| {
            (0..ch)
                .filter(|&y| {
                    let p = f.pixels[y * f.width + 4 * cw - 1];
                    rr(p) > 150 && gg(p) < 80 && bb(p) < 80
                })
                .count()
        };
        for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
            if fills {
                assert_eq!(
                    edge_red(frame),
                    ch,
                    "{name}: the stretch reaches the box edge"
                );
            } else {
                assert_eq!(
                    edge_red(frame),
                    0,
                    "{name}: control: the fit leaves the edge bare"
                );
            }
        }
    }
}

#[test]
fn kitty_cropped_put_matches_cpu_and_gpu() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let mut raw = Vec::with_capacity(2 * cw * ch * 4);
    for _y in 0..ch {
        for x in 0..2 * cw {
            raw.extend_from_slice(if x < cw {
                &[230, 20, 20, 255]
            } else {
                &[20, 20, 230, 255]
            });
        }
    }
    let b64 = aterm_codec::base64::encode(&raw).expect("encode");
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(format!("\x1b_Ga=t,f=32,s={},v={ch},i=1,q=2;{b64}\x1b\\", 2 * cw).as_bytes());
    term.process(format!("\x1b[1;1H\x1b_Ga=p,i=1,x={cw},y=0,w={cw},h={ch},q=2\x1b\\").as_bytes());
    term.process(b"\x1b[3;1H\x1b_Ga=p,i=1,q=2\x1b\\");
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(delta <= 8, "cropped put: CPU/GPU diverge by {delta} > 8");

    let count = |f: &Frame, row: usize, red: bool| {
        cell_pixels(f, cw, ch, row, 0)
            .iter()
            .filter(|&&p| {
                if red {
                    rr(p) > 150 && bb(p) < 80
                } else {
                    bb(p) > 150 && rr(p) < 80
                }
            })
            .count()
    };
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        assert_eq!(
            count(frame, 0, true),
            0,
            "{name}: the crop dropped the red half"
        );
        assert!(
            count(frame, 0, false) > 0,
            "{name}: the crop shows the blue half"
        );
        assert!(
            count(frame, 2, true) > 0,
            "{name}: control: the uncropped put starts red"
        );
    }
}

/// Transmit a one-cell Kitty image of one straight-alpha RGBA colour as image `id`.
fn kitty_solid_cell(term: &mut Terminal, id: u32, cw: usize, ch: usize, rgba: [u8; 4]) {
    let raw: Vec<u8> = rgba.iter().copied().cycle().take(cw * ch * 4).collect();
    let b64 = aterm_codec::base64::encode(&raw).expect("encode");
    term.process(format!("\x1b_Ga=t,f=32,s={cw},v={ch},i={id},q=2;{b64}\x1b\\").as_bytes());
}

#[test]
fn overlapping_kitty_images_stack_by_z_on_cpu_and_gpu() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    kitty_solid_cell(&mut term, 1, cw, ch, [230, 20, 20, 255]);
    kitty_solid_cell(&mut term, 2, cw, ch, [20, 230, 20, 128]);
    let put = |term: &mut Terminal, row: usize, id: u32, z: i32| {
        term.process(format!("\x1b[{};1H\x1b_Ga=p,i={id},z={z},C=1,q=2\x1b\\", row + 1).as_bytes());
    };
    put(&mut term, 0, 1, 0);
    put(&mut term, 0, 2, 1);
    put(&mut term, 1, 2, 1);
    put(&mut term, 1, 1, 2);
    put(&mut term, 2, 2, 1);
    put(&mut term, 2, 1, 0);
    term.process(b"\x1b[?25l"); // no cursor over the cells compared
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(delta <= 8, "stacked images: CPU/GPU diverge by {delta} > 8");

    let centre = |f: &Frame, row: usize| f.pixels[(row * ch + ch / 2) * f.width + cw / 2];
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        let (over, reversed, red) = (centre(frame, 0), centre(frame, 2), centre(frame, 1));
        assert!(
            rr(over) > 120 && gg(over) > 120 && bb(over) < 60,
            "{name}: green (z=1) blends over red (z=0): {over:06x}"
        );
        assert_eq!(
            over, reversed,
            "{name}: placed the other way round, z still decides"
        );
        assert!(
            rr(red) > 200 && gg(red) < 60,
            "{name}: opaque red (z=2) covers the green: {red:06x}"
        );
    }
}

#[test]
fn kitty_put_offset_matches_cpu_and_gpu() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    kitty_solid_cell(&mut term, 1, cw, ch, [230, 20, 20, 255]);
    let (ox, oy) = (cw / 2, ch / 2);
    term.process(format!("\x1b[1;1H\x1b_Ga=p,i=1,X={ox},Y={oy},q=2\x1b\\\x1b[?25l").as_bytes());
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(delta <= 8, "offset put: CPU/GPU diverge by {delta} > 8");

    let at = |f: &Frame, x: usize, y: usize| f.pixels[y * f.width + x];
    let red = |p: u32| rr(p) > 200 && gg(p) < 60 && bb(p) < 60;
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        assert!(!red(at(frame, 0, 0)), "{name}: the corner stays background");
        assert!(!red(at(frame, ox - 1, oy)), "{name}: left of the offset");
        assert!(!red(at(frame, ox, oy - 1)), "{name}: above the offset");
        assert!(
            red(at(frame, ox, oy)),
            "{name}: the image starts at the offset"
        );
        assert!(
            red(at(frame, cw + ox - 1, ch + oy - 1)),
            "{name}: and runs one cell on, into the next cell and row"
        );
        assert!(!red(at(frame, cw + ox, oy)), "{name}: and no further");
    }
}

#[test]
fn placement_relative_to_a_virtual_one_matches_cpu_and_gpu() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    kitty_solid_cell(&mut term, 1, cw, ch, [20, 20, 230, 255]);
    kitty_solid_cell(&mut term, 2, cw, ch, [230, 20, 20, 255]);
    term.process(b"\x1b_Ga=p,U=1,i=1,p=7,q=2\x1b\\");
    term.process("\x1b[2;3H\x1b[38;5;1m\x1b[58;5;7m\u{10EEEE}\u{0305}\u{0305}\x1b[0m".as_bytes());
    term.process(b"\x1b_Ga=p,i=2,P=1,Q=7,H=1,V=1,q=2\x1b\\\x1b[?25l");
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(
        delta <= 8,
        "floating placement: CPU/GPU diverge by {delta} > 8"
    );

    let centre = |f: &Frame, row: usize, col: usize| {
        f.pixels[(row * ch + ch / 2) * f.width + col * cw + cw / 2]
    };
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        let parent = centre(frame, 1, 2);
        assert!(
            bb(parent) > 200 && rr(parent) < 60,
            "{name}: the placeholder draws its virtual placement: {parent:06x}"
        );
        let child = centre(frame, 2, 3);
        assert!(
            rr(child) > 200 && bb(child) < 60,
            "{name}: the relative placement sits one cell down and right: {child:06x}"
        );
    }
}

#[test]
fn a_stack_taller_than_the_image_texture_shows_its_top_on_cpu_and_gpu() {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageRef, ImageScaling};
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let colour = |layer: u8| [layer * 15, 255 - layer * 15, 40, 255];
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"\x1b[?25l");
    let mut input = term.cell_frame(rows, cols);
    for layer in 1..=16u8 {
        let image = Arc::new(ImageData {
            bytes: colour(layer).to_vec(),
            format: ImageFormat::RawRgba8 {
                width: 1,
                height: 1,
            },
            cols: 1,
            rows: 200,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Stretch,
            source_rect: None,
        });
        input.images[0].push((
            0,
            ImageRef {
                image,
                cell_row: 0,
                cell_col: 0,
                kitty: None,
            },
        ));
    }
    let cpu_frame = cpu.render_input(&input);
    let mut win = aterm_gpu::WindowGpu::new();
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let top = colour(16);
    for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
        let centre = frame.pixels[(ch / 2) * frame.width + cw / 2];
        assert!(
            (rr(centre) - i32::from(top[0])).abs() <= 8
                && (gg(centre) - i32::from(top[1])).abs() <= 8,
            "{name}: the top image shows: {centre:06x}"
        );
    }
}

#[test]
fn a_kitty_frame_advance_repaints_on_retained_cpu_and_gpu_renderers() {
    let theme = Theme::default();
    let (mut cpu, mut gpu) = backends(18.0, theme);
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    let t0 = aterm_time::Instant::now();
    let at = |ms: u64| aterm_core::terminal::ClockReading {
        monotonic: t0 + std::time::Duration::from_millis(ms),
        wall_ms: None,
    };
    let frame = |rgba: [u8; 4]| {
        let raw: Vec<u8> = rgba.iter().copied().cycle().take(cw * ch * 4).collect();
        aterm_codec::base64::encode(&raw).expect("encode")
    };
    let (red, blue) = (frame([230, 20, 20, 255]), frame([20, 20, 230, 255]));
    for command in [
        format!("\x1b_Ga=t,f=32,s={cw},v={ch},i=7,q=2;{red}\x1b\\"),
        format!("\x1b_Ga=f,f=32,s={cw},v={ch},i=7,z=30,q=2;{blue}\x1b\\"),
        "\x1b_Ga=a,i=7,r=1,z=30\x1b\\".to_string(),
        "\x1b[1;1H\x1b_Ga=p,i=7,C=1,q=2\x1b\\\x1b[?25l".to_string(),
        "\x1b_Ga=a,i=7,s=3\x1b\\".to_string(),
    ] {
        term.process_at(command.as_bytes(), at(0));
    }
    let mut win = aterm_gpu::WindowGpu::new();
    let centre = |f: &Frame| f.pixels[(ch / 2) * f.width + cw / 2];
    for (step, want_red) in [(0u64, true), (30, false), (60, true), (90, false)] {
        term.advance_kitty_animations(at(step).monotonic);
        let input = term.cell_frame(rows, cols);
        let cpu_frame = cpu.render_input(&input);
        let gpu_frame = gpu.render_input(&mut win, &input, None);
        for (name, frame) in [("CPU", &cpu_frame), ("GPU", &gpu_frame)] {
            let pixel = centre(frame);
            let red = rr(pixel) > 200 && bb(pixel) < 60;
            let blue = bb(pixel) > 200 && rr(pixel) < 60;
            assert!(
                if want_red { red } else { blue },
                "{name} at {step} ms: {pixel:06x}"
            );
        }
    }
}
