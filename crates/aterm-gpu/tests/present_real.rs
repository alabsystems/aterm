// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE PRESENT-REAL THEOREM (Headless Present-Real, PHASE 2): the same
// `RenderInput` presented through a COPYABLE SWAPCHAIN with the video tap and
// through the headless VIRTUAL target (`present_virtual`) with the tap must
// yield BYTE-IDENTICAL harvested frames. Both arms run the ONE extracted
// `present_to_view` compose-and-blit seam, so parity is by construction — this
// suite pins it empirically so any future drift between the arms (a forgotten
// pass, a format/size divergence, a tap rewire) is a red CI, not a silent lie
// in the recording artifact.
//
// The swapchain arm is `present_swapchain_standin_for_test`: the REAL
// `present_input` path after its acquire (same seam, same blit pipeline, same
// uniform bytes) against a texture carrying exactly a copyable swapchain's
// configuration (`RENDER_ATTACHMENT | COPY_SRC`, the `pick_surface_format` SDR
// default) — only `present()` itself (pure WSI, no pixel effect) is absent,
// since a headless test has no compositor.
//
// Variants cover the swapchain-only layers single-frame tools miss — the ones
// the tap exists for: the additive GLOW quads, the radial HALOS, the under-
// glyph flame body + charred cores, the comet TRAIL, the half-res gaussian
// BLOOM, the SDR glow-boost CROWN (the blit-pass One/One draw), the settings
// TRAY, the bell INVERT, and the scissored second present.
//
// Gated like the differential harness: no GPU / no system font -> no-op.

use aterm_core::terminal::Terminal;
use aterm_gpu::video_tap::{CaptureOpts, DEFAULT_BUDGET, VideoTake};
use aterm_render::{
    CharFg, FireHaloCell, FireMode, FirePatch, GlowQuad, HaloMode, RainHalo, RenderInput, Theme,
    TrailCell, band_offset, band_offset_y, place_frame_bands, premul_rgb,
};

fn gpu_or_skip(px: f32, theme: Theme) -> Option<aterm_gpu::GpuRenderer> {
    match aterm_gpu::GpuRenderer::new(px, theme) {
        Ok(mut g) => {
            // THE FLIP: present_real is the wgpu present-theorem harness (its
            // stand-in textures and taps are wgpu-typed) — the WGPU ORACLE by
            // name. The armed twin lives in the metal armed differentials
            // (`metal_present_bytes_for_test` + the tap-ring differential).
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

fn opts(half_res: bool) -> CaptureOpts {
    CaptureOpts {
        half_res,
        budget_bytes: DEFAULT_BUDGET,
        fps_cap: None,
        requested_ms: 0,
    }
}

/// Harvest-and-compare: the two takes must agree on geometry, frame count, and
/// EVERY frame's exact bytes (timestamps excluded — the clock is the one
/// honest difference between two runs).
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

/// One theorem round: present every input of `seq` through BOTH arms (fresh
/// per-window state each arm; the tap armed for the whole sequence) and demand
/// byte-identical harvests. `reset_glow_ease_for_test` runs before each arm so
/// the SDR crown's attack envelope — the one deliberately wall-clock term in
/// the pass — starts both arms from the identical eased budget.
fn round(
    label: &str,
    gpu: &mut aterm_gpu::GpuRenderer,
    seq: &[(&RenderInput, bool)],
    w: u32,
    h: u32,
    half_res: bool,
) -> (VideoTake, VideoTake) {
    // Arm A: the copyable-swapchain present with the tap.
    let mut win_a = aterm_gpu::WindowGpu::new();
    gpu.video_begin_standin_for_test(&mut win_a, w, h, opts(half_res))
        .expect("standin tap");
    gpu.reset_glow_ease_for_test(&mut win_a);
    for (i, (input, invert)) in seq.iter().enumerate() {
        gpu.present_swapchain_standin_for_test(&mut win_a, input, *invert, None, None, (w, h));
        gpu.video_after_present(&mut win_a, i as u64 + 1);
    }
    let take_a = gpu.video_finish(&mut win_a).expect("swapchain take");

    // Arm B: the headless virtual present with the tap.
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
    let take_b = gpu.video_finish(&mut win_b).expect("virtual take");

    assert_eq!(
        take_a.frames.len(),
        seq.len(),
        "{label}: every present must harvest exactly one frame"
    );
    assert_takes_byte_identical(label, &take_a, &take_b);
    (take_a, take_b)
}

/// Deck a base text frame out with the full EMBERFORGE effect stack: additive
/// glow quads, radial halos, under-glyph flame body, charred glyph cores, a
/// per-pixel fire patch + the glyph contrast-halo strength it engulfs, a
/// comet trail, and the fire block-fill override.
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
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
        });
    }
    // A radial halo (ember) centred over the glow, spanning its row band.
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
    // Under-glyph flame body + a charred core on the engulfed glyph.
    input.glow_under.push(GlowQuad {
        row: 2,
        x: (2 * cw) as u16,
        y: (2 * ch) as u16,
        w: (2 * cw) as u16,
        h: ch as u16,
        color: premul_rgb(fire, 150),
        // ADDITIVE light (see `GlowQuad::alpha`).
        alpha: 0,
    });
    input.char_fg.push(CharFg {
        row: 2,
        col: 2,
        fg: 0x0020_100A,
    });
    // A live per-pixel fire patch + a contrast-halo strength on the engulfed
    // glyph, so the theorem covers the fire-field pass AND the glyph_halo
    // deco-over pass (the halo keys on `fire_halo` + a live `fire_patch`).
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
    // The cadence-comet body and the fire block fill.
    input.cursor_trail.push(TrailCell {
        row: 1,
        col: 1,
        alpha: 180,
    });
    input.cursor_trail_color = fire;
    input.cursor_fill_override = Some(fire);
}

