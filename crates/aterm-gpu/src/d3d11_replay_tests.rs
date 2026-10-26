// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D3D11 arm against wgpu's DX12 backend on the same adapter
//! (docs/AUDIT-third-party-2026-10-15.md GW.5): a renderer armed with
//! `arm_d3d11` draws every readback frame on the D3D11 arm (`encode_frame`'s
//! D3D11 tail, the effect layers, the glide and the tray card with its scrim
//! on the D3D11 offscreen, the D3D11 readback) and composes every headless
//! present there (`d3d11_present.rs`: the scissored repaint, the present
//! copy and its layers, the letterbox blit and the SDR crown into the virtual
//! target, the taps), and each frame or destination must equal an unarmed
//! renderer's wgpu-dx12 one byte for byte. Pipelines, bindings, the view
//! selection, clears, scissors, copies, sampling and the HLSL twins are the
//! arm's own; the plan and its inputs are shared.
//!
//! The two arms must render on one adapter: on the Windows lane's guest that
//! is WARP, the only adapter there. Elsewhere a test whose arms chose
//! different adapters ends COULD NOT RUN; a real GPU's frames are m6's.

use super::replay_fixtures::*;
use super::*;

/// The oracle (unarmed, wgpu-dx12) and a renderer armed with the D3D11 arm,
/// on one adapter; COULD NOT RUN when there is no device or the arms chose
/// different adapters.
fn arms() -> (GpuRenderer, GpuRenderer) {
    let renderer = |armed: bool| {
        let ctx = GpuContext::new()
            .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU context: {e}")));
        let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
            .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
        let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
            .expect("the GPU renderer assembles on an acquired device");
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        if armed {
            gpu.arm_d3d11();
            if !gpu.d3d11_arm_ready() {
                aterm_spec::host::could_not_run(format!(
                    "no D3D11 device: {}",
                    gpu.d3d11_mint_error.as_deref().unwrap_or("unknown")
                ));
            }
        } else {
            gpu.disarm_d3d11_for_oracle();
        }
        gpu
    };
    let (oracle, armed) = (renderer(false), renderer(true));
    let d3d = &armed
        .d3d11_arm
        .as_ref()
        .expect("minted above")
        .dev
        .caps()
        .adapter;
    if *d3d != oracle.ctx.adapter_name {
        aterm_spec::host::could_not_run(format!(
            "wgpu-dx12 chose {:?} and the D3D11 arm {d3d:?}; the differential needs one adapter",
            oracle.ctx.adapter_name
        ));
    }
    (oracle, armed)
}

/// The production readback, armed: every fixture (text, glyphs, the colour
/// row, decorations, glow, the image plane, the fx layers), three frames each
/// (the second and third reuse the arm's residency: streams, atlases,
/// pipelines, the offscreen), then the translucent cursor through the
/// CursorBlend row, each byte-identical to the wgpu-dx12 frame.
#[test]
fn armed_d3d11_readbacks_match_the_wgpu_arm_byte_for_byte() {
    let (mut oracle, mut armed) = arms();
    let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
    let mut fixtures = fixtures(&oracle, true);
    fixtures.push(image_fixture(&oracle));
    fixtures.push(fx_fixture(&oracle));
    let mut run = |oracle: &mut GpuRenderer,
                   armed: &mut GpuRenderer,
                   name: &str,
                   input: &RenderInput,
                   drive: &[StreamId]| {
        for pass in 0..3 {
            let expected = oracle
                .try_render_input(&mut win_o, input, None)
                .unwrap_or_else(|e| panic!("{name}: the wgpu frame: {e}"));
            assert!(!oracle.last_frame_on_d3d11(), "the oracle is unarmed");
            let actual = armed
                .try_render_input(&mut win_a, input, None)
                .unwrap_or_else(|e| panic!("{name}: the armed frame: {e}"));
            assert!(
                armed.last_frame_on_d3d11(),
                "{name}: the armed renderer fell back to wgpu (see the d3d11 arm note)"
            );
            if pass == 0 {
                let drawn: Vec<StreamId> = armed.frame_plan.iter().map(|i| i.stream).collect();
                for want in drive {
                    assert!(
                        drawn.contains(want),
                        "{name}: the fixture claims {want:?} but the armed plan never drew it"
                    );
                }
            }
            assert_frames_identical(&format!("armed {name} (frame {pass})"), &expected, &actual);
        }
        crate::stderr_line!("armed d3d11: {name}: byte-identical x3 ({drive:?})");
    };
    for (name, input, drive) in &fixtures {
        run(&mut oracle, &mut armed, name, input, drive);
    }
    oracle.set_cursor_opacity(0.5);
    armed.set_cursor_opacity(0.5);
    let input = fixtures_base_for_cursor();
    run(
        &mut oracle,
        &mut armed,
        "cursor_blend",
        &input,
        &[StreamId::CursorBlock],
    );
    assert!(
        armed
            .frame_plan
            .iter()
            .any(|i| i.stream == StreamId::CursorBlock && i.pipe == DrawPipe::CursorBlend),
        "cursor_opacity 0.5 routes the cursor fill through CursorBlend"
    );
    end_unless_the_colour_row_ran();
}

