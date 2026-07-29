// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Inline-image (iTerm2 OSC 1337 / Kitty) pixel and precedence parity. Both
// renderers composite the real image pixels: Kitty z<0 tiles sit after
// backgrounds/underlayers but before base+combining glyphs, while z>=0 tiles
// retain the traditional over-text slot before line decorations.
//
// Gated: no GPU or no system font -> the test no-ops.

use aterm_core::terminal::Terminal;
use aterm_render::{Frame, Renderer, Theme};
use std::sync::Arc;

fn rr(p: u32) -> i32 {
    ((p >> 16) & 0xff) as i32
}
fn gg(p: u32) -> i32 {
    ((p >> 8) & 0xff) as i32
}
fn bb(p: u32) -> i32 {
    (p & 0xff) as i32
}

fn max_channel_delta(a: &Frame, b: &Frame) -> i32 {
    let mut m = 0;
    for (&pa, &pb) in a.pixels.iter().zip(b.pixels.iter()) {
        m = m.max((rr(pa) - rr(pb)).abs());
        m = m.max((gg(pa) - gg(pb)).abs());
        m = m.max((bb(pa) - bb(pb)).abs());
    }
    m
}

/// Solid-colour `w`×`h` RGBA PNG.
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
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    // Place bright glyphs, then cover cols 0-1 of row 0 with an opaque image.
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

    // The 'W' foreground is near-white (37 = white). After the image covers the
    // cell, NEITHER path may leave white glyph pixels in cell (0,0): the CPU
    // paints the yellow image, the GPU paints the cell bg — but crucially, no
    // white glyph survives on EITHER path (image-vs-glyph precedence).
    let white_glyph = |px: &[u32]| -> usize {
        px.iter()
            .filter(|&&p| rr(p) > 200 && gg(p) > 200 && bb(p) > 200)
            .count()
    };
    let cpu_white = white_glyph(&cell_pixels(&cpu_frame, cw, ch, 0, 0));
    let gpu_white = white_glyph(&cell_pixels(&gpu_frame, cw, ch, 0, 0));
    assert_eq!(cpu_white, 0, "CPU must not draw the glyph under the image");
    assert_eq!(gpu_white, 0, "GPU must not draw the glyph under the image");

    // Sanity: an UNCOVERED bright glyph elsewhere still renders on both paths,
    // proving the suppression is specific to image cells, not global. Write a
    // glyph on row 2 (clear of the image) and confirm both paths draw it.
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
    // image-vs-EMOJI precedence: a colour emoji covered by an image must not show
    // its colour glyph on either path. The emoji would otherwise key to the
    // colour atlas; the image guard must suppress it identically on CPU and GPU.
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    // A red emoji 🔴 (2 cells wide), then an opaque grey image over those cells.
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

    // The emoji's saturated red must NOT survive under the image on either path.
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
    // A Kitty z<0 placement is explicitly BEHIND text. Use an OPAQUE tile so
    // this catches both suppression and painter-order regressions: if either
    // backend emits the tile after glyphs, the image erases the base and accent.
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    // Make the solid-image-background oracle exact: glyph coverage blends
    // directly over the framebuffer, without the corrected-alpha remap's
    // separate home-cell-bg operand.
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
    });

    let make_input = |text: &str, with_image: bool| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.set_cell_pixel_size(cw as u16, ch as u16);
        term.process(format!("\x1b[97m{text}").as_bytes());
        let mut input = term.cell_frame(rows, cols);
        if with_image {
            // Install the same reference a Kitty z=-1 placement contributes,
            // without asking the protocol handler to replace the text cell.
            input.images[0].push((
                0,
                aterm_core::grid::extra::ImageRef {
                    image: image.clone(),
                    cell_row: 0,
                    cell_col: 0,
                },
            ));
        } else {
            // Oracle for correct under-text compositing: an opaque image first
            // leaves exactly this solid cell background for the glyph pass.
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
    // Alpha makes ordering non-commutative. The correct result (image over cell
    // bg/underlayers, then glyph) must match a no-image oracle whose cell bg is
    // that exact source-over composite. A live `glow_under` deliberately forces
    // the GPU's A1/A2/A3 additive split, guarding the A3-open gate for an
    // under-image stream. The former buggy order (glyph, then image) cannot
    // match this oracle on glyph pixels.
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
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
            });
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
    let image_input = make_input(cell_bg, true);
    let oracle_input = make_input(blended_bg, false);
    // Pre-composite the underlayer into the cell bg: this is the framebuffer
    // state immediately before the old, wrong glyph-then-image order.
    let text_on_lit_bg = make_input(lit_bg_rgb, false);

    let cpu_image = cpu.render_input(&image_input);
    let cpu_oracle = cpu.render_input(&oracle_input);
    let cpu_text_first = cpu.render_input(&text_on_lit_bg);
    assert_eq!(
        cpu_image.pixels, cpu_oracle.pixels,
        "CPU must alpha-composite a z<0 image before drawing text"
    );

    // Negative control: show that this fixture distinguishes painter order.
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
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
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
        });
        for col in 0..=1 {
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
    // THE inline-image pixel-pass gate: with the GPU image pass landed, an
    // image-covered cell must paint the SAME pixels on the GPU as the CPU's
    // `blit_image_cell` composite — within the usual 8-LSB blend tolerance the
    // colour-emoji path also rides (float ALPHA_BLENDING vs integer `blend`).
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    // A 3x2 magenta image with a fully-transparent right column, so we exercise
    // BOTH the opaque straight-RGBA blit AND the straight-alpha-over-bg composite
    // (the transparent column must show the cell bg through on both paths).
    let (iw, ih) = (3u32 * cw as u32, 2u32 * ch as u32);
    let mut rgba = Vec::with_capacity((iw * ih * 4) as usize);
    for _y in 0..ih {
        for x in 0..iw {
            // Right third fully transparent; left two-thirds opaque magenta.
            let a = if x >= 2 * cw as u32 { 0 } else { 255 };
            rgba.extend_from_slice(&[200, 30, 180, a]);
        }
    }
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, iw, ih);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    // Coloured cell bg first (so the transparent image column blends over it),
    // then place the image over those cells.
    term.process(b"\x1b[42m"); // green background
    term.process(&osc_1337_file("inline=1;width=3;height=2", &png));

    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(delta <= 8, "image-pixel CPU/GPU diverge by {delta} > 8");

    // Sanity: the GPU actually drew the image (the opaque magenta is present in
    // cell (0,0), not just the bg) — otherwise a do-nothing GPU pass would also
    // pass the delta check if the CPU were blank, which it is not.
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
    // THE sixel pixel-pass gate: a DECODED sixel image — tagged
    // `ImageFormat::RawRgba8`, the format the shipped GUI now renders — must paint
    // the SAME pixels on the GPU as the CPU's `blit_image_cell` composite, within
    // the usual 8-LSB blend tolerance. This mirrors `image_pixels_gpu_match_cpu`
    // (the PNG gate) but drives a REAL sixel DCS through the Terminal so the
    // RawRgba8 decode→place→render path is what is under test, not a PNG.
    //
    // Build is sixel-enabled for aterm-gpu's TEST build (Cargo.toml dev-dep
    // re-declares aterm-core with `features = ["sixel"]`); without it the DCS would
    // be consumed as Unknown and no image would be placed, so the sanity check
    // below (GPU actually drew the sixel red) doubles as a "feature really on" gate.
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);

    // A sixel whose raster is `2*cw` px wide × 6 px tall (one sixel band): the LEFT
    // `cw` columns are full opaque red (`~` = all six band rows set), the RIGHT
    // `cw` columns are UNPAINTED (`?` = empty column) so they stay transparent.
    // At the cw×ch cell metric the footprint is 2×1 cells: the left cell is opaque
    // red, the right cell is fully transparent (cell bg shows through) — exactly
    // the opaque-blit AND straight-alpha-over composite the PNG gate exercises,
    // but via RawRgba8.
    let mut dcs: Vec<u8> = Vec::new();
    // raster attrs 1;1;Ph;Pv with Ph=2*cw, Pv=6; define color 1 = RGB% red; select it.
    dcs.extend_from_slice(format!("\x1bP0;0;8q\"1;1;{};6#1;2;100;0;0#1", 2 * cw).as_bytes());
    dcs.extend(std::iter::repeat_n(b'~', cw)); // opaque red columns (all 6 rows)
    dcs.extend(std::iter::repeat_n(b'?', cw)); // empty (transparent) columns
    dcs.extend_from_slice(b"$-\x1b\\"); // graphics CR + NL, then ST

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"\x1b[44m"); // blue cell background (shows through the transparent cell)
    term.process(&dcs);

    // The sixel must have been DECODED + placed as a RawRgba8 image (proves the
    // feature is wired and the DCS path produced the format under test).
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

    // Sanity: the GPU actually drew the opaque red sixel pixels in cell (0,0), so a
    // do-nothing GPU pass cannot pass the delta check by both renderers being blank.
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
    // No-regression gate for the scissored present path WITH images: a reused
    // renderer driven through an image frame then a single-cell change (which
    // takes the scissored dirty-row repaint) must read back BYTE-IDENTICAL to a
    // fresh FULL render of the same input. Images now mark their rows dirty
    // (`row_differs` compares the per-row image list), so the scissor band always
    // covers them — an image can never be left stale on a partial repaint.
    let theme = aterm_render::Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (6usize, 16usize);
    let png = solid_png(2 * cw as u32, 2 * ch as u32, [220, 60, 160]);

    // A fresh full-render oracle for an input (separate renderer, no prior frame).
    let fresh = |input: &aterm_render::RenderInput| -> Vec<u32> {
        let mut g = aterm_gpu::GpuRenderer::new(px, theme).expect("GPU available a moment ago");
        let mut w = aterm_gpu::WindowGpu::new();
        g.render_input(&mut w, input, None).pixels
    };

    // Frame 1: place an image (rows 0-1), some text on row 3.
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(&osc_1337_file("inline=1;width=2;height=2", &png));
    term.process(b"\x1b[4;1Hhi"); // text on row 3, clear of the image
    let mut win = aterm_gpu::WindowGpu::new();
    let input1 = term.cell_frame(rows, cols);
    // Prime the present path (first present is always a full repaint).
    let f1 = gpu.present_input_readback(&mut win, &input1);
    assert_eq!(
        f1.pixels,
        fresh(&input1),
        "image present frame 1 must match a full render"
    );

    // Frame 2: change ONE cell on the text row (image rows untouched) — this takes
    // the scissored dirty-row path; the image must survive verbatim.
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

    // Frame 3: remove the image (overwrite its rows) — the image must disappear,
    // matching a fresh render of the now-image-free frame.
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
fn image_free_frame_stays_within_cpu_gpu_tolerance() {
    // The image plumbing must be inert for image-free content: a normal text
    // frame stays within the usual antialiasing tolerance, exactly as before.
    let theme = Theme::default();
    let px = 18.0;
    let mut gpu = match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            return;
        }
    };
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    gpu.debug_block_on_lazy_fallbacks();
    let (rows, cols) = (4usize, 12usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[31mhello\x1b[0m \x1b[44mworld\x1b[0m");
    let mut win = aterm_gpu::WindowGpu::new();
    let input = term.cell_frame(rows, cols);
    let cpu_frame = cpu.render_input(&input);
    let gpu_frame = gpu.render_input(&mut win, &input, None);
    let delta = max_channel_delta(&cpu_frame, &gpu_frame);
    assert!(delta <= 8, "image-free CPU/GPU diverge by {delta} > 8");
}

/// KITTY-CORE pixel verification: a Kitty `a=T` RGBA image rasterizes to real
/// pixels through the SAME CPU inline-image compositor that iTerm2/Sixel use.
/// CPU-only (no GPU device needed); skips if no system font.
#[test]
fn kitty_rgba_image_rasterizes_to_pixels_cpu() {
    let theme = Theme::default();
    let px = 18.0;
    let Some(mut cpu) = Renderer::from_system(px, theme) else {
        eprintln!("SKIP: no system monospace font");
        return;
    };
    // Deterministic parity: block on the lazy fallback parses so neither
    // renderer compares a provisional `.notdef` frame against a real glyph.
    cpu.debug_block_on_lazy_fallbacks();
    let (cw, ch) = cpu.cell_size();
    let (rows, cols) = (4usize, 8usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.set_cell_pixel_size(cw as u16, ch as u16);
    term.process(b"\r");

    // A solid-red RGBA image exactly one cell (cw x ch px) -> 1x1 footprint.
    let mut raw = Vec::with_capacity(cw * ch * 4);
    for _ in 0..(cw * ch) {
        raw.extend_from_slice(&[255, 0, 0, 255]);
    }
    let mut seq = format!("\x1b_Ga=T,f=32,s={cw},v={ch};").into_bytes();
    seq.extend_from_slice(
        aterm_codec::base64::encode(&raw)
            .expect("encode")
            .as_bytes(),
    );
    seq.extend_from_slice(b"\x1b\\");
    term.process(&seq);

    let input = term.cell_frame(rows, cols);
    assert!(!input.images[0].is_empty(), "kitty a=T placed the image");

    let frame = cpu.render_input(&input);
    let cell = cell_pixels(&frame, cw, ch, 0, 0);
    assert!(
        cell.iter()
            .any(|&p| rr(p) > 180 && gg(p) < 80 && bb(p) < 80),
        "the Kitty RGBA image must rasterize to red pixels (got none) — \
         confirms kitty graphics actually render via the shared compositor"
    );
}
