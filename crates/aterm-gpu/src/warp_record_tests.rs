// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows renderer's frames on WARP, held to a record: the replay
//! fixtures' readbacks, the effect layers' readback and the production
//! present's destinations (`replay_fixtures`), each a SHA-256 of its RGBA
//! bytes, against `oracle/warp-frames.tsv`. The record was drawn by wgpu's
//! DX12 backend, Windows's renderer before GW.5's D3D11 flip
//! (docs/AUDIT-third-party-2026-10-15.md), in the Windows lane's guest on
//! WARP ("Microsoft Basic Render Driver"), the only adapter there, for each
//! architecture the lane runs. It is the oracle that outlives wgpu: whatever
//! arm this build presents with must draw these bytes.
//!
//! The D3D11 arm, Windows's renderer since the flip, is held to it in every
//! build: its readbacks and headless presents draw there. The oracle build
//! (`--features wgpu-oracle`) holds wgpu-dx12 itself to it as well.
//!
//! The test prints every frame's row (`warp-record` lines) whether it passes
//! or not; recording is copying those rows into the file by hand, never a
//! mode of the test. Another adapter, or another Windows build whose fonts or
//! WARP differ, is not what the record describes: a real GPU's frames are
//! m6's to measure.

use super::replay_fixtures::*;
use super::*;

/// The recorded rows: `arch`, frame, `WxH`, SHA-256 (hex).
const RECORD: &str = include_str!("../oracle/warp-frames.tsv");

/// The adapter the record was drawn on.
const WARP: &str = "Microsoft Basic Render Driver";

/// A renderer on the D3D11 arm (`armed`, on WARP too) or, in the oracle
/// build, on wgpu-dx12; COULD NOT RUN when the adapter is not WARP.
fn warp_renderer(armed: bool) -> GpuRenderer {
    let ctx = GpuContext::new()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU context: {e}")));
    if ctx.adapter_name != WARP {
        aterm_spec::host::could_not_run(format!(
            "the record is WARP's and this adapter is {:?}; a real GPU's frames are m6's",
            ctx.adapter_name
        ));
    }
    let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    if armed {
        gpu.arm_d3d11();
        assert!(
            gpu.d3d11_arm_ready(),
            "the D3D11 arm mints on WARP: {:?}",
            gpu.d3d11_mint_error
        );
        let adapter = &gpu.d3d11_arm.as_ref().expect("minted").dev.caps().adapter;
        assert_eq!(adapter, WARP, "the D3D11 arm draws on the record's adapter");
    } else {
        #[cfg(wgpu_arm)]
        gpu.disarm_d3d11_for_oracle();
        #[cfg(not(wgpu_arm))]
        unreachable!("only the oracle build draws on wgpu");
    }
    gpu
}

/// Assert the last frame drew on the arm the renderer was built for.
fn drew_on(gpu: &GpuRenderer, armed: bool, name: &str) {
    assert_eq!(
        gpu.last_frame_on_d3d11(),
        armed,
        "{name}: drawn on the wrong arm (see the d3d11 arm note)"
    );
}

/// One frame's row: its geometry and the SHA-256 of its RGBA bytes.
fn row_of(f: &Frame) -> (String, String) {
    (
        format!("{}x{}", f.width, f.height),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(f.rgba_bytes())),
    )
}

/// Frames by name, in drawing order.
type Named = Vec<(String, Frame)>;