/// The negative control: one flipped instance byte on the D3D11 arm is a
/// visible difference, so the differential above cannot pass vacuously; the
/// next frame, unflipped, matches again.
#[test]
fn a_flipped_instance_byte_breaks_the_armed_d3d11_frame() {
    let (mut oracle, mut armed) = arms();
    let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
    let input = fixtures_base_for_cursor();
    let expected = oracle
        .try_render_input(&mut win_o, &input, None)
        .expect("the wgpu frame");
    armed.d3d11_arm.as_mut().expect("minted").corrupt_next = true;
    let corrupt = armed
        .try_render_input(&mut win_a, &input, None)
        .expect("the corrupted frame");
    assert!(
        armed.last_frame_on_d3d11(),
        "the corrupted frame drew on D3D11"
    );
    assert_ne!(
        expected.pixels, corrupt.pixels,
        "a flipped instance byte must change the frame, or the comparison is blind"
    );
    let clean = armed
        .try_render_input(&mut win_a, &input, None)
        .expect("the clean frame");
    assert_frames_identical("clean after the flip", &expected, &clean);
}

/// The readback's present-time layers on the armed D3D11 arm: bloom (the
/// glow extract into a half-resolution target and the screen composite),
/// heat shimmer (stage copy and refract, phase pinned) and the tray card over
/// its scrim (Windows lays one under a modal's card), each alone and then all
/// in the shared order, three frames each, byte-identical to wgpu-dx12's
/// frame. Every case is drawn before the verdict, so a failure names each
/// layer that differs. The controls: each layer changes the plain frame, so
/// the comparison covers it.
#[test]
fn armed_d3d11_effect_layers_match_the_wgpu_arm_byte_for_byte() {
    let (mut oracle, mut armed) = arms();
    for g in [&mut oracle, &mut armed] {
        g.set_shimmer_phase_for_test(Some(12.5));
    }
    let input = heat_input(&oracle);
    let card: Vec<u8> = (0..24 * 10 * 4).map(|i| (i * 37 % 256) as u8).collect();
    let scrim = TrayScrim {
        rgba: [10, 12, 30, 140],
        top: 4,
    };
    let tray = |scrim: Option<TrayScrim>| TrayQuad {
        rgba: &card,
        pw: 24,
        ph: 10,
        dx: 5,
        dy: 3,
        scrim,
    };
    // (name, bloom, shimmer, tray, scrim)
    let cases = [
        ("plain", false, false, false, false),
        ("bloom", true, false, false, false),
        ("shimmer", false, true, false, false),
        ("tray", false, false, true, false),
        ("tray over its scrim", false, false, true, true),
        ("every layer", true, true, true, true),
    ];
    let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
    let mut findings = Vec::new();
    let mut drawn = Vec::new();
    for (name, bloom, shimmer, with_tray, with_scrim) in cases {
        for g in [&mut oracle, &mut armed] {
            g.set_bloom(bloom);
            g.set_shimmer(shimmer);
        }
        if shimmer {
            assert!(
                armed.shimmer_live(&input) && oracle.shimmer_live(&input),
                "the heat scene drives the shimmer"
            );
            assert!(
                armed.shimmer_region_for_test(&input).is_some(),
                "the heat scene's haze has blank rows to rise through"
            );
        }
        let t = || with_tray.then(|| tray(with_scrim.then_some(scrim)));
        for pass in 0..3 {
            let expected = oracle
                .try_render_input(&mut win_o, &input, t())
                .expect("the wgpu frame");
            let actual = armed
                .try_render_input(&mut win_a, &input, t())
                .expect("the armed frame");
            assert!(
                armed.last_frame_on_d3d11(),
                "{name}: the readback drew on D3D11"
            );
            findings.extend(frames_differ(
                &format!("{name} (frame {pass})"),
                &expected,
                &actual,
            ));
            if pass == 0 {
                drawn.push((name, expected));
            }
        }
    }
    assert!(findings.is_empty(), "{findings:#?}");
    let plain = &drawn[0].1;
    for (name, frame) in &drawn[1..] {
        assert_ne!(
            plain.pixels, frame.pixels,
            "{name} changes the frame, so the comparison above covers it"
        );
    }
    assert_ne!(
        drawn[3].1.pixels, drawn[4].1.pixels,
        "the scrim changes the frame, so the comparison above covers it"
    );
}

