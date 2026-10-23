// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_render::{GlowQuad, RenderInput, Theme, premul_rgb};

mod common;

/// A small grid with text and a row of additive glow quads.
fn glow_scene(gpu: &aterm_gpu::GpuRenderer) -> RenderInput {
    let (rows, cols) = (6usize, 24usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"frame instrumentation\r\nglow over text");
    let mut input = term.cell_frame(rows, cols);
    let (cw, ch) = gpu.cell_size();
    for col in 2..10 {
        input.cursor_glow_add.push(GlowQuad {
            row: 1,
            x: (col * cw) as u16,
            y: ch as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x0050_FA7B, 160),
            alpha: 0,
            color2: premul_rgb(0x0050_FA7B, 160),
            alpha2: 0,
        });
    }
    input
}

/// Install a sink so presents open GPU-time frames.
#[cfg(any(target_os = "macos", wgpu_arm))]
fn timing_on() {
    fn sink(_: aterm_gpu::gpu_time::GpuFrameTime) {}
    let _ = aterm_gpu::gpu_time::install_sink(sink);
    assert!(aterm_gpu::gpu_time::sink_installed());
}

/// The settled outcome of GPU-time frame `serial`.
#[cfg(target_os = "macos")]
fn await_settled(serial: u64) -> aterm_gpu::gpu_time::Settled {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(outcome) = aterm_gpu::gpu_time::settled(serial) {
            return outcome;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "GPU-time frame {serial} never settled: a completion handler did not fire"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// A present on the first-party Metal arm reports a nonzero GPU time summed over at least its
/// two command buffers (Submit A's offscreen frame, Submit B's compose), and every present gets
/// its own serial.
#[test]
#[cfg(target_os = "macos")]
fn a_metal_present_reports_its_gpu_time() {
    timing_on();
    let mut gpu = crate::common::gpu(18.0, Theme::default());
    let mut win = aterm_gpu::WindowGpu::new();
    let input = glow_scene(&gpu);
    assert!(gpu.present_virtual(&mut win, &input, false, None, None));
    let first = gpu
        .last_gpu_time_serial()
        .expect("a timed Metal present records its serial");
    match await_settled(first) {
        aterm_gpu::gpu_time::Settled::Timed(t) => {
            assert_eq!(t.serial, first);
            assert!(t.gpu_ns > 0, "the device ran the frame: {t:?}");
            assert!(
                t.parts >= 2,
                "Submit A and Submit B are both parts of the present: {t:?}"
            );
        }
        other => panic!("a healthy present must be timed, got {other:?}"),
    }
    assert!(gpu.present_virtual(&mut win, &input, false, None, None));
    let second = gpu.last_gpu_time_serial().expect("timed again");
    assert!(second > first, "each present opens its own frame");
    assert!(matches!(
        await_settled(second),
        aterm_gpu::gpu_time::Settled::Timed(_)
    ));
}

/// The wgpu arm (the Windows and BSD presents, the oracle here) times a present with per-pass
/// timestamp queries: the readback waits for the device to run the frame, lands after the
/// present returns and is harvested by `poll_gpu_time`, reporting one part per render pass the
/// present ran (the offscreen plan's passes and the blit, with bloom off), so no pass goes
/// untimed and no healthy present is dropped.
#[test]
#[cfg(wgpu_arm)]
fn a_wgpu_present_reports_its_gpu_time_through_timestamp_queries() {
    timing_on();
    let mut gpu = crate::common::wgpu_oracle(18.0, Theme::default());
    gpu.set_bloom(false);
    if !gpu.gpu_time_supported() {
        aterm_spec::host::could_not_run(
            "this adapter grants no TIMESTAMP_QUERY, so the wgpu arm has no GPU clock",
        );
    }
    for present in 0..20 {
        let mut win = aterm_gpu::WindowGpu::new();
        let input = glow_scene(&gpu);
        assert!(gpu.present_virtual(&mut win, &input, false, None, None));
        let serial = gpu
            .last_gpu_time_serial()
            .expect("a timed wgpu present records its serial");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let outcome = loop {
            gpu.poll_gpu_time();
            if let Some(outcome) = aterm_gpu::gpu_time::settled(serial) {
                break outcome;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the timestamp readback of frame {serial} never mapped"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        match outcome {
            aterm_gpu::gpu_time::Settled::Timed(t) => {
                assert!(t.gpu_ns > 0, "the timed passes ran on the device: {t:?}");
                assert_eq!(
                    t.parts,
                    gpu.frame_shape().passes + 1,
                    "every offscreen pass and the blit are timed: {t:?}"
                );
            }
            other => panic!("healthy wgpu present {present} must be timed, got {other:?}"),
        }
    }
}

/// The frame shape names what the plan drew: the glow quads land in the `glow_add` stream, the
/// pass count is the plan's, and the built bytes cover every non-empty stream.
#[test]
fn the_frame_shape_counts_streams_passes_and_bytes() {
    let mut gpu = crate::common::gpu(18.0, Theme::default());
    let mut win = aterm_gpu::WindowGpu::new();
    let input = glow_scene(&gpu);
    assert!(gpu.present_virtual(&mut win, &input, false, None, None));
    let shape = gpu.frame_shape();
    assert_eq!(shape.passes, gpu.last_frame_passes());
    assert_eq!(shape.ablation, aterm_gpu::UnormAblation::Off);
    let streams: Vec<(&str, u32)> = shape.streams().collect();
    assert!(
        streams.contains(&("glow_add", 8)),
        "eight glow quads in the glow_add stream: {streams:?}"
    );
    assert!(
        streams.iter().any(|&(name, n)| name == "glyph" && n > 0),
        "the text draws glyph instances: {streams:?}"
    );
    assert!(
        shape.instance_bytes >= 8 * 16,
        "the built bytes cover at least the glow instances: {shape:?}"
    );
    assert_eq!(
        shape.total_instances(),
        streams.iter().map(|&(_, n)| u64::from(n)).sum::<u64>()
    );
    assert!(
        win.last_present_encode_ns() > 0
            && win.last_present_encode_ns() <= win.last_present_work_ns(),
        "encode is a nonzero part of the present's work"
    );
}

/// The ablation seam prices the Unorm light a glow frame pays for on the split, and only it: with
/// the light dropped the same frame runs one pass and still stages the glow instances; thinned, it
/// keeps every pass; with the seam off again the passes come back. The shape says what the arm's
/// plan took, so an arm that ignored the seam reads as the plan it ran.
#[test]
fn the_unorm_ablation_saves_the_glow_frames_passes() {
    use aterm_gpu::UnormAblation;
    let mut gpu = crate::common::gpu(18.0, Theme::default());
    gpu.set_unorm_route(aterm_gpu::UnormRoute::Split);
    let input = glow_scene(&gpu);
    let under = |gpu: &mut aterm_gpu::GpuRenderer, ablation| {
        gpu.set_unorm_ablation(ablation);
        let mut win = aterm_gpu::WindowGpu::new();
        assert!(gpu.present_virtual(&mut win, &input, false, None, None));
        gpu.frame_shape()
    };
    let full = under(&mut gpu, UnormAblation::Off);
    let dropped = under(&mut gpu, UnormAblation::Drop);
    let thinned = under(&mut gpu, UnormAblation::Thin);
    let again = under(&mut gpu, UnormAblation::Off);
    assert_eq!(full.ablation, UnormAblation::Off);
    assert_eq!(dropped.ablation, UnormAblation::Drop);
    assert_eq!(thinned.ablation, UnormAblation::Thin);
    assert_eq!((full.unorm_draws, full.unorm_instances), (1, 8));
    assert_eq!((dropped.unorm_draws, dropped.unorm_instances), (0, 0));
    assert_eq!((thinned.unorm_draws, thinned.unorm_instances), (1, 1));
    assert!(
        dropped.passes < full.passes,
        "the glow's Unorm group costs passes: {} full vs {} dropped",
        full.passes,
        dropped.passes
    );
    assert_eq!(dropped.passes, 1, "an all-sRGB plan is one pass");
    assert_eq!(thinned.passes, full.passes, "thinning keeps every pass");
    assert_eq!(
        dropped.instances, full.instances,
        "the ablation changes the plan, not the instances built"
    );
    assert_eq!(thinned.instances, full.instances);
    assert_eq!(again.passes, full.passes, "the seam turns off cleanly");
    assert_eq!(again.ablation, UnormAblation::Off);
}

/// On a device whose framebuffer-fetch probe passed, the glow frame's light draws through its
/// twin inside the one sRGB pass, and the shape says so; the split, chosen by the seam, opens
/// its passes again for the same draws. Elsewhere the route stays the split whatever the seam
/// wants.
#[test]
fn the_fetch_route_draws_the_glow_frames_light_in_one_pass() {
    use aterm_gpu::{FetchProbe, UnormRoute};
    let mut gpu = crate::common::gpu(18.0, Theme::default());
    let input = glow_scene(&gpu);
    let under = |gpu: &mut aterm_gpu::GpuRenderer, route| {
        gpu.set_unorm_route(route);
        let mut win = aterm_gpu::WindowGpu::new();
        assert!(gpu.present_virtual(&mut win, &input, false, None, None));
        gpu.frame_shape()
    };
    let fetch = under(&mut gpu, UnormRoute::Fetch);
    let split = under(&mut gpu, UnormRoute::Split);
    assert_eq!(split.route, UnormRoute::Split);
    assert_eq!((split.unorm_draws, split.unorm_instances), (1, 8));
    assert_eq!(
        (fetch.unorm_draws, fetch.unorm_instances, fetch.instances),
        (split.unorm_draws, split.unorm_instances, split.instances),
        "the route changes where the light draws, not what draws"
    );
    let probe = gpu.fetch_probe().expect("the frames above minted the arm");
    if probe == FetchProbe::Passed {
        assert_eq!(fetch.route, UnormRoute::Fetch);
        assert_eq!(fetch.passes, 1, "the light draws inside the one sRGB pass");
        assert!(
            split.passes > fetch.passes,
            "the split pays passes for the same light: {} vs {}",
            split.passes,
            fetch.passes
        );
    } else {
        assert_eq!(fetch.route, UnormRoute::Split, "{probe:?} keeps the split");
        assert_eq!(fetch.passes, split.passes);
    }
}
