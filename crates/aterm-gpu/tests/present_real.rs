// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_gpu::video_tap::{CaptureOpts, DEFAULT_BUDGET, VideoTake};
use aterm_render::{
    CharFg, FireHaloCell, FireMode, FirePatch, GlowQuad, HaloMode, PixelRect, RainHalo,
    RenderInput, Theme, TrailCell, band_offset, band_offset_y, frame_placement, place_frame_bands,
    premul_rgb,
};

use crate::common::wgpu_oracle as gpu;

/// The pixels of `rect` in a straight-RGBA8 surface `surface_w` pixels wide, rows packed.
fn cut_rgba8(rgba: &[u8], surface_w: usize, rect: PixelRect) -> Vec<u8> {
    (rect.y..rect.y + rect.height)
        .flat_map(|y| {
            let start = (y * surface_w + rect.x) * 4;
            rgba[start..start + rect.width * 4].iter().copied()
        })
        .collect()
}

fn opts(half_res: bool) -> CaptureOpts {
    CaptureOpts {
        half_res,
        budget_bytes: DEFAULT_BUDGET,
        fps_cap: None,
        requested_ms: 0,
    }
}

/// The two takes agree on geometry, frame count and every frame's bytes (timestamps excluded).
fn assert_takes_byte_identical(label: &str, swap: &VideoTake, virt: &VideoTake) {
    assert_eq!(
        (swap.w, swap.h, swap.device_px, swap.half_res, swap.format),
        (virt.w, virt.h, virt.device_px, virt.half_res, virt.format),
        "{label}: capture geometry/format must agree"
    );
    assert_eq!(swap.dropped, 0, "{label}: swapchain arm lost frames");
    assert_eq!(virt.dropped, 0, "{label}: virtual arm lost frames");
    assert_eq!(
        swap.frames.len(),
        virt.frames.len(),
        "{label}: frame counts must agree"
    );
    for (i, (a, b)) in swap.frames.iter().zip(virt.frames.iter()).enumerate() {
        assert_eq!((a.w, a.h), (b.w, b.h), "{label}: frame {i} dims must agree");
        assert!(
            a.rgba == b.rgba,
            "{label}: frame {i} must be BYTE-IDENTICAL between the copyable \
             swapchain and the virtual present (first diff at byte {:?})",
            a.rgba.iter().zip(b.rgba.iter()).position(|(x, y)| x != y)
        );
    }
}

/// Present every input of `seq` through both arms.
fn round(
    label: &str,
    gpu: &mut aterm_gpu::GpuRenderer,
    seq: &[(&RenderInput, bool)],
    w: u32,
    h: u32,
    half_res: bool,
) -> (VideoTake, VideoTake) {
    let mut win_a = aterm_gpu::WindowGpu::new();
    gpu.video_begin_standin_for_test(&mut win_a, w, h, opts(half_res))
        .expect("standin tap");
    gpu.reset_glow_ease_for_test(&mut win_a);
    for (i, (input, invert)) in seq.iter().enumerate() {
        gpu.present_swapchain_standin_for_test(&mut win_a, input, *invert, None, None, (w, h));
        gpu.video_after_present(&mut win_a, i as u64 + 1);
    }
    let take_a = gpu
        .video_finish(&mut win_a)
        .expect("swapchain take")
        .settle();

    let mut win_b = aterm_gpu::WindowGpu::new();
    gpu.virtual_begin(&mut win_b, w, h, opts(half_res))
        .expect("virtual tap");
    gpu.reset_glow_ease_for_test(&mut win_b);
    for (i, (input, invert)) in seq.iter().enumerate() {
        assert!(
            gpu.present_virtual(&mut win_b, input, *invert, None, None),
            "{label}: the virtual present cannot drop"
        );
        gpu.video_after_present(&mut win_b, i as u64 + 1);
    }
    let take_b = gpu.video_finish(&mut win_b).expect("virtual take").settle();

    assert_eq!(
        take_a.frames.len(),
        seq.len(),
        "{label}: every present must harvest exactly one frame"
    );
    assert_takes_byte_identical(label, &take_a, &take_b);
    (take_a, take_b)
}