/// The device-removed latch: the injected-loss drill makes the arm's next
/// frame answer as a removed device, that frame falls through to wgpu by
/// name (and still equals the oracle), and the renderer reports the device
/// lost with the drill as its cause, which is what sends the frontend to the
/// CPU renderer (`recover_from_gpu_loss`). The control: an undrilled armed
/// renderer stays on D3D11 and live.
#[test]
fn a_removed_d3d11_device_reaches_device_lost() {
    let (mut oracle, mut armed) = arms();
    let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
    let input = fixtures_base_for_cursor();
    let expected = oracle
        .try_render_input(&mut win_o, &input, None)
        .expect("the wgpu frame");
    let first = armed
        .try_render_input(&mut win_a, &input, None)
        .expect("the first armed frame");
    assert!(
        armed.last_frame_on_d3d11(),
        "the control frame drew on D3D11"
    );
    assert_frames_identical("before the drill", &expected, &first);
    assert!(!armed.device_lost(), "an undrilled device stays live");
    assert_eq!(armed.device_loss_reason(), None);

    armed
        .d3d11_arm
        .as_ref()
        .expect("minted")
        .dev
        .shared()
        .inject_device_loss();
    let after = armed
        .try_render_input(&mut win_a, &input, None)
        .expect("the frame after the loss draws on wgpu");
    assert!(
        !armed.last_frame_on_d3d11(),
        "the frame after the loss did not draw on D3D11"
    );
    assert_frames_identical("after the drill (wgpu)", &expected, &after);
    assert!(armed.device_lost(), "a removed device reaches device_lost");
    let reason = armed.device_loss_reason().unwrap_or_default();
    assert!(reason.contains("injected-loss drill"), "{reason}");
}