/// The theorem over a PLAIN TEXT frame plus a SCISSORED second present (one
/// changed row): the base compose, the letterbox blit, and the dirty-row
/// repaint path agree byte-for-byte between the arms.
#[test]
fn present_real_theorem_text_and_scissored_present() {
    let Some(mut gpu) = gpu_or_skip(16.0, Theme::default()) else {
        return;
    };
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
    // Non-vacuity: the two swapchain frames submitted through the WSI present
    // path really differ (the scissored second present repainted the changed rows).
    let (f1, f2) = (&take_a.frames[0], &take_a.frames[1]);
    assert!(
        f1.rgba != f2.rgba,
        "the scissored second present must change the captured bytes"
    );
}

/// A raw window is commonly a few pixels larger than its integer-cell frame
/// after a font zoom. Drive that exact geometry through the PRODUCTION present
/// seam and require every harvested swapchain pixel to equal the CPU placement
/// reference. The live OSC-11 background distinguishes the bands from the
/// configured theme, while the coloured bottom-right cell makes clipping the
/// trailing content observable even on an SDR stand-in whose clear is otherwise
/// the same colour as the expected band.
#[test]
fn production_present_resolves_odd_bands_and_trailing_content() {
    let theme = Theme::default();
    let Some(mut gpu) = gpu_or_skip(16.0, theme) else {
        return;
    };
    gpu.set_bloom(false);
    gpu.set_sdr_glow_boost(0.0);
    gpu.set_pad(0);

    let (rows, cols) = (6usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    // A live background that is intentionally not `Theme::default().bg` pins
    // the present bands to the frame's OSC-11 colour.
    term.process(b"\x1b]11;rgb:2222/4444/6666\x1b\\");
    term.process(b"$ production present +7px");
    // No padding + a vivid last-cell background makes the final source columns
    // and rows non-background pixels. Disable wrap/cursor so the write neither
    // scrolls nor covers the sentinel cell.
    term.process(b"\x1b[?7l\x1b[?25l\x1b[6;28H\x1b[48;2;255;0;255m \x1b[0m");
    let mut input = term.cell_frame(rows, cols);
    // `cell_frame` leaves this host-resolved presentation field unset; the GUI
    // fills it from the live terminal colours before presenting. Reproduce that
    // host seam explicitly so the band must follow OSC 11 rather than the theme.
    input.default_bg = 0x0022_4466;
    let live_bg = input.default_bg & 0x00ff_ffff;
    assert_ne!(
        live_bg,
        theme.bg & 0x00ff_ffff,
        "OSC 11 must make the live-band check non-vacuous"
    );

    // Read the exact offscreen source, then let the CPU's production twin place
    // it into a raw-window destination with an odd 3/4 remainder split.
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
    );

    // Exercise the REAL swapchain present seam, not the dedicated blit helper:
    // this includes the production viewport/scissor and video-introspection copy.
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
        .expect("production present take");
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

    // Non-vacuity for the clipping sentinel: the expected bottom-right source
    // pixel is magenta, not the live band colour.
    assert_eq!(source.pixels[fw * fh - 1] & 0x00ff_ffff, 0x00ff_00ff);

    // Tier-1 conformance: normalize this real odd-band present onto the bounded
    // SurfaceCoverage machine (frame=4, raw surface=5). Exact equality above is
    // the real-code witness for full coverage; all four raw-destination corners
    // are the live OSC-11 band witness.
    let live_rgba = [0x22, 0x44, 0x66, 0xff];
    let pixel = |x: usize, y: usize| {
        let start = (y * dw + x) * 4;
        &frame.rgba[start..start + 4]
    };
    let bands_use_live_bg = [(0, 0), (dw - 1, 0), (0, dh - 1), (dw - 1, dh - 1)]
        .into_iter()
        .all(|(x, y)| pixel(x, y) == live_rgba);
    let model = aterm_spec::derive::surface_coverage_model();
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

    // Negative control: project the deleted frame-sized viewport. It covers
    // only the logical frame and leaves the band stale, so both model
    // invariants reject the state.
    let mut old_frame_viewport = model.init_state();
    assert!(model.fire("Zoom", &mut old_frame_viewport));
    old_frame_viewport.insert("covered", 4);
    old_frame_viewport.insert("band_live", 0);
    old_frame_viewport.insert("presented", 1);
    assert!(!model.check_invariant("PresentCoversSurface", &old_frame_viewport));
    assert!(!model.check_invariant("RemainderUsesLiveBackground", &old_frame_viewport));
}