/// Add the full fire effect stack to a base text frame.
fn add_fire_stack(input: &mut RenderInput, cw: usize, ch: usize) {
    let fire = 0x00FF_6A00; // warm ember orange
    for (i, a) in [60u8, 120, 190, 240].iter().enumerate() {
        input.cursor_glow_add.push(GlowQuad {
            row: 1,
            x: ((i + 2) * cw) as u16,
            y: ch as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(fire, *a),
            alpha: 0,
            color2: premul_rgb(fire, *a),
            alpha2: 0,
        });
    }
    input.glow_halo.push(RainHalo {
        row: 1,
        x: (2 * cw) as u16,
        y: ch as u16,
        w: (3 * cw) as u16,
        h: ch as u16,
        color: premul_rgb(0x00FF_B060, 200),
        cx: (3 * cw + cw / 2) as u16,
        cy: (ch + ch / 2) as u16,
        rx: (2 * cw).max(1) as u16,
        ry: ch.max(1) as u16,
        mode: HaloMode::Add,
    });
    input.glow_under.push(GlowQuad {
        row: 2,
        x: (2 * cw) as u16,
        y: (2 * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: premul_rgb(fire, 150),
        alpha: 0,
        color2: premul_rgb(fire, 150),
        alpha2: 0,
    });
    input.char_fg.push(CharFg {
        row: 2,
        col: 2,
        fg: 0x0020_100A,
    });
    input.fire_patch.push(FirePatch {
        row: 2,
        x: (2 * cw) as u16,
        y: (2 * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        base_y: (3 * ch) as u16,
        peak_h: (2 * ch) as u16,
        phase: 4096,
        temp: 220,
        strength: 255,
        lean: 0,
        cov_cap: 200,
        cell_h: ch as u16,
        mode: FireMode::Add,
    });
    input.fire_halo.push(FireHaloCell {
        row: 2,
        col: 2,
        strength: 220,
    });
    input.cursor_trail.push(TrailCell {
        row: 1,
        col: 1,
        alpha: 180,
    });
    input.cursor_trail_color = fire;
    input.cursor_fill_override = Some(fire);
}

#[test]
fn present_real_theorem_text_and_scissored_present() {
    let mut gpu = gpu(16.0, Theme::default());
    gpu.set_bloom(false);
    let (rows, cols) = (6usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ the present-real theorem\r\n$ frame one");
    let input1 = term.cell_frame(rows, cols);
    term.process(b"\r\n$ frame two (one row changed)");
    let input2 = term.cell_frame(rows, cols);
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (take_a, _) = round(
        "text+scissor",
        &mut gpu,
        &[(&input1, false), (&input2, false)],
        fw as u32,
        fh as u32,
        false,
    );
    let (f1, f2) = (&take_a.frames[0], &take_a.frames[1]);
    assert!(
        f1.rgba != f2.rgba,
        "the scissored second present must change the captured bytes"
    );
}

#[test]
fn production_present_resolves_odd_bands_and_trailing_content() {
    let theme = Theme::default();
    let mut gpu = gpu(16.0, theme);
    gpu.set_bloom(false);
    gpu.set_sdr_glow_boost(0.0);
    gpu.set_pad(0);

    let (rows, cols) = (6usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b]11;rgb:2222/4444/6666\x1b\\");
    term.process(b"$ production present +7px");
    term.process(b"\x1b[?7l\x1b[?25l\x1b[6;28H\x1b[48;2;255;0;255m \x1b[0m");
    let mut input = term.cell_frame(rows, cols);
    input.default_bg = 0x0022_4466;
    let live_bg = input.default_bg & 0x00ff_ffff;
    assert_ne!(
        live_bg,
        theme.bg & 0x00ff_ffff,
        "OSC 11 must make the live-band check non-vacuous"
    );

    let mut source_win = aterm_gpu::WindowGpu::new();
    let source = gpu.render_input(&mut source_win, &input, None);
    let (fw, fh) = (source.width, source.height);
    let (dw, dh) = (fw + 7, fh + 7);
    assert_eq!(
        (band_offset(dw, fw), band_offset_y(dh, fh)),
        (3, if cfg!(target_os = "linux") { 0 } else { 3 }),
        "x: a +7 destination splits 3 leading / 4 trailing; y: platform policy"
    );
    let mut expected = vec![0u32; dw * dh];
    place_frame_bands(
        &mut expected,
        dw,
        dh,
        &source.pixels,
        fw,
        fh,
        false,
        live_bg,
        0..0,
    );

    let mut present_win = aterm_gpu::WindowGpu::new();
    gpu.video_begin_standin_for_test(&mut present_win, dw as u32, dh as u32, opts(false))
        .expect("standin tap");
    gpu.present_swapchain_standin_for_test(
        &mut present_win,
        &input,
        false,
        None,
        None,
        (dw as u32, dh as u32),
    );
    gpu.video_after_present(&mut present_win, 1);
    let take = gpu
        .video_finish(&mut present_win)
        .expect("production present take")
        .settle();
    assert_eq!(take.frames.len(), 1, "one present must yield one frame");
    let frame = &take.frames[0];
    assert_eq!((frame.w, frame.h), (dw as u32, dh as u32));

    let expected_rgba: Vec<u8> = expected
        .iter()
        .flat_map(|p| {
            [
                ((p >> 16) & 0xff) as u8,
                ((p >> 8) & 0xff) as u8,
                (p & 0xff) as u8,
                0xff,
            ]
        })
        .collect();
    let full_surface_matches = frame.rgba == expected_rgba;
    assert!(
        full_surface_matches,
        "production present must match CPU band placement at +7px (first diff byte {:?}; \
         got first {:?}, expected first {:?})",
        frame
            .rgba
            .iter()
            .zip(expected_rgba.iter())
            .position(|(got, want)| got != want),
        &frame.rgba[..16],
        &expected_rgba[..16]
    );

    assert_eq!(source.pixels[fw * fh - 1] & 0x00ff_ffff, 0x00ff_00ff);

    let place = frame_placement(dw, dh, fw, fh);
    let cut = |place: aterm_render::FramePlacement| {
        let rect = place
            .visible_rect(dw, dh)
            .expect("the destination is the placement's surface");
        cut_rgba8(&frame.rgba, dw, rect)
    };
    assert!(
        cut(place) == source.rgba_bytes(),
        "the destination cut at the frame's placement is the frame"
    );
    let mut shifted = place;
    shifted.x.band_before += 1;
    shifted.x.band_after -= 1;
    assert!(
        cut(shifted) != source.rgba_bytes(),
        "a cut one pixel off the placement is not the frame"
    );

    let live_rgba = [0x22, 0x44, 0x66, 0xff];
    let pixel = |x: usize, y: usize| {
        let start = (y * dw + x) * 4;
        &frame.rgba[start..start + 4]
    };
    let bands_use_live_bg = [(0, 0), (dw - 1, 0), (0, dh - 1), (dw - 1, dh - 1)]
        .into_iter()
        .all(|(x, y)| pixel(x, y) == live_rgba);
    let model = aterm_models_core::surface_coverage_model();
    let mut model_state = model.init_state();
    assert!(model.fire("Zoom", &mut model_state));
    assert!(model.fire("Present", &mut model_state));
    let real_projection = std::collections::BTreeMap::from([
        ("frame", 4),
        ("covered", if full_surface_matches { 5 } else { 4 }),
        ("band_live", i64::from(u8::from(bands_use_live_bg))),
        ("presented", 1),
    ]);
    assert_eq!(
        real_projection, model_state,
        "shipping GPU pixels must implement the ty-proven full-surface transition"
    );

    let mut old_frame_viewport = model.init_state();
    assert!(model.fire("Zoom", &mut old_frame_viewport));
    old_frame_viewport.insert("covered", 4);
    old_frame_viewport.insert("band_live", 0);
    old_frame_viewport.insert("presented", 1);
    assert!(!model.check_invariant("PresentCoversSurface", &old_frame_viewport));
    assert!(!model.check_invariant("RemainderUsesLiveBackground", &old_frame_viewport));
}

#[test]
fn overflowing_frame_crop_is_the_visible_intersection() {
    let theme = Theme::default();
    let mut gpu = gpu(16.0, theme);
    gpu.set_bloom(false);
    gpu.set_sdr_glow_boost(0.0);
    gpu.set_pad(0);

    let (rows, cols) = (6usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b]11;rgb:2222/4444/6666\x1b\\");
    term.process(b"$ overflow crop |||| ####\r\n$ every column differs: abcdefghij");
    let mut input = term.cell_frame(rows, cols);
    input.default_bg = 0x0022_4466;

    let mut source_win = aterm_gpu::WindowGpu::new();
    let source = gpu.render_input(&mut source_win, &input, None);
    let (fw, fh) = (source.width, source.height);
    let (dw, dh) = (fw - 9, fh - 5);
    let place = frame_placement(dw, dh, fw, fh);
    assert_eq!((place.x.visible, place.y.visible), (dw, dh));
    assert_eq!(
        (place.x.crop_before, place.x.crop_after),
        (5, 4),
        "x: a -9 destination crops 5 leading / 4 trailing"
    );
    assert_eq!(
        place.y.crop_before,
        if cfg!(target_os = "linux") { 0 } else { 3 },
        "y: platform policy"
    );

    let mut present_win = aterm_gpu::WindowGpu::new();
    gpu.video_begin_standin_for_test(&mut present_win, dw as u32, dh as u32, opts(false))
        .expect("standin tap");
    gpu.present_swapchain_standin_for_test(
        &mut present_win,
        &input,
        false,
        None,
        None,
        (dw as u32, dh as u32),
    );
    gpu.video_after_present(&mut present_win, 1);
    let take = gpu
        .video_finish(&mut present_win)
        .expect("production present take")
        .settle();
    let frame = &take.frames[0];
    assert_eq!((frame.w, frame.h), (dw as u32, dh as u32));

    let source_rgba = source.rgba_bytes();
    let sub_rect = |x0: usize, y0: usize| -> Vec<u8> {
        (y0..y0 + dh)
            .flat_map(|y| {
                let start = (y * fw + x0) * 4;
                source_rgba[start..start + dw * 4].iter().copied()
            })
            .collect()
    };
    let rect = place
        .visible_rect(dw, dh)
        .expect("the destination is the placement's surface");
    let cut = cut_rgba8(&frame.rgba, dw, rect);
    assert!(
        cut == sub_rect(place.x.crop_before, place.y.crop_before),
        "the cut is the frame's visible intersection (first diff byte {:?})",
        cut.iter()
            .zip(sub_rect(place.x.crop_before, place.y.crop_before).iter())
            .position(|(got, want)| got != want)
    );
    assert!(
        cut != sub_rect(place.x.crop_before + 1, place.y.crop_before),
        "the sub-rect one pixel further in is not what the glass shows"
    );
}

#[test]
fn one_shot_presented_snapshot_is_independent_from_video() {
    let mut gpu = gpu(16.0, Theme::default());
    gpu.set_bloom(true);
    gpu.set_sdr_glow_boost(0.25);
    let (rows, cols) = (5usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ exact destination snapshot");
    let mut input = term.cell_frame(rows, cols);
    let (cw, ch) = gpu.cell_size();
    add_fire_stack(&mut input, cw, ch);
    let (fw, fh) = gpu.frame_size(rows, cols);
    let destination = (fw as u32 + 5, fh as u32 + 3);
    let mut win = aterm_gpu::WindowGpu::new();

    gpu.video_begin_standin_for_test(&mut win, destination.0, destination.1, opts(false))
        .expect("video tap");
    gpu.presented_snapshot_begin_standin_for_test(&mut win, destination.0, destination.1)
        .expect("one-shot tap");
    gpu.present_swapchain_standin_for_test(&mut win, &input, false, None, None, destination);
    gpu.video_after_present(&mut win, 77);
    gpu.presented_snapshot_after_present(&mut win, 77)
        .expect("one-shot post-present");
    gpu.presented_snapshot_finish(&mut win)
        .expect("one-shot finish");
    let snapshot = gpu
        .presented_snapshot_take(&mut win)
        .and_then(aterm_gpu::video_tap::RawCapture::convert)
        .expect("one-shot take");
    let video = gpu.video_finish(&mut win).expect("video take").settle();

    assert_eq!(video.dropped, 0);
    assert_eq!(video.decimated, 0);
    assert_eq!(video.frames.len(), 1);
    assert_eq!((snapshot.w, snapshot.h), destination);
    assert_eq!(snapshot.t_us, 77);
    assert_eq!(
        snapshot.rgba, video.frames[0].rgba,
        "the independent taps must observe the same final destination"
    );
}

#[test]
fn a_virtual_snapshot_holds_the_bytes_its_window_reports() {
    let mut gpu = gpu(16.0, Theme::default());
    let (rows, cols) = (3usize, 9usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ sized");
    let input = term.cell_frame(rows, cols);
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (w, h) = (fw as u32, fh as u32);
    let mut win = aterm_gpu::WindowGpu::new();
    assert_eq!(win.virtual_snapshot_bytes(), None);

    gpu.virtual_begin(&mut win, w, h, opts(false))
        .expect("virtual tap");
    assert!(
        gpu.present_virtual(&mut win, &input, false, None, None),
        "the virtual present cannot drop"
    );
    let reported = win.virtual_snapshot_bytes();
    let raw = gpu
        .virtual_presented_snapshot_current(&win, win.resident_input_epoch(), 1)
        .expect("the virtual destination");
    assert_eq!((raw.width(), raw.height()), (w, h));
    assert_eq!(reported, Some(raw.allocation()));
    assert!(
        raw.allocation() > (w * h * 4) as usize,
        "its rows are padded past the texels"
    );
    let _ = gpu.video_finish(&mut win);
}

#[test]
fn present_real_theorem_fire_glow_bloom_halo_crown() {
    let mut gpu = gpu(16.0, Theme::default());
    gpu.set_bloom(true);
    gpu.set_shimmer_phase_for_test(Some(0.40));
    gpu.set_sdr_glow_boost(0.5);
    let (rows, cols) = (6usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ cargo test --release\r\n$ emberforge blaze\r\n$ dark cores");
    let mut input = term.cell_frame(rows, cols);
    let (cw, ch) = gpu.cell_size();
    add_fire_stack(&mut input, cw, ch);
    let (fw, fh) = gpu.frame_size(rows, cols);

    let bare = term.cell_frame(rows, cols);
    let (bare_take, _) = round(
        "fire-control",
        &mut gpu,
        &[(&bare, false)],
        fw as u32,
        fh as u32,
        false,
    );
    let (fire_take, _) = round(
        "fire+bloom+crown",
        &mut gpu,
        &[(&input, false)],
        fw as u32,
        fh as u32,
        false,
    );
    assert!(
        bare_take.frames[0].rgba != fire_take.frames[0].rgba,
        "the fire stack must actually land in the captured bytes (non-vacuous)"
    );
    let warm = fire_take.frames[0]
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 140 && p[0] > p[2].saturating_add(40))
        .count();
    assert!(
        warm > 0,
        "expected warm fire pixels in the captured frame (got none)"
    );
}