/// The production present path, armed: a renderer on the D3D11 arm composes
/// its headless presents (the dirty-row scissor over the resident offscreen,
/// the present copy, bloom, shimmer, the sub-row glide with its apron, the
/// card over its scrim, the letterbox blit with the bell inversion and the
/// drop overlay, the SDR crown) into its virtual target, and the destination
/// must equal an unarmed renderer's wgpu-dx12 present byte for byte: every
/// fixture presented four times (the later ones take the scissored path over
/// the resident frame), then a changed row, then a cropped, letterboxed
/// destination. Every present is drawn before the verdict, so a failure
/// names each one that differs.
#[test]
fn armed_d3d11_presents_match_the_wgpu_arm_byte_for_byte() {
    let (mut oracle, mut armed) = arms();
    for g in [&mut oracle, &mut armed] {
        g.set_bloom(true);
        g.set_shimmer(true);
        g.set_shimmer_phase_for_test(Some(7.25));
        g.set_sdr_glow_boost(1.0);
    }
    let card: Vec<u8> = (0..20 * 8 * 4).map(|i| (i * 53 % 256) as u8).collect();
    let scrim = TrayScrim {
        rgba: [10, 12, 30, 140],
        top: 5,
    };
    let tray = |scrim: Option<TrayScrim>| TrayQuad {
        rgba: &card,
        pw: 20,
        ph: 8,
        dx: 3,
        dy: 2,
        scrim,
    };
    let overlay = DropOverlay {
        accent: 0x0040_a0ff,
        wash_a: 40,
        border_a: 200,
        border_scale_q4: 16,
    };
    let mut fixtures = fixtures(&oracle, true);
    fixtures.push(image_fixture(&oracle));
    fixtures.push(fx_fixture(&oracle));
    fixtures.push(("heat", heat_input(&oracle), Vec::new()));
    // The layers the fixtures drive: the bloom halo and the SDR crown (cursor
    // glow), the shimmer (an additive fire patch), the sub-row glide (scroll).
    assert!(
        fixtures
            .iter()
            .any(|(_, i, _)| !i.cursor_glow_add.is_empty())
    );
    assert!(
        fixtures
            .iter()
            .any(|(_, i, _)| armed.shimmer_live(i) && armed.shimmer_region_for_test(i).is_some())
    );
    assert!(fixtures.iter().any(|(_, i, _)| i.scroll_frac_px != 0));
    let mut findings = Vec::new();
    let mut check = |name: String, expected: &Frame, actual: &Frame| {
        findings.extend(frames_differ(&name, expected, actual));
    };
    for (name, input, _) in &fixtures {
        let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
        for (pass, (invert, overlay, with_tray, with_scrim)) in [
            (false, None, false, false),
            (false, None, true, false),
            (false, None, true, true),
            (true, Some(overlay), false, false),
        ]
        .into_iter()
        .enumerate()
        {
            let card = || with_tray.then(|| tray(with_scrim.then_some(scrim)));
            let expected = presented(
                &mut oracle,
                &mut win_o,
                input,
                invert,
                overlay,
                card(),
                None,
            );
            assert!(!oracle.last_frame_on_d3d11(), "the oracle is unarmed");
            let actual = presented(&mut armed, &mut win_a, input, invert, overlay, card(), None);
            assert!(
                armed.last_frame_on_d3d11(),
                "{name}: the armed present fell back (see the d3d11 arm note)"
            );
            check(format!("present {name} (pass {pass})"), &expected, &actual);
        }
        // A changed row over the resident frame (the scissored repaint). A
        // host write after the fill: the row stamps are not trusted, so the
        // diff compares content.
        let mut changed = input.clone();
        changed.row_rev_lane = 0;
        if let Some(cell) = changed.cells.get_mut(1).and_then(|r| r.get_mut(2)) {
            cell.ch = 'Z';
        }
        let expected = presented(&mut oracle, &mut win_o, &changed, false, None, None, None);
        let actual = presented(&mut armed, &mut win_a, &changed, false, None, None, None);
        check(format!("present {name} (changed row)"), &expected, &actual);
        // A cropped source letterboxed into a larger destination.
        let (fw, fh) = oracle.frame_size(changed.rows, changed.cols);
        let crop = PresentCrop {
            source_y: (fh / 4) as u32,
            height: (fh / 2) as u32,
        };
        let dest = (fw as u32 + 11, (fh / 2) as u32 + 9);
        let expected = presented(
            &mut oracle,
            &mut win_o,
            &changed,
            false,
            None,
            None,
            Some((crop, dest)),
        );
        let actual = presented(
            &mut armed,
            &mut win_a,
            &changed,
            false,
            None,
            None,
            Some((crop, dest)),
        );
        check(format!("present {name} (cropped)"), &expected, &actual);
        crate::stderr_line!("armed d3d11 present: {name}: drawn x6");
    }
    assert!(findings.is_empty(), "{findings:#?}");
    end_unless_the_colour_row_ran();
}

/// The negative control for the present differential: the bell inversion, a
/// changed row and the scrim each change the armed D3D11 present, so the
/// agreement above is the composition's, not two blank destinations'.
#[test]
fn a_changed_present_changes_the_d3d11_destination() {
    let (_, mut armed) = arms();
    let mut win = WindowGpu::new();
    let input = fixtures_base_for_cursor();
    let plain = presented(&mut armed, &mut win, &input, false, None, None, None);
    assert!(armed.last_frame_on_d3d11(), "the control presents on D3D11");
    let distinct: std::collections::BTreeSet<u32> = plain.pixels.iter().copied().collect();
    assert!(
        distinct.len() > 2,
        "the plain present draws text, not a flat clear ({} colours)",
        distinct.len()
    );
    let inverted = presented(&mut armed, &mut win, &input, true, None, None, None);
    assert_ne!(
        plain.pixels, inverted.pixels,
        "the bell inversion reaches the destination"
    );
    let card = vec![200u8; 6 * 4 * 4];
    let tray = |scrim| TrayQuad {
        rgba: &card,
        pw: 6,
        ph: 4,
        dx: 2,
        dy: 1,
        scrim,
    };
    let carded = presented(
        &mut armed,
        &mut win,
        &input,
        false,
        None,
        Some(tray(None)),
        None,
    );
    let scrimmed = presented(
        &mut armed,
        &mut win,
        &input,
        false,
        None,
        Some(tray(Some(TrayScrim {
            rgba: [0, 0, 0, 160],
            top: 0,
        }))),
        None,
    );
    assert_ne!(
        carded.pixels, scrimmed.pixels,
        "the scrim reaches the destination"
    );
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';
    let other = presented(&mut armed, &mut win, &changed, false, None, None, None);
    assert_ne!(
        plain.pixels, other.pixels,
        "a changed cell reaches the destination"
    );
}