/// A one-shot exact-destination capture can run during an active video without
/// consuming, decimating, or otherwise perturbing the recorder. Both copies are
/// appended after the final presentation pass in the same encoder, so their
/// harvested bytes must be identical.
#[test]
fn one_shot_presented_snapshot_is_independent_from_video() {
    let Some(mut gpu) = gpu_or_skip(16.0, Theme::default()) else {
        return;
    };
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
        .expect("one-shot take");
    let video = gpu.video_finish(&mut win).expect("video take");

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

/// The theorem over the FULL FIRE STACK at full res with the GPU bloom ON and
/// the SDR glow-boost crown engaged — every swapchain-only pass the tap exists
/// to capture (glow, halo, under, char_fg, fire field, glyph contrast-halo,
/// trail, bloom, crown) agrees byte-for-byte between the arms.
#[test]
fn present_real_theorem_fire_glow_bloom_halo_crown() {
    let Some(mut gpu) = gpu_or_skip(16.0, Theme::default()) else {
        return;
    };
    gpu.set_bloom(true);
    // The heat shimmer stays ON (the theorem should cover the whole parity
    // class) with its one wall-clock term PINNED, so both arms compose the
    // identical refraction — the shimmer analogue of `reset_glow_ease_for_test`.
    gpu.set_shimmer_phase_for_test(Some(0.40));
    gpu.set_sdr_glow_boost(0.5);
    let (rows, cols) = (6usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ cargo test --release\r\n$ emberforge blaze\r\n$ dark cores");
    let mut input = term.cell_frame(rows, cols);
    let (cw, ch) = gpu.cell_size();
    add_fire_stack(&mut input, cw, ch);
    let (fw, fh) = gpu.frame_size(rows, cols);

    // A bare control frame first, so the fire variant provably changes pixels.
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
    // And the fire really reads WARM somewhere: a pixel visibly hotter in red
    // than blue (the ember ramp), so the parity claim covers lit pixels.
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

/// The theorem under the HALF-RES harvest (the multi-second recording
/// default): the 2x2 box downscale runs identically on both arms.
#[test]
fn present_real_theorem_half_res_harvest() {
    let Some(mut gpu) = gpu_or_skip(16.0, Theme::default()) else {
        return;
    };
    gpu.set_bloom(true);
    // Shimmer ON with a pinned phase, as in the fire-stack theorem above.
    gpu.set_shimmer_phase_for_test(Some(0.40));
    let (rows, cols) = (6usize, 28usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ half-res harvest parity");
    let mut input = term.cell_frame(rows, cols);
    let (cw, ch) = gpu.cell_size();
    add_fire_stack(&mut input, cw, ch);
    let (fw, fh) = gpu.frame_size(rows, cols);
    round(
        "half-res fire",
        &mut gpu,
        &[(&input, false)],
        fw as u32,
        fh as u32,
        true,
    );
}

/// The theorem over the BELL-INVERT chrome (a blit-uniform arm) — the last
/// present-time layer distinct from the streams above.
#[test]
fn present_real_theorem_bell_invert() {
    let Some(mut gpu) = gpu_or_skip(16.0, Theme::default()) else {
        return;
    };
    gpu.set_bloom(false);
    let (rows, cols) = (5usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ bell");
    let input = term.cell_frame(rows, cols);
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (take, _) = round(
        "bell-invert",
        &mut gpu,
        &[(&input, false), (&input, true)],
        fw as u32,
        fh as u32,
        false,
    );
    assert!(
        take.frames[0].rgba != take.frames[1].rgba,
        "the invert frame must differ from the plain frame (non-vacuous)"
    );
}