/// Every frame the record describes, in drawing order, with the frames each
/// fixture claims to differ (`(a, b)`: the input changed between them).
fn draw_frames(armed: bool) -> (Named, Vec<(String, String)>) {
    let mut frames = Vec::new();
    let mut distinct = Vec::new();

    // Readbacks: the encode plan, three frames each (the second and third
    // reuse the residency), every stream each fixture claims drawn.
    let mut gpu = warp_renderer(armed);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut fixtures = fixtures(&gpu, true);
    fixtures.push(image_fixture(&gpu));
    fixtures.push(fx_fixture(&gpu));
    let mut win = WindowGpu::new();
    for (name, input, drive) in &fixtures {
        for pass in 0..3 {
            let f = gpu
                .try_render_input(&mut win, input, None)
                .unwrap_or_else(|e| panic!("{name}: the readback: {e}"));
            drew_on(&gpu, armed, name);
            if pass == 0 {
                let drawn: Vec<StreamId> = gpu.frame_plan.iter().map(|i| i.stream).collect();
                for want in drive {
                    assert!(
                        drawn.contains(want),
                        "{name}: the fixture claims {want:?} but the plan never drew it"
                    );
                }
            }
            frames.push((format!("{name}/readback/{pass}"), f));
        }
    }
    gpu.set_cursor_opacity(0.5);
    let cursor = fixtures_base_for_cursor();
    let f = gpu
        .try_render_input(&mut win, &cursor, None)
        .expect("the cursor-blend readback");
    drew_on(&gpu, armed, "cursor_blend");
    assert!(
        gpu.frame_plan
            .iter()
            .any(|i| i.stream == StreamId::CursorBlock && i.pipe == DrawPipe::CursorBlend),
        "cursor_opacity 0.5 routes the cursor fill through CursorBlend"
    );
    frames.push(("cursor_blend/readback/0".to_owned(), f));
    gpu.set_cursor_opacity(1.0);

    // The readback's present-time layers: bloom, the shimmer with its phase
    // pinned, and the tray card.
    gpu.set_bloom(true);
    gpu.set_shimmer(true);
    gpu.set_shimmer_phase_for_test(Some(12.5));
    let heat = heat_input(&gpu);
    assert!(
        gpu.shimmer_live(&heat) && gpu.shimmer_region_for_test(&heat).is_some(),
        "the heat scene drives the shimmer, and its haze has blank rows to rise through"
    );
    let card: Vec<u8> = (0..24 * 10 * 4).map(|i| (i * 37 % 256) as u8).collect();
    let mut win = WindowGpu::new();
    for pass in 0..3 {
        let tray = TrayQuad {
            rgba: &card,
            pw: 24,
            ph: 10,
            dx: 5,
            dy: 3,
            scrim: None,
        };
        let f = gpu
            .try_render_input(&mut win, &heat, Some(tray))
            .expect("the layered readback");
        drew_on(&gpu, armed, "heat/layered");
        frames.push((format!("heat/layered/{pass}"), f));
    }
    distinct.push(("heat/readback".to_owned(), "heat/layered/0".to_owned()));
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let f = gpu
        .try_render_input(&mut win, &heat, None)
        .expect("the plain heat readback");
    drew_on(&gpu, armed, "heat/readback");
    frames.push(("heat/readback".to_owned(), f));

    // The production present into the virtual target: the dirty-row scissor
    // over the resident offscreen, the present copy, bloom, shimmer, the
    // glide with its apron, the card, the letterbox blit with the bell
    // inversion and the drop overlay, and the SDR crown.
    let mut gpu = warp_renderer(armed);
    gpu.set_bloom(true);
    gpu.set_shimmer(true);
    gpu.set_shimmer_phase_for_test(Some(7.25));
    gpu.set_sdr_glow_boost(1.0);
    let card: Vec<u8> = (0..20 * 8 * 4).map(|i| (i * 53 % 256) as u8).collect();
    let tray = || TrayQuad {
        rgba: &card,
        pw: 20,
        ph: 8,
        dx: 3,
        dy: 2,
        scrim: None,
    };
    let overlay = DropOverlay {
        accent: 0x0040_a0ff,
        wash_a: 40,
        border_a: 200,
        border_scale_q4: 16,
    };
    let mut presents = fixtures;
    presents.push(("heat", heat_input(&gpu), Vec::new()));
    for (name, input, _) in &presents {
        let mut win = WindowGpu::new();
        for (pass, (invert, overlay, with_tray)) in [
            (false, None, false),
            (false, None, true),
            (true, Some(overlay), false),
        ]
        .into_iter()
        .enumerate()
        {
            let f = presented(
                &mut gpu,
                &mut win,
                input,
                invert,
                overlay,
                with_tray.then(tray),
                None,
            );
            drew_on(&gpu, armed, name);
            frames.push((format!("{name}/present/{pass}"), f));
        }
        let mut changed = input.clone();
        changed.row_rev_lane = 0;
        if let Some(cell) = changed.cells.get_mut(1).and_then(|r| r.get_mut(2)) {
            cell.ch = 'Z';
        }
        let f = presented(&mut gpu, &mut win, &changed, false, None, None, None);
        drew_on(&gpu, armed, name);
        frames.push((format!("{name}/present/changed"), f));
        let (fw, fh) = gpu.frame_size(changed.rows, changed.cols);
        let crop = PresentCrop {
            source_y: (fh / 4) as u32,
            height: (fh / 2) as u32,
        };
        let dest = (fw as u32 + 11, (fh / 2) as u32 + 9);
        let f = presented(
            &mut gpu,
            &mut win,
            &changed,
            false,
            None,
            None,
            Some((crop, dest)),
        );
        drew_on(&gpu, armed, name);
        frames.push((format!("{name}/present/cropped"), f));
        distinct.push((format!("{name}/present/0"), format!("{name}/present/2")));
    }
    // The changed cell lies under the image plane's opaque texels in
    // `images`; the plain text frame shows it.
    distinct.push((
        "text_cursor/present/0".to_owned(),
        "text_cursor/present/changed".to_owned(),
    ));
    (frames, distinct)
}