/// The device-removed latch through the presents: the injected-loss drill
/// (`ATERM_D3D11_INJECT_LOSS=<n>`) fires after the first armed present, the
/// next frame finds the device removed, that present drops, and the
/// renderer reports the device lost with the drill as its cause, which is
/// what sends the frontend to the CPU renderer (`recover_from_gpu_loss`).
/// The control: the same presents on an undrilled renderer stay on D3D11
/// and the device stays live.
#[test]
fn a_removed_d3d11_device_drops_the_present_and_reaches_device_lost() {
    let input = fixtures_base_for_cursor();
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';

    let (_, mut control) = arms();
    let mut win = WindowGpu::new();
    for i in [&input, &changed] {
        presented(&mut control, &mut win, i, false, None, None, None);
        assert!(
            control.last_frame_on_d3d11(),
            "the control presents on D3D11"
        );
    }
    assert!(!control.device_lost(), "an undrilled device stays live");
    assert_eq!(control.device_loss_reason(), None);

    let (_, mut drilled) = arms();
    drilled.d3d11_inject_loss_after_for_test(1);
    let mut win = WindowGpu::new();
    // `present_virtual`, not `presented`: the drill arms the device's next
    // frame, which the helper's snapshot readback would take.
    assert!(
        drilled.present_virtual(&mut win, &input, false, None, None),
        "the first present draws"
    );
    assert!(
        drilled.last_frame_on_d3d11(),
        "the first present draws on D3D11"
    );
    assert!(
        !drilled.device_lost(),
        "the drill fires after the present, not during it"
    );
    // A second window, so the lost frame's scope is Full: a failed D3D11
    // frame falls through to the wgpu tail, which repaints a Dirty scope only
    // over a wgpu offscreen of its own, and the first window has none.
    let mut win2 = WindowGpu::new();
    assert!(
        !drilled.present_virtual(&mut win2, &changed, false, None, None),
        "the present after the removal drops"
    );
    assert!(
        !drilled.last_frame_on_d3d11(),
        "the frame after the removal did not draw on D3D11"
    );
    assert!(
        drilled.device_lost(),
        "a removed device at a frame's first enter reaches device_lost"
    );
    let reason = drilled.device_loss_reason().unwrap_or_default();
    assert!(reason.contains("injected-loss drill"), "{reason}");
}

/// The video tap on the D3D11 arm records exactly what the arm presents: a
/// recording of three headless presents (a static frame, a glide frame, a
/// changed row) equals, frame for frame, the wgpu arm's recording of the same
/// presents, and its format names the D3D11 destination.
#[test]
fn the_d3d11_video_tap_records_what_the_wgpu_tap_records() {
    let (mut oracle, mut armed) = arms();
    let (_, scroll, _) = fixtures(&oracle, true)
        .into_iter()
        .find(|(name, ..)| *name == "scroll")
        .expect("the scroll fixture");
    let mut still = scroll.clone();
    still.scroll_frac_px = 0;
    let mut changed = still.clone();
    changed.row_rev_lane = 0;
    changed.cells[2][1].ch = '#';
    let (fw, fh) = oracle.frame_size(still.rows, still.cols);
    let opts = || crate::video_tap::CaptureOpts {
        half_res: false,
        budget_bytes: 64 * 1024 * 1024,
        fps_cap: None,
        requested_ms: 1000,
    };
    let mut takes = Vec::new();
    for gpu in [&mut oracle, &mut armed] {
        let mut win = WindowGpu::new();
        gpu.virtual_begin(&mut win, fw as u32, fh as u32, opts())
            .expect("the recording starts");
        for (t, input) in [(10, &still), (20, &scroll), (30, &changed)] {
            assert!(gpu.present_virtual(&mut win, input, false, None, None));
            gpu.video_after_present(&mut win, t);
        }
        takes.push(
            gpu.video_finish(&mut win)
                .expect("a recording was live")
                .settle(),
        );
    }
    let (wgpu_take, d3d_take) = (&takes[0], &takes[1]);
    assert!(
        armed.last_frame_on_d3d11(),
        "the armed recording drew on D3D11"
    );
    assert_eq!(
        d3d_take.format, "rgba8",
        "the D3D11 destination is RGBA8 sRGB"
    );
    assert_eq!((d3d_take.dropped, d3d_take.frames.len()), (0, 3));
    assert_eq!(wgpu_take.frames.len(), 3);
    for (e, a) in wgpu_take.frames.iter().zip(&d3d_take.frames) {
        assert_eq!((e.w, e.h, e.t_us), (a.w, a.h, a.t_us));
        assert!(
            e.rgba == a.rgba,
            "recorded frame {} differs between the arms",
            a.seq
        );
    }
    assert_ne!(
        d3d_take.frames[0].rgba, d3d_take.frames[2].rgba,
        "the recording follows the presents (a changed row is a changed frame)"
    );
}