/// Every frame wgpu-dx12 draws on WARP equals its recorded frame byte for
/// byte (its SHA-256), in the oracle build. The negative controls ride along:
/// each present's bell inversion and the text frame's changed row must change
/// its digest, and the layers must change the heat frame, so a record of
/// blank or constant frames cannot pass. Without Segoe UI Emoji the colour
/// row is not drawn, and the test ends COULD NOT RUN after holding the rest.
#[test]
#[cfg(wgpu_arm)]
fn warp_frames_match_the_recorded_wgpu_dx12_frames() {
    hold_to_the_record(false);
}

/// The same record, with its controls, held against the D3D11 arm: its
/// readbacks (fixtures, the cursor blend, the layered heat frame) and its
/// presents' destinations must reproduce wgpu-dx12's recorded bytes.
#[test]
fn armed_d3d11_frames_match_the_recorded_wgpu_dx12_frames() {
    hold_to_the_record(true);
}

/// Draw [`draw_frames`] on the renderer `armed` names and compare every row
/// with the record.
fn hold_to_the_record(armed: bool) {
    let arch = std::env::consts::ARCH;
    let recorded: std::collections::BTreeMap<&str, (&str, &str)> = RECORD
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (a, name, geom, sha) = (f.next()?, f.next()?, f.next()?, f.next()?);
            (a == arch).then_some((name, (geom, sha)))
        })
        .collect();
    let (frames, distinct) = draw_frames(armed);
    let mut findings = Vec::new();
    let mut digests = std::collections::BTreeMap::new();
    for (name, f) in &frames {
        let (geom, sha) = row_of(f);
        if !armed {
            crate::stderr_line!("warp-record\t{arch}\t{name}\t{geom}\t{sha}");
        }
        match recorded.get(name.as_str()) {
            None => findings.push(format!("{name}: not recorded for {arch}")),
            Some(&(g, s)) if (g, s) != (geom.as_str(), sha.as_str()) => findings.push(format!(
                "{name}: drew {geom} {sha}, the record holds {g} {s}"
            )),
            Some(_) => {}
        }
        digests.insert(name.clone(), sha);
    }
    for name in recorded.keys() {
        if !digests.contains_key(*name) {
            findings.push(format!("{name}: recorded for {arch} but not drawn"));
        }
    }
    for (a, b) in &distinct {
        assert_ne!(
            digests.get(a),
            digests.get(b),
            "{a} and {b} differ in their input, so their frames must differ"
        );
    }
    assert!(
        findings.is_empty(),
        "THIS BUILD'S WARP FRAMES ({}) ARE NOT wgpu-dx12's ({} of {} frames): {findings:#?}. \
         Compare the live differential (`--features wgpu-oracle`) to find the first texel; never \
         re-record to pass.",
        if armed { "D3D11 arm armed" } else { "unarmed" },
        findings.len(),
        frames.len()
    );
    end_unless_the_colour_row_ran();
}