/// A glow frame for the EDR present: white text for the reference-white scale
/// to lift above 1.0, and a row of additive aurora quads for the crown.
fn edr_input(gpu: &GpuRenderer) -> RenderInput {
    use aterm_core::terminal::Terminal;
    let (cw, ch) = gpu.cell_size();
    let mut term = Terminal::new(4, 16);
    term.process(b"edr \x1b[97mWHITE\x1b[0m crown");
    let mut input = term.cell_frame(4, 16);
    input.cursor_visible = false;
    let base = 0x0050_FA7B;
    for (i, a) in [40u8, 90, 160, 220, 255].iter().enumerate() {
        input.cursor_glow_add.push(aterm_render::GlowQuad {
            row: 2,
            x: ((i + 1) * cw) as u16,
            y: (2 * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: aterm_render::premul_rgb(base, *a),
            alpha: 0,
            color2: aterm_render::premul_rgb(base, *a),
            alpha2: 0,
        });
    }
    input
}

/// The EDR present, armed: the real present into an `Rgba16Float`
/// destination (the blit's linear decode at the scRGB reference-white scale,
/// the letterbox bands, the aurora crown above reference white under its
/// headroom) against wgpu-dx12's, plain and with the crown, at the exact fit
/// and with bands: every half-float identical. Controls: the crown changes
/// the frame and lifts a channel above 1.0, and the scale lifts the white
/// text above 1.0 without it.
#[test]
fn armed_d3d11_edr_presents_match_the_wgpu_arm() {
    let (mut oracle, mut armed) = arms();
    for gpu in [&mut oracle, &mut armed] {
        gpu.set_hdr_glow(true);
    }
    let input = edr_input(&armed);
    let window = || {
        let mut win = WindowGpu::new();
        win.set_edr_max(2.0);
        win.set_sdr_white_scale(2.5);
        win
    };
    let (mut wo, mut wa) = (window(), window());
    let mut drawn = Vec::new();
    for (boost, extra) in [(false, 0), (true, 0), (true, 7)] {
        let (want, ww, wh) =
            oracle.present_hdr_sized_for_test(&mut wo, &input, boost, extra, extra);
        let (got, gw, gh) = armed.present_hdr_sized_for_test(&mut wa, &input, boost, extra, extra);
        assert!(
            armed.last_frame_on_d3d11(),
            "the armed EDR present drew on D3D11"
        );
        assert!(!oracle.last_frame_on_d3d11(), "the oracle drew on wgpu");
        assert_eq!(
            (gw, gh),
            (ww, wh),
            "boost {boost}, bands {extra}: the destination size"
        );
        let first = want
            .iter()
            .zip(&got)
            .position(|(w, g)| w.to_bits() != g.to_bits());
        if let Some(i) = first {
            let differing = want
                .iter()
                .zip(&got)
                .filter(|(w, g)| w.to_bits() != g.to_bits())
                .count();
            panic!(
                "boost {boost}, bands {extra}: {differing} half-floats differ, the first at \
                 texel {} channel {}: wgpu {} vs D3D11 {}",
                i / 4,
                i % 4,
                want[i],
                got[i]
            );
        }
        drawn.push(got);
    }
    let over = |px: &[f32]| {
        px.iter()
            .enumerate()
            .filter(|(i, v)| i % 4 != 3 && **v > 1.0)
            .count()
    };
    let (plain, lit) = (&drawn[0], &drawn[1]);
    assert_ne!(plain, lit, "the crown changes the EDR frame");
    assert!(
        over(plain) > 0,
        "the reference-white scale lifts the white text above 1.0"
    );
    assert!(
        over(lit) > over(plain),
        "the crown emits above reference white"
    );
}
