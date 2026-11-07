// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each arm's frames held to aterm's CPU side, and against a record: the
//! replay fixtures' readbacks, the effect layers' readback, the production
//! present's destinations and the translucent window's (`replay_fixtures`),
//! the video tap's take, and where the arm draws them the EDR present's
//! `Rgba16Float` destination (macOS and Windows) and the present blit and
//! crown rows run standalone over every destination and uniform arm (macOS,
//! `metal::blit`).
//!
//! The verdict is each frame's CPU twin (docs/AUDIT-third-party-2026-10-15.md
//! ORACLE-ADAPTER and HR-ORACLE): the CPU renderer's frame of the same input
//! for a readback, and for a layered readback, a present, a layer, a video
//! frame, an EDR present and a blit or crown row the CPU's run of the
//! present's passes over it (the CPU's bloom law, [`shimmer_twin`], the
//! card, [`blit_twin`] under the uniform [`twin_uniform`] derives from the
//! CPU's laws rather than from the arm's builder, and the crown's laws at
//! the frame's offset and visible rows), each held to the parity suites'
//! tolerance ([`twin_law`]). The arm's atlas takes its glyphs from the CPU
//! renderer it wraps, so a twin draws with the faces the arm does: the
//! verdict is decided alike on any adapter (another Mac's GPU, llvmpipe,
//! WARP) and through a font or OS update, and moves only when the arm draws
//! otherwise than aterm's own renderer. Every frame has a twin but a video
//! take the adapter holds converted (Display P3, scRGB), which the output
//! names; a present over a layer no twin draws (a bloom over ramped or fx
//! light, a crown under a glide) fails the test, so the fixture that brings
//! one brings its twin. Controls hold each twinned layer to showing: a
//! frame that runs it must fail its twin drawn without the bloom, without
//! the shimmer, without the crown or with a wider drop border ([`Mutant`]);
//! a cropped present, as every GUI present is, carries the shimmer and the
//! crown, and one at a window's size the drop border's width.
//!
//! Each frame's SHA-256 is also held against the record of the arm this
//! build presents with: `oracle/metal-frames.tsv` (macOS),
//! `oracle/gl-frames.tsv` (Linux) and `oracle/warp-frames.tsv` (Windows),
//! drawn by wgpu (GW.5 and GW.6): wgpu-metal on an Apple M4 Pro, wgpu's
//! Vulkan backend on the Linux lane's Mesa (keyed by the GL arm's renderer
//! string on the same Mesa, llvmpipe, since the GL arm reads them), and
//! wgpu-dx12 on WARP ("Microsoft Basic Render Driver") in the Windows lane's
//! guest, for each architecture the lane runs, before GW.6 removed wgpu. A
//! row names the architecture and the adapter; its key does not name the
//! system's fonts, driver build or OS, so an update on the same adapter moves
//! rows with no change to the arm. The record is therefore a signal, never
//! the verdict: on an adapter it holds, each frame drawn otherwise is printed
//! (`frame-record: moved:`) and the verdict line counts them; on one it does
//! not, the verdict line says so.
//!
//! The test prints every frame's row (`frame-record` lines) whether it
//! passes or not. A recorded row changes only by a stated re-record, never by
//! a mode of the test: those lines copied into the file by hand, from a tree
//! whose frames hold to their twins and whose arm code did not change but by
//! a fix that brought them to their twins, with the commit naming the rows,
//! why they moved (the system update, before and after, or the fix) and that
//! the arm drew them, and the record's header naming their drawer. A frame
//! the oracle gains is recorded the same way, from the arm, once it holds to
//! its twin.

use super::replay_fixtures::*;
use super::*;

/// The recorded rows: `arch`, adapter, frame, `WxH`, SHA-256 (hex).
#[cfg(target_os = "macos")]
const RECORD: &str = include_str!("../oracle/metal-frames.tsv");
#[cfg(target_os = "linux")]
const RECORD: &str = include_str!("../oracle/gl-frames.tsv");
#[cfg(windows)]
const RECORD: &str = include_str!("../oracle/warp-frames.tsv");

/// The arm this build presents with, as the findings name it.
#[cfg(target_os = "macos")]
const ARM: &str = "Metal";
#[cfg(target_os = "linux")]
const ARM: &str = "GL";
#[cfg(windows)]
const ARM: &str = "D3D11";

/// The adapter the armed arm draws on, the record's key beside the
/// architecture; COULD NOT RUN when there is no device to draw on.
fn native_adapter() -> String {
    #[cfg(target_os = "macos")]
    let name = crate::metal::ffi::Device::preferred_listed()
        .map(|d| d.name())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no Metal device"));
    #[cfg(target_os = "linux")]
    let name = crate::gl::test_device().caps().renderer.clone();
    #[cfg(windows)]
    let name = crate::d3d11::device::D3dDevice::headless()
        .map(|d| d.caps().adapter.clone())
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no Direct3D 11 device: {e}")));
    name
}

/// A renderer armed with this build's arm on `family` (`None`: the default
/// system or bundled primary face).
fn record_renderer_with(family: Option<&str>) -> GpuRenderer {
    let ctx = GpuContext::new_headless()
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU context: {e}")));
    let cpu = Renderer::from_system_with_family(family, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, family.map(str::to_owned), Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    #[cfg(target_os = "macos")]
    gpu.arm_metal_for_test();
    #[cfg(target_os = "linux")]
    gpu.arm_gl();
    #[cfg(windows)]
    {
        gpu.arm_d3d11();
        assert!(
            gpu.d3d11_arm_ready(),
            "the D3D11 arm mints: {:?}",
            gpu.d3d11_mint_error
        );
    }
    gpu
}

/// Assert the last frame drew on this build's arm.
fn drew_on(gpu: &GpuRenderer, name: &str) {
    #[cfg(target_os = "macos")]
    let on_arm = gpu.metal_render_armed();
    #[cfg(target_os = "linux")]
    let on_arm = gpu.last_frame_on_gl();
    #[cfg(windows)]
    let on_arm = gpu.last_frame_on_d3d11();
    assert!(
        on_arm,
        "{name}: not drawn on the {ARM} arm (see the arm's note)"
    );
}

/// How a drawn frame stores each of a texel's four channels, in the order
/// its destination holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Texel {
    /// A byte of sRGB-encoded colour: RGBA8 for a readback, a present's
    /// destination and a video take, BGRA8 for a layer and a `Bgra8Unorm`
    /// blit or crown target.
    Unorm8,
    /// An `f32`, little-endian, on a byte's `0..=1` scale: a twin of a byte
    /// destination before the destination rounds it.
    Unit,
    /// An IEEE half of linear light (1.0 the SDR white), little-endian: an
    /// `Rgba16Float` blit or crown target.
    #[cfg(target_os = "macos")]
    Half,
    /// An `f32` of linear light, little-endian: an EDR present's halves
    /// widened, and a twin of a half-float destination.
    Linear,
}

/// One drawn frame: its geometry, how it stores a channel, and its bytes.
#[derive(Clone)]
struct Drawn {
    width: usize,
    height: usize,
    texel: Texel,
    bytes: Vec<u8>,
}

impl From<Frame> for Drawn {
    fn from(f: Frame) -> Self {
        Self {
            width: f.width,
            height: f.height,
            texel: Texel::Unorm8,
            bytes: f.rgba_bytes(),
        }
    }
}

impl Drawn {
    /// A twin's exact texels (RGBA, as the shader returns them) as `dest`
    /// stores them: in its channel order, and clamped to `0..=1` where it
    /// is a byte.
    fn twin(width: usize, height: usize, texels: &[[f32; 4]], dest: Dest) -> Self {
        let bytes = texels
            .iter()
            .flat_map(|&[r, g, b, a]| {
                let px = if dest == Dest::Bgra8 {
                    [b, g, r, a]
                } else {
                    [r, g, b, a]
                };
                px.map(|v| {
                    if dest == Dest::Half {
                        v
                    } else {
                        v.clamp(0.0, 1.0)
                    }
                })
            })
            .flat_map(f32::to_le_bytes)
            .collect();
        Self {
            width,
            height,
            texel: if dest == Dest::Half {
                Texel::Linear
            } else {
                Texel::Unit
            },
            bytes,
        }
    }

    /// Each channel in 8-bit codes: a byte as itself, a unit float times
    /// 255, and linear light by its extended sRGB encoding times 255 (the
    /// scRGB convention), so a code is the same step in every destination.
    fn codes(&self) -> Vec<f32> {
        let encoded = |v: f32| {
            let m = v.abs();
            let e = if m <= 0.003_130_8 {
                12.92 * m
            } else {
                1.055 * m.powf(1.0 / 2.4) - 0.055
            };
            e.copysign(v) * 255.0
        };
        let floats = |bytes: &[u8]| -> Vec<f32> {
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&f| f32::from_le_bytes(f))
                .collect()
        };
        match self.texel {
            Texel::Unorm8 => self.bytes.iter().map(|&b| f32::from(b)).collect(),
            Texel::Unit => floats(&self.bytes).into_iter().map(|v| v * 255.0).collect(),
            #[cfg(target_os = "macos")]
            Texel::Half => self
                .bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&h| encoded(f16_bits_to_f32(u16::from_le_bytes(h))))
                .collect(),
            Texel::Linear => floats(&self.bytes).into_iter().map(encoded).collect(),
        }
    }
}

/// The destination a twin's texels stand for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dest {
    /// `Rgba8Unorm`, and a present's or layer's destination read back as RGBA.
    Rgba8,
    /// `Bgra8Unorm` as stored, and a layer as the record took it.
    Bgra8,
    /// `Rgba16Float`: unclamped.
    Half,
}

/// One frame's row: its geometry and the SHA-256 of its bytes.
fn row_of(f: &Drawn) -> (String, String) {
    (
        format!("{}x{}", f.width, f.height),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&f.bytes)),
    )
}

/// Frames by name, in drawing order.
type Named = Vec<(String, Drawn)>;

/// Twins by the name of the frame each stands for.
type Twins = std::collections::BTreeMap<String, Drawn>;

/// What [`draw_frames`] drew: every frame the record describes, in drawing
/// order; the frames each fixture claims to differ (`(a, b)`: the input
/// changed between them) and to equal (a readback redrawn over its
/// residency); the twin of each frame aterm's CPU side draws too, and why
/// any other has none; the controls, each a frame's twin drawn as a
/// [`Mutant`], which that frame must fail; and the shape of each readback
/// held to the CPU renderer's frame, as the arm encoded it
/// ([`Self::shape`]).
struct Drawing {
    frames: Named,
    distinct: Vec<(String, String)>,
    same: Vec<(String, String)>,
    twins: Twins,
    untwinned: Vec<(String, String)>,
    mutants: Vec<(String, Mutant, Drawn)>,
    shapes: std::collections::BTreeMap<String, String>,
}

impl Drawing {
    /// The shape of the readback `gpu` just drew as `name` ([`shape_note`]); `epoch` is the
    /// font epoch before it.
    fn shape(&mut self, name: &str, gpu: &GpuRenderer, epoch: u64) {
        self.shapes.insert(
            name.to_owned(),
            shape_note(&gpu.frame_shape(), gpu.cpu.font_epoch() != epoch),
        );
    }

    /// `frame`, drawn as `name`, and its twin.
    fn push(&mut self, name: String, frame: Drawn, twin: Drawn) {
        self.twins.insert(name.clone(), twin);
        self.frames.push((name, frame));
    }

    /// `frame`, drawn as `name`, which no twin stands for on this adapter,
    /// and why.
    fn push_untwinned(&mut self, name: String, frame: Drawn, why: String) {
        self.untwinned.push((name.clone(), why));
        self.frames.push((name, frame));
    }
}

/// A twin drawn otherwise than the law says, for a control: the frame as
/// drawn must fail it, so the layer the mutant changes shows in that frame
/// past the twin law's bar, and an arm that drops or drifts it fails its
/// twin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mutant {
    /// The law's twin.
    Law,
    /// Without the bloom.
    NoBloom,
    /// Without the heat shimmer.
    NoShimmer,
    /// Without the SDR crown.
    NoCrown,
    /// The drop overlay's border two pixels wider than the CPU presenter's.
    WideBorder,
}

/// Every frame the record describes, with each one's CPU twin where aterm
/// draws one: the CPU renderer's frame of the same input for a readback, and
/// for a present, a layer, a video frame, an EDR present and a blit or crown
/// row the CPU's run of the present's shaders over it.
fn draw_frames() -> Drawing {
    draw_frames_with(None)
}

fn draw_frames_with(family: Option<&str>) -> Drawing {
    let mut d = Drawing {
        frames: Vec::new(),
        distinct: Vec::new(),
        same: Vec::new(),
        twins: Twins::new(),
        untwinned: Vec::new(),
        mutants: Vec::new(),
        shapes: std::collections::BTreeMap::new(),
    };

    // Readbacks: the encode plan, three frames each (the second and third
    // reuse the residency), every stream each fixture claims drawn.
    let mut gpu = record_renderer_with(family);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut fixtures = fixtures(&gpu, true);
    fixtures.push(image_fixture(&gpu));
    fixtures.push(fx_fixture(&gpu));
    let mut win = WindowGpu::new();
    for (name, input, drive) in &fixtures {
        let twin = cpu_twin(&mut gpu, input);
        for pass in 0..3 {
            let epoch = gpu.cpu.font_epoch();
            let f = gpu
                .try_render_input(&mut win, input, None)
                .unwrap_or_else(|e| panic!("{name}: the readback: {e}"));
            drew_on(&gpu, name);
            let frame = format!("{name}/readback/{pass}");
            d.shape(&frame, &gpu, epoch);
            if pass == 0 {
                let drawn: Vec<StreamId> = gpu.frame_plan.iter().map(|i| i.stream).collect();
                for want in drive {
                    assert!(
                        drawn.contains(want),
                        "{name}: the fixture claims {want:?} but the plan never drew it"
                    );
                }
            }
            d.push(frame, f.into(), twin.clone());
        }
    }
    gpu.set_cursor_opacity(0.5);
    let cursor = fixtures_base_for_cursor();
    let epoch = gpu.cpu.font_epoch();
    let f = gpu
        .try_render_input(&mut win, &cursor, None)
        .expect("the cursor-blend readback");
    drew_on(&gpu, "cursor_blend");
    d.shape("cursor_blend/readback/0", &gpu, epoch);
    assert!(
        gpu.frame_plan
            .iter()
            .any(|i| i.stream == StreamId::CursorBlock && i.pipe == DrawPipe::CursorBlend),
        "cursor_opacity 0.5 routes the cursor fill through CursorBlend"
    );
    let twin = cpu_twin(&mut gpu, &cursor);
    d.push("cursor_blend/readback/0".to_owned(), f.into(), twin);
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
    let tray = TrayQuad {
        rgba: &card,
        pw: 24,
        ph: 10,
        dx: 5,
        dy: 3,
        scrim: None,
        serial: 0,
        delta: None,
        above: &[],
    };
    let layered = |gpu: &mut GpuRenderer, mutant| -> Drawn {
        layered_source(gpu, &heat, Some(&tray), mutant).into()
    };
    let twin = layered(&mut gpu, Mutant::Law);
    let mut win = WindowGpu::new();
    for pass in 0..3 {
        let f = gpu
            .try_render_input(&mut win, &heat, Some(tray))
            .expect("the layered readback");
        drew_on(&gpu, "heat/layered");
        d.push(format!("heat/layered/{pass}"), f.into(), twin.clone());
    }
    for mutant in [Mutant::NoBloom, Mutant::NoShimmer] {
        let bare = layered(&mut gpu, mutant);
        d.mutants.push(("heat/layered/0".to_owned(), mutant, bare));
    }
    for pass in ["1", "2"] {
        d.same
            .push(("heat/layered/0".to_owned(), format!("heat/layered/{pass}")));
    }
    d.distinct
        .push(("heat/readback".to_owned(), "heat/layered/0".to_owned()));
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let epoch = gpu.cpu.font_epoch();
    let f = gpu
        .try_render_input(&mut win, &heat, None)
        .expect("the plain heat readback");
    drew_on(&gpu, "heat/readback");
    d.shape("heat/readback", &gpu, epoch);
    let twin = cpu_twin(&mut gpu, &heat);
    d.push("heat/readback".to_owned(), f.into(), twin);

    // The production present into the virtual target: the dirty-row scissor
    // over the resident offscreen, the present copy, bloom, shimmer, the
    // glide with its apron, the card, the letterbox blit with the bell
    // inversion and the drop overlay, and the SDR crown.
    let mut gpu = record_renderer_with(family);
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
        serial: 0,
        delta: None,
        above: &[],
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
        let (fw, fh) = gpu.frame_size(input.rows, input.cols);
        let exact = (fw as u32, fh as u32);
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
            drew_on(&gpu, name);
            let card = with_tray.then(tray);
            let twin = |gpu: &mut GpuRenderer, mutant| {
                present_twin(
                    gpu,
                    &win,
                    input,
                    (invert, overlay),
                    None,
                    exact,
                    card.as_ref(),
                    mutant,
                )
            };
            let law = twin(&mut gpu, Mutant::Law);
            let frame = format!("{name}/present/{pass}");
            if overlay.is_some() && *name == "text_cursor" {
                let wide = twin(&mut gpu, Mutant::WideBorder);
                d.mutants.push((frame.clone(), Mutant::WideBorder, wide));
            }
            let layers: &[Mutant] = match (pass, *name) {
                (0, "heat") => &[Mutant::NoBloom, Mutant::NoShimmer, Mutant::NoCrown],
                (0, "glow_halo") => &[Mutant::NoBloom],
                _ => &[],
            };
            for &mutant in layers {
                let bare = twin(&mut gpu, mutant);
                d.mutants.push((frame.clone(), mutant, bare));
            }
            d.push(frame, f.into(), law);
        }
        let mut changed = input.clone();
        changed.row_rev_lane = 0;
        if let Some(cell) = changed.cells.get_mut(1).and_then(|r| r.get_mut(2)) {
            cell.ch = 'Z';
        }
        let f = presented(&mut gpu, &mut win, &changed, false, None, None, None);
        drew_on(&gpu, name);
        let twin = present_twin(
            &mut gpu,
            &win,
            &changed,
            (false, None),
            None,
            exact,
            None,
            Mutant::Law,
        );
        d.push(format!("{name}/present/changed"), f.into(), twin);
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
        drew_on(&gpu, name);
        let twin = |gpu: &mut GpuRenderer, mutant| {
            present_twin(
                gpu,
                &win,
                &changed,
                (false, None),
                Some(crop),
                dest,
                None,
                mutant,
            )
        };
        let law = twin(&mut gpu, Mutant::Law);
        let frame = format!("{name}/present/cropped");
        // The crown on a cropped destination. (The changed cell letters the
        // blank row the heat scene's haze rises through, so it has none.)
        if matches!(*name, "glow_halo" | "heat") {
            let bare = twin(&mut gpu, Mutant::NoCrown);
            d.mutants.push((frame.clone(), Mutant::NoCrown, bare));
        }
        d.push(frame, f.into(), law);
        if *name == "heat" {
            // The shimmer on a cropped destination, as every GUI present
            // takes it: the scene as it stands, its haze row and the comet
            // in the visible rows.
            let (_, ch) = gpu.cell_size();
            let crop = PresentCrop {
                source_y: (ch / 2) as u32,
                height: (3 * ch) as u32,
            };
            let dest = (fw as u32 + 11, crop.height + 9);
            let f = presented(
                &mut gpu,
                &mut win,
                input,
                false,
                None,
                None,
                Some((crop, dest)),
            );
            drew_on(&gpu, name);
            let twin = |gpu: &mut GpuRenderer, mutant| {
                present_twin(
                    gpu,
                    &win,
                    input,
                    (false, None),
                    Some(crop),
                    dest,
                    None,
                    mutant,
                )
            };
            let law = twin(&mut gpu, Mutant::Law);
            let frame = "heat/present/cropped-haze".to_owned();
            let bare = twin(&mut gpu, Mutant::NoShimmer);
            d.mutants.push((frame.clone(), Mutant::NoShimmer, bare));
            d.push(frame, f.into(), law);
        }
        d.distinct
            .push((format!("{name}/present/0"), format!("{name}/present/1")));
        d.distinct
            .push((format!("{name}/present/0"), format!("{name}/present/2")));
    }
    // The changed cell lies under the image plane's opaque texels in
    // `images`; the plain text frame shows it.
    d.distinct.push((
        "text_cursor/present/0".to_owned(),
        "text_cursor/present/changed".to_owned(),
    ));
    rim_present_frame(&mut gpu, &mut d);

    // The translucent window: a background opacity under 1 reaches the
    // readback's alpha (the virtual present's target is opaque).
    let mut gpu = record_renderer_with(family);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let input = fixtures_base_for_cursor();
    for (label, opacity) in [("opaque", 1.0f32), ("translucent", 0.55)] {
        gpu.set_background_opacity(opacity);
        let mut win = WindowGpu::new();
        let epoch = gpu.cpu.font_epoch();
        let f = gpu
            .try_render_input(&mut win, &input, None)
            .unwrap_or_else(|e| panic!("{label}: the readback: {e}"));
        drew_on(&gpu, label);
        let frame = format!("window/{label}/readback");
        d.shape(&frame, &gpu, epoch);
        let twin = cpu_twin(&mut gpu, &input);
        d.push(frame, f.into(), twin);
    }
    d.distinct.push((
        "window/opaque/readback".to_owned(),
        "window/translucent/readback".to_owned(),
    ));
    #[cfg(target_os = "macos")]
    window_layer_frames(&mut d, family);
    video_take_frames(&mut d, family);
    #[cfg(any(target_os = "macos", windows))]
    edr_present_frames(&mut d, family);
    #[cfg(target_os = "macos")]
    blit_frames(&mut d, family);
    #[cfg(target_os = "macos")]
    crown_row_frames(&mut d);
    d
}

/// The drop overlay's border at a window's size, as `rim/present/cropped`:
/// a frame at least 880 x 840 cropped to 700 visible rows, under the
/// level-up rim's widest scale (40/16). The law's base is 3 px over the
/// visible rows where the source's short side alone gives 4, so the shader's
/// reading of a 7 px border, the rows the law takes and the scale each show
/// in the frame; the replay fixtures' frames sit at the law's 2 px floor.
fn rim_present_frame(gpu: &mut GpuRenderer, d: &mut Drawing) {
    use aterm_render::drop_overlay_border_px;
    let (cw, ch) = gpu.cell_size();
    let (rows, cols) = (840usize.div_ceil(ch), 880usize.div_ceil(cw));
    let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    term.process(b"$ the rim over a window >_\r\n");
    term.process(b"\x1b[32mcharged\x1b[0m plain 0123456789");
    let input = term.cell_frame(rows, cols);
    let (fw, fh) = gpu.frame_size(rows, cols);
    let crop = PresentCrop {
        source_y: ch as u32,
        height: 700,
    };
    assert!(
        drop_overlay_border_px(fw, 700, 40) == 7 && drop_overlay_border_px(fw, fh, 40) == 10,
        "the rim frame ({fw}x{fh}) takes 7 px over its visible rows and 10 over its source"
    );
    let overlay = DropOverlay {
        accent: 0x0040_a0ff,
        wash_a: 40,
        border_a: 200,
        border_scale_q4: 40,
    };
    let dest = (fw as u32 + 11, crop.height + 9);
    let mut win = WindowGpu::new();
    let f = presented(
        gpu,
        &mut win,
        &input,
        false,
        Some(overlay),
        None,
        Some((crop, dest)),
    );
    drew_on(gpu, "rim");
    let twin = |gpu: &mut GpuRenderer, mutant| {
        present_twin(
            gpu,
            &win,
            &input,
            (false, Some(overlay)),
            Some(crop),
            dest,
            None,
            mutant,
        )
    };
    let frame = "rim/present/cropped".to_owned();
    let wide = twin(gpu, Mutant::WideBorder);
    d.mutants.push((frame.clone(), Mutant::WideBorder, wide));
    let law = twin(gpu, Mutant::Law);
    d.push(frame, f.into(), law);
}

/// What the arm's encode of a readback staged ([`GpuRenderer::frame_shape`]), and whether the
/// font epoch moved during it (the readback then encoded the frame again): beside a finding seen
/// once, what that frame's encode held.
fn shape_note(s: &FrameShape, epoch_moved: bool) -> String {
    format!(
        "encoded in {} passes, {} fx instances ({} refused, {} atlas bytes written), route \
         {:?}, fetch {:?}{}",
        s.passes,
        s.fx_instances,
        s.fx_refused,
        s.fx_atlas_bytes,
        s.route,
        s.fetch_probe,
        if epoch_moved {
            ", encoded again after the font epoch moved"
        } else {
            ""
        }
    )
}

/// `findings`, each followed by its frame's shape where [`Drawing::shapes`] holds one.
fn with_shapes(
    findings: &[String],
    shapes: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    findings
        .iter()
        .map(|f| {
            let frame = f.split_once(": ").map_or(f.as_str(), |(n, _)| n);
            shapes
                .get(frame)
                .map_or_else(|| f.clone(), |s| format!("{f} [{s}]"))
        })
        .collect()
}

/// The CPU renderer's frame of `input`: the renderer the arm wraps, under
/// the settings the arm reads from it (opacity, cursor, chrome).
fn cpu_twin(gpu: &mut GpuRenderer, input: &RenderInput) -> Drawn {
    gpu.cpu.render_input(input).into()
}

/// The blit uniform a present of `input`'s `sw` x `sh` frame into a `dw` x
/// `dh` SDR, opaque destination asks for, derived from aterm's CPU laws and
/// never from the arm's builder (`present_blit_uniform`), so a drift there
/// moves the arm off its twin: the frame placed by
/// `aterm_render::frame_placement`, the crop's rows of it, the bands in the
/// page's background (the CPU presenter's `present_band_bg`), the chrome rows
/// the CPU renderer declares, and the drop overlay's accent, its wash and
/// border alphas and its border width over the visible frame
/// (`aterm_render::drop_overlay_border_px`, the CPU presenter's law).
fn twin_uniform(
    gpu: &GpuRenderer,
    input: &RenderInput,
    (invert, overlay): (bool, Option<DropOverlay>),
    crop: Option<PresentCrop>,
    (sw, sh): (u32, u32),
    (dw, dh): (u32, u32),
) -> BlitUniform {
    let unit = |packed: u32, shift: u32| ((packed >> shift) & 0xff) as f32 / 255.0;
    let (visible_y, visible_h) = crop.map_or((0, sh), |c| {
        let y = c.source_y.min(sh);
        (y, c.height.min(sh - y))
    });
    let place = aterm_render::frame_placement(dw as usize, dh as usize, sw as usize, sh as usize);
    let band = if input.default_bg == aterm_core::render::COLOR_UNSET {
        gpu.theme.bg
    } else {
        input.default_bg
    };
    let chrome = gpu.cpu.chrome_extent_px(sh as usize);
    let mut u = BlitUniform {
        flag: u32::from(invert),
        overlay: u32::from(overlay.is_some()),
        border_px: 0.0,
        encode_srgb: if gpu.ctx.srgb_offscreen { 0.0 } else { 1.0 },
        accent: [0.0; 3],
        chrome_y1: chrome.end as f32,
        dims: [sw as f32, sh as f32],
        wash_a: 0.0,
        border_a: 0.0,
        band: [unit(band, 16), unit(band, 8), unit(band, 0), 1.0],
        content_off: [place.x.origin as f32, place.y.origin as f32],
        hdr: 0.0,
        translucent: 0.0,
        sdr_white_scale: 1.0,
        visible_y: visible_y as f32,
        visible_h: visible_h as f32,
        premult: 0.0,
        chrome_y0: chrome.start as f32,
        chrome_pad: [0.0; 3],
    };
    if let Some(o) = overlay {
        let border = aterm_render::drop_overlay_border_px(
            sw as usize,
            visible_h as usize,
            o.border_scale_q4,
        );
        u.border_px = border as f32;
        u.accent = [unit(o.accent, 16), unit(o.accent, 8), unit(o.accent, 0)];
        u.wash_a = f32::from(o.wash_a) / 255.0;
        u.border_a = f32::from(o.border_a) / 255.0;
    }
    u
}

/// The CPU's frame of `input` with the present-time layers over it, as an
/// armed readback's offscreen and a present's copy hold it before the blit:
/// the bloom by its CPU law (`aterm_render::bloom::apply` at the page's
/// strength) and the shimmer ([`shimmer_twin`]), each unless the mutant drops
/// it, and the card ([`tray_twin`]). The CPU's bloom law takes the flat glow
/// quads, so a bloom over ramped or fx light has no twin and fails here.
fn layered_source(
    gpu: &mut GpuRenderer,
    input: &RenderInput,
    tray: Option<&TrayQuad<'_>>,
    mutant: Mutant,
) -> Frame {
    let mut src = gpu.cpu.render_input(input);
    if mutant != Mutant::NoBloom && gpu.enable_bloom && fx_gpu::bloom_source_live(input) {
        assert!(
            input.cursor_glow_add_ramps.items.is_empty()
                && !input.fx_layers.iter().any(|l| l.bloom),
            "no twin draws a bloom over ramped or fx light: give the frame one"
        );
        let params = aterm_render::bloom::BloomParams {
            strength: page_bloom_strength(gpu.bloom_strength, input, gpu.theme.bg),
            radius: gpu.bloom_radius,
        };
        aterm_render::bloom::apply(&mut src, &input.cursor_glow_add, input.rows, params)
            .expect("the CPU's bloom admits the CPU's frame");
    }
    if mutant != Mutant::NoShimmer {
        shimmer_twin(gpu, input, &mut src);
    }
    if let Some(tray) = tray {
        tray_twin(&mut src, tray);
    }
    src
}

/// `shaders/shimmer.metal`'s `fs_shimmer` on the CPU (`shimmer.glsl` and
/// `shimmer.hlsl` are its twins), as the readback's in-place pass and the
/// present draw it into an `Rgba8Unorm` frame where the shimmer is live:
/// each texel of the region (`shimmer_region`) takes the frame as it stood at
/// its centre displaced by the haze's ripple at the pinned phase, read
/// through the arm's linear, clamp-to-edge sampler, and stores it as a byte.
/// The arm shimmers before the glide moves the band, so a shimmer under a
/// glide has no twin and fails here.
fn shimmer_twin(gpu: &GpuRenderer, input: &RenderInput, frame: &mut Frame) {
    use std::f32::consts::TAU;
    if !gpu.shimmer_live(input) {
        return;
    }
    let (fw, fh) = (frame.width, frame.height);
    let Some(r) = gpu.shimmer_region(input, fw as u32, fh as u32) else {
        return;
    };
    assert_eq!(
        input.scroll_frac_px, 0,
        "no twin draws a shimmer under a glide: give the frame one"
    );
    let (cw, ch) = gpu.cell_size();
    let amp = (ch as f32 / 18.0).clamp(0.75, SHIMMER_AMP_PX);
    let period = (ch as f32).max(4.0);
    let rolloff = (cw as f32).max(1.0);
    let phase = gpu.shimmer_phase();
    let still = frame.pixels.clone();
    let texel = |x: usize, y: usize| {
        let p = still[y * fw + x];
        [p >> 16 & 0xff, p >> 8 & 0xff, p & 0xff, 255 - (p >> 24)].map(|v| v as f32 / 255.0)
    };
    let mix = |a: f32, b: f32, t: f32| a + (b - a) * t;
    for y in r.y0 as usize..r.y1 as usize {
        for x in r.x0 as usize..r.x1 as usize {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let xb = ((p[0] - r.band_x0) / r.band_w.max(1e-3) - 0.5).clamp(0.0, 63.0);
            let i0 = xb.floor() as usize;
            let heat = mix(r.heat[i0], r.heat[(i0 + 1).min(63)], xb.fract());
            let a = ((r.hot_top - p[1]) / r.rise.max(1.0)).clamp(0.0, 1.0);
            let vfade = 1.0 - a * a * (3.0 - 2.0 * a);
            let xr = (p[0] - r.x0 as f32).min(r.x1 as f32 - p[0]);
            let e = (xr / rolloff.max(1.0)).clamp(0.0, 1.0);
            let xfade = e * e * (3.0 - 2.0 * e);
            let ky = TAU / period.max(4.0);
            let col = p[0] * ky;
            let w1 = (ky * p[1]
                + TAU * (0.55 * phase)
                + 1.7 * (0.37 * col + TAU * (0.09 * phase)).sin())
            .sin();
            let w2 = (1.93 * ky * p[1] + TAU * (0.83 * phase) + 0.61 * col).sin();
            let wx = (1.31 * ky * p[1] + TAU * (0.70 * phase) + 1.11 * col).sin();
            let env = amp * heat.clamp(0.0, 1.0) * vfade * xfade;
            let mut d = [0.30 * wx * env, 0.94 * (0.62 * w1 + 0.38 * w2) * env];
            let m = d[0].hypot(d[1]);
            if m > amp {
                d = d.map(|v| v * (amp / m));
            }
            // The sampler's texel space: centres on the half pixel, clamped
            // to the frame's interior, then to its edge texels.
            let sx = (p[0] + d[0]).clamp(0.5, fw as f32 - 0.5) - 0.5;
            let sy = (p[1] + d[1]).clamp(0.5, fh as f32 - 0.5) - 0.5;
            let (x0, y0) = (sx.floor() as usize, sy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(fw - 1), (y0 + 1).min(fh - 1));
            let (tx, ty) = (sx.fract(), sy.fract());
            let (t00, t10, t01, t11) = (texel(x0, y0), texel(x1, y0), texel(x0, y1), texel(x1, y1));
            let c = [0, 1, 2, 3].map(|k| {
                let v = mix(mix(t00[k], t10[k], tx), mix(t01[k], t11[k], tx), ty);
                (v.clamp(0.0, 1.0) * 255.0).round() as u32
            });
            frame.pixels[y * fw + x] = ((255 - c[3]) << 24) | (c[0] << 16) | (c[1] << 8) | c[2];
        }
    }
}

/// The twin of a present of `input` into a `dest` destination read back as
/// RGBA: [`layered_source`] through [`blit_twin`] under [`twin_uniform`],
/// then the SDR crown by its CPU law (`aterm_render::bloom::sdr_crown`) at
/// the budget the present eased to, drawn at the frame's offset in the
/// destination and cut to its visible rows there, as the arm's crown pass is
/// scissored; drawn otherwise where `mutant` says. The arm draws a crown
/// under a glide at the glide's offset, so that crown has no twin and fails
/// here.
#[allow(
    clippy::too_many_arguments,
    reason = "the present's own tuple (input, effects, crop, destination, card) plus the twin's window and mutant"
)]
fn present_twin(
    gpu: &mut GpuRenderer,
    win: &WindowGpu,
    input: &RenderInput,
    effects: (bool, Option<DropOverlay>),
    crop: Option<PresentCrop>,
    dest: (u32, u32),
    tray: Option<&TrayQuad<'_>>,
    mutant: Mutant,
) -> Drawn {
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    let size = (fw as u32, fh as u32);
    let src = layered_source(gpu, input, tray, mutant);
    let mut u = twin_uniform(gpu, input, effects, crop, size, dest);
    if mutant == Mutant::WideBorder {
        u.border_px += 2.0;
    }
    let texels = blit_twin(&src.rgba_bytes(), size, &u, dest);
    let (dw, dh) = (dest.0 as usize, dest.1 as usize);
    if !fx_gpu::crown_glow_present(input) || win.sdr_glow_level <= 0.0 || mutant == Mutant::NoCrown
    {
        return Drawn::twin(dw, dh, &texels, Dest::Rgba8);
    }
    assert_eq!(
        input.scroll_frac_px, 0,
        "no twin draws a crown under a glide: give the frame one"
    );
    // The crown blends into the destination's bytes. It draws in the
    // frame's space, so run its law over a frame-sized canvas holding the
    // destination texels each visible frame texel lands on, and put those
    // back.
    let mut out: Vec<u32> = texels
        .iter()
        .map(|px| {
            let [r, g, b, a] = px.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u32);
            ((255 - a) << 24) | (r << 16) | (g << 8) | b
        })
        .collect();
    let visible = u.visible_y as usize..(u.visible_y + u.visible_h) as usize;
    let (ox, oy) = (u.content_off[0] as i64, u.content_off[1] as i64);
    let landing: Vec<Option<usize>> = (0..fw * fh)
        .map(|i| {
            let (x, y) = ((i % fw) as i64 + ox, (i / fw) as i64 + oy);
            (visible.contains(&(i / fw))
                && (0..dw as i64).contains(&x)
                && (0..dh as i64).contains(&y))
            .then(|| y as usize * dw + x as usize)
        })
        .collect();
    let mut canvas = Frame {
        width: fw,
        height: fh,
        pixels: landing.iter().map(|at| at.map_or(0, |i| out[i])).collect(),
    };
    aterm_render::bloom::sdr_crown(
        &mut canvas,
        &input.cursor_glow_add,
        input.rows,
        win.sdr_glow_level,
    )
    .expect("the CPU's crown admits a frame-sized canvas");
    for (at, px) in landing.iter().zip(&canvas.pixels) {
        if let Some(i) = at {
            out[*i] = *px;
        }
    }
    Frame {
        width: dw,
        height: dh,
        pixels: out,
    }
    .into()
}

/// The card a present composites over its frame (`shaders/tray.metal`
/// through `ALPHA_BLENDING` into the present copy's `Rgba8Unorm`): each
/// straight-alpha texel source-over the frame's, stored as a byte.
fn tray_twin(frame: &mut Frame, tray: &TrayQuad<'_>) {
    let unit = |v: u32| v as f32 / 255.0;
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    for j in 0..tray.ph as usize {
        for i in 0..tray.pw as usize {
            let (x, y) = (tray.dx as usize + i, tray.dy as usize + j);
            if x >= frame.width || y >= frame.height {
                continue;
            }
            let at = (j * tray.pw as usize + i) * 4;
            let c = [0, 1, 2, 3].map(|k| unit(u32::from(tray.rgba[at + k])));
            let p = frame.pixels[y * frame.width + x];
            let d = [p >> 16 & 0xff, p >> 8 & 0xff, p & 0xff, 255 - (p >> 24)].map(unit);
            let rgb = [0, 1, 2].map(|k| byte(c[k] * c[3] + d[k] * (1.0 - c[3])));
            let a = byte(c[3] + d[3] * (1.0 - c[3]));
            frame.pixels[y * frame.width + x] =
                ((255 - a) << 24) | (rgb[0] << 16) | (rgb[1] << 8) | rgb[2];
        }
    }
}

/// macOS's window layer: the present into a letterboxed BGRA destination as
/// the `CAMetalLayer` takes it, opaque and translucent (its alpha below 255
/// where the window shows through). The record took each layer's RGBA
/// readback with R and B exchanged, so its frames hold them so, and their
/// twins too.
#[cfg(target_os = "macos")]
fn window_layer_frames(d: &mut Drawing, family: Option<&str>) {
    let input = fixtures_base_for_cursor();
    for (label, opacity) in [("opaque", 1.0f32), ("translucent", 0.55)] {
        let translucent = opacity < 1.0;
        let mut gpu = record_renderer_with(family);
        gpu.set_bloom(false);
        gpu.set_shimmer(false);
        gpu.set_background_opacity(opacity);
        let mut win = WindowGpu::new();
        let (fw, fh) = gpu.frame_size(input.rows, input.cols);
        let (dw, dh) = (fw as u32 + 9, fh as u32 + 7);
        let rgba = gpu
            .metal_present_bytes_for_test(&mut win, &input, (dw, dh), translucent)
            .unwrap_or_else(|e| panic!("{label}: the layer present: {e}"));
        assert!(
            gpu.metal_render_armed(),
            "{label}: the layer present drew on Metal"
        );
        let sub255 = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px[3] < 255)
            .count();
        assert_eq!(
            sub255 > 0,
            translucent,
            "{label}: only the translucent layer shows through ({sub255} texels below 255)"
        );
        let frame = Frame {
            width: dw as usize,
            height: dh as usize,
            pixels: rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&[r, g, b, a]| {
                    ((255 - u32::from(a)) << 24)
                        | (u32::from(b) << 16)
                        | (u32::from(g) << 8)
                        | u32::from(r)
                })
                .collect(),
        };
        let src = layered_source(&mut gpu, &input, None, Mutant::Law);
        let size = (src.width as u32, src.height as u32);
        let mut u = twin_uniform(&gpu, &input, (false, None), None, size, (dw, dh));
        if translucent {
            u.translucent = 1.0;
            u.band[3] = gpu.backdrop_margin_alpha();
        }
        let texels = blit_twin(&src.rgba_bytes(), size, &u, (dw, dh));
        let twin = Drawn::twin(dw as usize, dh as usize, &texels, Dest::Bgra8);
        d.push(format!("window/{label}/layer"), frame.into(), twin);
    }
    d.distinct.push((
        "window/opaque/layer".to_owned(),
        "window/translucent/layer".to_owned(),
    ));
}

/// The video tap: a take of three headless presents (a still frame, a glide
/// frame, a changed row) through the arm's capture ring, each frame as the
/// take holds it.
fn video_take_frames(d: &mut Drawing, family: Option<&str>) {
    let mut gpu = record_renderer_with(family);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let (_, scroll, _) = fixtures(&gpu, true)
        .into_iter()
        .find(|(name, ..)| *name == "scroll")
        .expect("the scroll fixture");
    let mut still = scroll.clone();
    still.scroll_frac_px = 0;
    let mut changed = still.clone();
    changed.row_rev_lane = 0;
    changed.cells[2][1].ch = '#';
    let (fw, fh) = gpu.frame_size(still.rows, still.cols);
    let opts = crate::video_tap::CaptureOpts {
        half_res: false,
        budget_bytes: 64 * 1024 * 1024,
        fps_cap: None,
        requested_ms: 1000,
    };
    let mut win = WindowGpu::new();
    gpu.virtual_begin(&mut win, fw as u32, fh as u32, opts)
        .unwrap_or_else(|e| panic!("video: the take starts: {e}"));
    let times = [10, 20, 30];
    let inputs = [&still, &scroll, &changed];
    for (t_us, input) in times.into_iter().zip(inputs) {
        assert!(
            gpu.present_virtual(&mut win, input, false, None, None),
            "video: the present at {t_us} us refused (see the arm's note)"
        );
        drew_on(&gpu, "video");
        gpu.video_after_present(&mut win, t_us);
    }
    let take = gpu
        .video_finish(&mut win)
        .expect("a take was live")
        .settle();
    crate::stderr_line!("frame-record: the video take's format is {}", take.format);
    assert_eq!(
        (take.dropped, take.frames.len()),
        (0, 3),
        "video: the take holds every present"
    );
    // A take of an 8-bit sRGB destination holds its texels as RGBA8; a
    // converted one (Display P3, scRGB) has no twin here.
    let eight_bit = matches!(take.format, "bgra8" | "rgba8");
    for (i, ((f, t_us), input)) in take.frames.iter().zip(times).zip(inputs).enumerate() {
        assert_eq!(f.t_us, t_us, "video: frame {i}'s time");
        let name = format!("video/take/{i}");
        let drawn = Drawn {
            width: f.w as usize,
            height: f.h as usize,
            texel: Texel::Unorm8,
            bytes: f.rgba.to_vec(),
        };
        if !eight_bit {
            let why = format!(
                "the take holds it as {}, which no twin converts",
                take.format
            );
            d.push_untwinned(name, drawn, why);
            continue;
        }
        let src = layered_source(&mut gpu, input, None, Mutant::Law);
        let size = (src.width as u32, src.height as u32);
        let u = twin_uniform(&gpu, input, (false, None), None, size, (f.w, f.h));
        let texels = blit_twin(&src.rgba_bytes(), size, &u, (f.w, f.h));
        let twin = Drawn::twin(f.w as usize, f.h as usize, &texels, Dest::Rgba8);
        d.push(name, drawn, twin);
    }
    d.distinct
        .push(("video/take/0".to_owned(), "video/take/2".to_owned()));
}

/// The EDR present's frame: white text for the reference-white scale to lift
/// above 1.0, and a row of additive aurora quads for the crown.
#[cfg(any(target_os = "macos", windows))]
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

/// The EDR present into an `Rgba16Float` destination (the blit's linear
/// decode at the scRGB reference-white scale, the letterbox bands, the aurora
/// crown above reference white under its headroom), plain and with the crown,
/// at the exact fit and with bands. Controls: the crown changes the frame and
/// lifts a channel above 1.0, and the scale lifts the white text above 1.0
/// without it.
#[cfg(any(target_os = "macos", windows))]
fn edr_present_frames(d: &mut Drawing, family: Option<&str>) {
    let mut gpu = record_renderer_with(family);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    gpu.set_hdr_glow(true);
    let input = edr_input(&gpu);
    let mut win = WindowGpu::new();
    win.set_edr_max(2.0);
    win.set_sdr_white_scale(2.5);
    let over = |px: &[f32]| {
        px.iter()
            .enumerate()
            .filter(|(i, v)| i % 4 != 3 && **v > 1.0)
            .count()
    };
    let mut lifted = Vec::new();
    for (label, boost, extra) in [("plain", false, 0), ("crown", true, 0), ("banded", true, 7)] {
        let (px, w, h) = edr_present(&mut gpu, &mut win, &input, boost, extra);
        drew_on(&gpu, label);
        lifted.push(over(&px));
        let twin = edr_twin(&mut gpu, &win, &input, boost, (w, h));
        d.push(
            format!("edr/{label}"),
            Drawn {
                width: w as usize,
                height: h as usize,
                texel: Texel::Linear,
                bytes: px.iter().flat_map(|v| v.to_le_bytes()).collect(),
            },
            twin,
        );
    }
    d.distinct
        .push(("edr/plain".to_owned(), "edr/crown".to_owned()));
    assert!(
        lifted[0] > 0,
        "the reference-white scale lifts the white text above 1.0"
    );
    assert!(
        lifted[1] > lifted[0],
        "the crown emits above reference white"
    );
}

/// The HDR crown's fragment (`fs_hdr_glow`) on the CPU: a premultiplied
/// sRGB light decoded to linear, boosted where it is white (by up to 3x
/// past the 0.35 knee) and clamped to the headroom, as it adds into an
/// `Rgba16Float` destination.
#[cfg(any(target_os = "macos", windows))]
fn hdr_crown_add(rgb: [f32; 3], boost: f32, headroom: f32) -> [f32; 3] {
    let c = rgb.map(|v| v.clamp(0.0, 1.0));
    let t = ((c[0].min(c[1]).min(c[2]) - 0.35) / (0.60 - 0.35)).clamp(0.0, 1.0);
    let hot = (1.0 + 2.0 * t * t * (3.0 - 2.0 * t)).min(3.0);
    c.map(|v| {
        aterm_render::hdr::clamp_add(
            aterm_render::hdr::srgb_channel_to_linear(v) * boost * hot,
            headroom,
        )
    })
}

/// The twin of an EDR present of `input` into a `dest` `Rgba16Float`
/// destination: the CPU's frame through [`blit_twin`] with the linear
/// encode at the window's reference white, and where `boost` the HDR crown
/// ([`hdr_crown_add`]) over each glow quad's light, ramped across its width,
/// inside the frame.
#[cfg(any(target_os = "macos", windows))]
fn edr_twin(
    gpu: &mut GpuRenderer,
    win: &WindowGpu,
    input: &RenderInput,
    boost: bool,
    dest: (u32, u32),
) -> Drawn {
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    let size = (fw as u32, fh as u32);
    let src = gpu.cpu.render_input(input);
    let scale = win.sdr_white_scale.max(1.0);
    let mut u = twin_uniform(gpu, input, (false, None), None, size, dest);
    u.hdr = 1.0;
    u.sdr_white_scale = scale;
    let mut texels = blit_twin(&src.rgba_bytes(), size, &u, dest);
    let headroom = aterm_render::hdr::additive_headroom(win.edr_max);
    if boost && headroom > 0.0 {
        let (dw, ox, oy) = (
            dest.0 as usize,
            u.content_off[0] as usize,
            u.content_off[1] as usize,
        );
        for q in input
            .cursor_glow_add
            .iter()
            .filter(|q| usize::from(q.row) < input.rows)
        {
            let (qx, qy) = (usize::from(q.x), usize::from(q.y));
            for x in qx..(qx + usize::from(q.w)).min(fw) {
                let c =
                    aterm_render::glow_lerp_rgb(q.color, q.color2, (x - qx) as u32, u32::from(q.w));
                let rgb = [c >> 16 & 0xff, c >> 8 & 0xff, c & 0xff].map(|v| v as f32 / 255.0);
                let add = hdr_crown_add(
                    rgb,
                    aterm_render::hdr::HDR_GLOW_BOOST * scale,
                    headroom * scale,
                );
                for y in qy..(qy + usize::from(q.h)).min(fh) {
                    let px = &mut texels[(y + oy) * dw + x + ox];
                    for k in 0..3 {
                        px[k] += add[k];
                    }
                }
            }
        }
    }
    Drawn::twin(dest.0 as usize, dest.1 as usize, &texels, Dest::Half)
}

/// The EDR present on Windows: the D3D11 arm's real present into an f16
/// destination.
#[cfg(windows)]
fn edr_present(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    input: &RenderInput,
    boost: bool,
    extra: u32,
) -> (Vec<f32>, u32, u32) {
    gpu.present_hdr_sized_for_test(win, input, boost, extra, extra)
}

/// The EDR present on macOS: the Metal arm's production present into an
/// `Rgba16Float` drawable (the crown runs where the HDR glow is on).
#[cfg(target_os = "macos")]
fn edr_present(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    input: &RenderInput,
    boost: bool,
    extra: u32,
) -> (Vec<f32>, u32, u32) {
    gpu.set_hdr_glow(boost);
    let (fw, fh) = gpu.frame_size(input.rows, input.cols);
    let (dw, dh) = (fw as u32 + extra, fh as u32 + extra);
    let px = gpu
        .metal_present_standalone_for_test(
            win,
            input,
            (dw, dh),
            (false, true),
            crate::device_layer::TexelFormat::Rgba16Float,
            |gpu, target| {
                let live = gpu
                    .metal_arm
                    .as_mut()
                    .ok_or("the arm is not live")?
                    .live_mut();
                let row = dw as usize * 8;
                let rb = live
                    .layer()
                    .try_create_readback_buffer(
                        "aterm-gpu edr readback",
                        (row * dh as usize) as u64,
                    )?
                    .into_metal()
                    .into_obj();
                let mut cb = live.session.begin()?;
                cb.copy_texture_to_buffer(target, dw as usize, dh as usize, &rb, row)?;
                if cb.commit().wait_outcome() != crate::metal::loss::CbOutcome::Completed {
                    return Err("the readback copy did not complete".to_owned());
                }
                // SAFETY: shared storage sized `row * dh` whose writing command buffer has
                // completed.
                let halves = unsafe { crate::metal::ffi::buffer_bytes(&rb, row * dh as usize) };
                Ok(halves
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&b| f16_bits_to_f32(u16::from_le_bytes(b)))
                    .collect::<Vec<f32>>())
            },
        )
        .unwrap_or_else(|e| panic!("the Metal EDR present: {e}"));
    gpu.set_hdr_glow(true);
    (px, dw, dh)
}

/// One case of the Metal present blit: its destination, its uniform's arms,
/// its source, and how far the destination outgrows the source (the bands).
#[cfg(target_os = "macos")]
struct BlitCase {
    name: &'static str,
    target: crate::metal::ffi::PixelFormat,
    invert: bool,
    overlay: Option<DropOverlay>,
    crop: Option<PresentCrop>,
    encode_srgb: bool,
    hdr: bool,
    sdr_white_scale: f32,
    /// The translucent arm's band alpha, and whether the output is
    /// premultiplied.
    translucent: Option<(f32, bool)>,
    extra: (u32, u32),
    /// `blit_sources`' label of the frame the blit reads.
    source: &'static str,
}

#[cfg(target_os = "macos")]
impl BlitCase {
    const PLAIN: Self = Self {
        name: "",
        target: crate::metal::ffi::PixelFormat::Rgba8Unorm,
        invert: false,
        overlay: None,
        crop: None,
        encode_srgb: false,
        hdr: false,
        sdr_white_scale: 1.0,
        translucent: None,
        extra: (0, 0),
        source: "opaque",
    };
}

/// The chrome rows the blit's sources carry: `first` is
/// `ChromeBleed::first`, so the uniform carries `chrome_y1` (and `chrome_y0`
/// past a surfaceless strip) and the bands beside chrome rows continue their
/// edges.
#[cfg(target_os = "macos")]
const fn blit_chrome(first: usize) -> aterm_render::ChromeBleed {
    aterm_render::ChromeBleed {
        rows: 2,
        first,
        color: 0x0030_3135,
        seam: Some(0x0060_6164),
        top_extends_cells: false,
        row_edges: [
            Some(aterm_render::ChromeRowEdges {
                row: 1,
                left: 0x0050_FA7B,
                right: 0x0044_4750,
            }),
            None,
            None,
        ],
    }
}

/// The blit's sources: `(label, background opacity, chrome)`.
#[cfg(target_os = "macos")]
const BLIT_SOURCES: [(&str, f32, Option<usize>); 4] = [
    ("opaque", 1.0, None),
    ("glass", 0.55, None),
    ("chrome", 1.0, Some(0)),
    ("strip", 1.0, Some(1)),
];

/// The blit's frame: a prompt, glyphs and saturated SGR runs.
#[cfg(target_os = "macos")]
fn blit_input() -> RenderInput {
    use aterm_core::terminal::Terminal;
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ blit check >_\r\n");
    term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[34mBLUE\x1b[0m\r\n");
    term.process(b"\x1b[1mbold\x1b[0m plain 0123456789");
    term.cell_frame(ROWS, COLS)
}

/// The blit uniform a case hands the arm, over a `sw` x `sh` source into a
/// `dw` x `dh` destination: the arm's present uniform (`present_blit_uniform`)
/// with the chrome extent the source frame set, then the case's arms.
#[cfg(target_os = "macos")]
fn blit_uniform(
    gpu: &GpuRenderer,
    win: &WindowGpu,
    c: &BlitCase,
    (sw, sh): (u32, u32),
    (dw, dh): (u32, u32),
) -> BlitUniform {
    let mut want = present_blit_uniform(c.invert, c.overlay, c.crop, sw, sh, dw, dh, gpu.theme.bg);
    want.chrome_y0 = win.chrome_y0 as f32;
    want.chrome_y1 = win.chrome_y1 as f32;
    if let Some((band_a, premult)) = c.translucent {
        want = want.with_translucency(band_a);
        if premult {
            want = want.with_premultiplied_output();
        }
    }
    want.encode_srgb = if c.encode_srgb || !gpu.ctx.srgb_offscreen {
        1.0
    } else {
        0.0
    };
    want.hdr = f32::from(u8::from(c.hdr));
    want.sdr_white_scale = c.sdr_white_scale;
    want
}

/// The uniform a case's twin reads: [`twin_uniform`] (the CPU's laws, not
/// the arm's builder) under the case's arms.
#[cfg(target_os = "macos")]
fn blit_twin_uniform(
    gpu: &GpuRenderer,
    input: &RenderInput,
    c: &BlitCase,
    source: (u32, u32),
    dest: (u32, u32),
) -> BlitUniform {
    let mut u = twin_uniform(gpu, input, (c.invert, c.overlay), c.crop, source, dest);
    if let Some((band_a, premult)) = c.translucent {
        u.translucent = 1.0;
        u.band[3] = band_a;
        u.premult = f32::from(u8::from(premult));
    }
    u.encode_srgb = f32::from(u8::from(c.encode_srgb || !gpu.ctx.srgb_offscreen));
    u.hdr = f32::from(u8::from(c.hdr));
    u.sdr_white_scale = c.sdr_white_scale;
    u
}

/// The Metal present blit (`shaders/blit.metal`, `Pipeline::Blit`) over
/// every destination format and uniform arm the present takes: the exact
/// fit, the bell inversion, the bands, the crop and drop overlay (RGBA and
/// the presented BGRA), the linear-to-sRGB re-encode, translucent and
/// premultiplied glass, the EDR clamp into `Rgba16Float` and the scRGB
/// reference white with bands, and the chrome rows' edges through the bands
/// (under the inversion, and below a surfaceless strip). Each source frame is
/// a row of its own; each blit's row is its destination's texels.
#[cfg(target_os = "macos")]
fn blit_frames(d: &mut Drawing, family: Option<&str>) {
    use crate::metal::ffi::PixelFormat;
    let mut gpu = record_renderer_with(family);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut win = WindowGpu::new();
    let input = blit_input();
    let source = |gpu: &mut GpuRenderer, win: &mut WindowGpu, label: &str| {
        let &(_, opacity, chrome) = BLIT_SOURCES
            .iter()
            .find(|(l, ..)| *l == label)
            .expect("a named source");
        gpu.set_background_opacity(opacity);
        gpu.set_chrome_bleed(chrome.map(blit_chrome));
        let f = gpu
            .try_render_input(win, &input, None)
            .unwrap_or_else(|e| panic!("blit/source/{label}: the readback: {e}"));
        drew_on(gpu, label);
        f
    };
    let mut size = (0, 0);
    for (label, opacity, _) in BLIT_SOURCES {
        let f: Drawn = source(&mut gpu, &mut win, label).into();
        let see_through = f.bytes.as_chunks::<4>().0.iter().any(|c| c[3] != 255);
        assert_eq!(
            see_through,
            opacity < 1.0,
            "blit/source/{label}: only the glass source shows through, so the translucent arm \
             is not vacuous"
        );
        size = (f.width as u32, f.height as u32);
        let twin = cpu_twin(&mut gpu, &input);
        d.push(format!("blit/source/{label}"), f, twin);
    }
    let (sw, sh) = size;
    let overlay = DropOverlay {
        accent: 0x0033_88ff,
        wash_a: 40,
        border_a: 200,
        border_scale_q4: 0,
    };
    let crop = PresentCrop {
        source_y: 3,
        height: sh - 3,
    };
    let bands = (37, 23);
    let p = BlitCase::PLAIN;
    let cases = [
        BlitCase { name: "exact", ..p },
        BlitCase {
            name: "exact-invert",
            invert: true,
            ..p
        },
        BlitCase {
            name: "bands",
            extra: bands,
            ..p
        },
        BlitCase {
            name: "cropped-overlay",
            overlay: Some(overlay),
            crop: Some(crop),
            extra: bands,
            ..p
        },
        BlitCase {
            name: "cropped-overlay-bgra",
            target: PixelFormat::Bgra8Unorm,
            overlay: Some(overlay),
            crop: Some(crop),
            extra: bands,
            ..p
        },
        BlitCase {
            name: "encode-srgb",
            encode_srgb: true,
            ..p
        },
        BlitCase {
            name: "glass-bands",
            target: PixelFormat::Bgra8Unorm,
            translucent: Some((0.55, false)),
            extra: bands,
            source: "glass",
            ..p
        },
        BlitCase {
            name: "glass-premultiplied",
            target: PixelFormat::Bgra8Unorm,
            translucent: Some((0.55, true)),
            extra: bands,
            source: "glass",
            ..p
        },
        BlitCase {
            name: "edr-clamp",
            target: PixelFormat::Rgba16Float,
            hdr: true,
            ..p
        },
        BlitCase {
            name: "chrome-bands",
            target: PixelFormat::Bgra8Unorm,
            extra: bands,
            source: "chrome",
            ..p
        },
        BlitCase {
            name: "chrome-invert",
            invert: true,
            crop: Some(crop),
            extra: bands,
            source: "chrome",
            ..p
        },
        BlitCase {
            name: "chrome-below-strip",
            extra: bands,
            source: "strip",
            ..p
        },
        BlitCase {
            name: "edr-scrgb-bands",
            target: PixelFormat::Rgba16Float,
            hdr: true,
            sdr_white_scale: 3.0,
            extra: bands,
            ..p
        },
    ];
    for c in &cases {
        let src = source(&mut gpu, &mut win, c.source);
        assert_eq!((src.width as u32, src.height as u32), (sw, sh));
        let (dw, dh) = (sw + c.extra.0, sh + c.extra.1);
        let uniform = blit_uniform(&gpu, &win, c, (sw, sh), (dw, dh));
        let bytes = crate::metal::blit::MetalBlit::new(c.target)
            .and_then(|b| {
                b.run(
                    &src.rgba_bytes(),
                    sw,
                    sh,
                    aterm_bits::bytes_of(&uniform),
                    dw,
                    dh,
                )
            })
            .unwrap_or_else(|e| panic!("blit/{}: the Metal blit: {e}", c.name));
        let (texel, dest) = match c.target {
            PixelFormat::Rgba16Float => (Texel::Half, Dest::Half),
            PixelFormat::Bgra8Unorm => (Texel::Unorm8, Dest::Bgra8),
            _ => (Texel::Unorm8, Dest::Rgba8),
        };
        let twin = |mutant| {
            let mut u = blit_twin_uniform(&gpu, &input, c, (sw, sh), (dw, dh));
            if mutant == Mutant::WideBorder {
                u.border_px += 2.0;
            }
            let texels = blit_twin(&src.rgba_bytes(), (sw, sh), &u, (dw, dh));
            Drawn::twin(dw as usize, dh as usize, &texels, dest)
        };
        let name = format!("blit/{}", c.name);
        if c.overlay.is_some() {
            d.mutants
                .push((name.clone(), Mutant::WideBorder, twin(Mutant::WideBorder)));
        }
        d.push(
            name,
            Drawn {
                width: dw as usize,
                height: dh as usize,
                texel,
                bytes,
            },
            twin(Mutant::Law),
        );
    }
    gpu.set_background_opacity(1.0);
    gpu.set_chrome_bleed(None);
    for (a, b) in [
        ("exact", "exact-invert"),
        ("exact", "encode-srgb"),
        ("glass-bands", "glass-premultiplied"),
        ("bands", "chrome-below-strip"),
    ] {
        d.distinct.push((format!("blit/{a}"), format!("blit/{b}")));
    }
}

/// The crown rows' fixture: a 20 x 14 target seeded with known texels, three
/// instances (one straddling the scissor), the content offset armed.
#[cfg(target_os = "macos")]
const CROWN_SIZE: (u32, u32) = (20, 14);
#[cfg(target_os = "macos")]
const CROWN_OFF: [f32; 2] = [3.0, 2.0];
#[cfg(target_os = "macos")]
const CROWN_SCISSOR: [u32; 4] = [3, 2, 17, 12];
#[cfg(target_os = "macos")]
const CROWN_INSTANCES: [([u16; 4], [u8; 4]); 3] = [
    ([1, 1, 8, 6], [240, 160, 80, 255]),
    ([5, 3, 6, 5], [9, 6, 3, 255]),
    ([10, 6, 9, 7], [90, 200, 250, 255]),
];

/// The crown rows on their own targets: `HDR_GLOW` additive into
/// `Rgba16Float` (boost 4, headroom 1.2) and `SDR_GLOW`'s screen blend into
/// `Bgra8Unorm` (headroom 0.35), each a scissored `Load` over seeded texels.
/// Controls: each crown changes its seed.
#[cfg(target_os = "macos")]
fn crown_row_frames(d: &mut Drawing) {
    let (w, h) = CROWN_SIZE;
    const HALVES: [u16; 4] = [0x0000, 0x3400, 0x3800, 0x3c00];
    let hdr_seed: Vec<u8> = (0..(w * h * 4) as usize)
        .flat_map(|i| HALVES[i % 4].to_le_bytes())
        .collect();
    let sdr_seed: Vec<u8> = (0..(w * h * 4) as usize)
        .map(|i| ((i * 5) % 180) as u8)
        .collect();
    for (label, hdr, boost, headroom, seed) in [
        ("hdr", true, 4.0, 1.2, &hdr_seed),
        ("sdr", false, 1.0, 0.35, &sdr_seed),
    ] {
        let bytes = crate::metal::blit::crown_row_bytes(
            hdr,
            &CROWN_INSTANCES,
            CROWN_OFF,
            boost,
            headroom,
            seed,
            CROWN_SCISSOR,
            (w, h),
        )
        .unwrap_or_else(|e| panic!("crown/{label}: the Metal row: {e}"));
        assert_ne!(&bytes, seed, "crown/{label}: the crown adds light");
        let twin = crown_row_twin(hdr, boost, headroom, seed, (w, h));
        d.push(
            format!("crown/{label}"),
            Drawn {
                width: w as usize,
                height: h as usize,
                texel: if hdr { Texel::Half } else { Texel::Unorm8 },
                bytes,
            },
            twin,
        );
    }
}

/// The twin of a crown row ([`crown_row_frames`]): each instance's light,
/// offset by [`CROWN_OFF`] and cut to [`CROWN_SCISSOR`], into the seeded
/// target in drawing order: `HDR_GLOW`'s [`hdr_crown_add`] added into
/// `Rgba16Float`, or `SDR_GLOW`'s light scaled by the headroom screened
/// over `Bgra8Unorm` (`src + dst * (1 - src)`), stored as a byte after
/// each instance.
#[cfg(target_os = "macos")]
fn crown_row_twin(hdr: bool, boost: f32, headroom: f32, seed: &[u8], (w, h): (u32, u32)) -> Drawn {
    let (w, h) = (w as usize, h as usize);
    let mut texels: Vec<[f32; 4]> = if hdr {
        seed.as_chunks::<8>()
            .0
            .iter()
            .map(|t| {
                let half = |k: usize| f16_bits_to_f32(u16::from_le_bytes([t[2 * k], t[2 * k + 1]]));
                [half(0), half(1), half(2), half(3)]
            })
            .collect()
    } else {
        seed.as_chunks::<4>()
            .0
            .iter()
            .map(|&[b, g, r, a]| [r, g, b, a].map(|v| f32::from(v) / 255.0))
            .collect()
    };
    let [sx0, sy0, sx1, sy1] = CROWN_SCISSOR.map(|v| v as usize);
    let [ox, oy] = CROWN_OFF.map(|v| v as usize);
    for ([x, y, qw, qh], colour) in CROWN_INSTANCES {
        let rgb = [colour[0], colour[1], colour[2]].map(|v| f32::from(v) / 255.0);
        let (x0, y0) = (usize::from(x) + ox, usize::from(y) + oy);
        for py in y0.max(sy0)..(y0 + usize::from(qh)).min(sy1).min(h) {
            for px in x0.max(sx0)..(x0 + usize::from(qw)).min(sx1).min(w) {
                let t = &mut texels[py * w + px];
                if hdr {
                    let add = hdr_crown_add(rgb, boost, headroom);
                    for k in 0..3 {
                        t[k] += add[k];
                    }
                } else {
                    let src = rgb.map(|v| v * headroom.max(0.0) * boost.max(0.0));
                    for k in 0..3 {
                        let v = src[k] + t[k] * (1.0 - src[k]);
                        t[k] = (v.clamp(0.0, 1.0) * 255.0).round() / 255.0;
                    }
                }
            }
        }
    }
    Drawn::twin(w, h, &texels, if hdr { Dest::Half } else { Dest::Bgra8 })
}

/// `shaders/blit.metal`'s `fs_blit` on the CPU, the twin of each arm's
/// present blit (`blit.glsl` and `blit.hlsl` are its twins): every texel of a
/// `dw` x `dh` destination drawn from `src` (`sw` x `sh` RGBA8, read as Unorm
/// as the present's offscreen and `MetalBlit`'s source hold it) under `u`, as
/// the RGBA the shader returns before the destination stores it.
fn blit_twin(
    src: &[u8],
    (sw, sh): (u32, u32),
    u: &BlitUniform,
    (dw, dh): (u32, u32),
) -> Vec<[f32; 4]> {
    assert_eq!(src.len(), sw as usize * sh as usize * 4, "an RGBA8 source");
    let l2s = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    };
    // `hdr_grid_encode3`: the grid's sRGB-to-linear decode, clamped.
    let grid = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let v = if c > 0.040_45 {
            ((c + 0.055) / 1.055).powf(2.4)
        } else {
            c / 12.92
        };
        v.clamp(0.0, 1.0)
    };
    let visible_y1 = u.visible_y + u.visible_h;
    let mut out = Vec::with_capacity(dw as usize * dh as usize);
    for y in 0..dh {
        for x in 0..dw {
            // The fragment's centre less the frame's offset: whole source
            // texels, read 1:1.
            let mut p = [
                x as f32 + 0.5 - u.content_off[0],
                y as f32 + 0.5 - u.content_off[1],
            ];
            let mut chrome_edge = false;
            if (p[0] < 0.0 || p[0] >= u.dims[0])
                && p[1] >= u.visible_y.max(u.chrome_y0)
                && p[1] < visible_y1.min(u.chrome_y1)
            {
                p[0] = p[0].clamp(0.0, u.dims[0] - 1.0);
                chrome_edge = true;
            }
            if p[0] < 0.0 || p[1] < u.visible_y || p[0] >= u.dims[0] || p[1] >= visible_y1 {
                let a = if u.translucent == 0.0 { 1.0 } else { u.band[3] };
                let mut rgb = [u.band[0], u.band[1], u.band[2]];
                if u.hdr != 0.0 {
                    rgb = rgb.map(|c| grid(c) * u.sdr_white_scale);
                }
                if u.premult != 0.0 {
                    rgb = rgb.map(|c| c * a);
                }
                out.push([rgb[0], rgb[1], rgb[2], a]);
                continue;
            }
            let at = (p[1] as usize * sw as usize + p[0] as usize) * 4;
            let c = [src[at], src[at + 1], src[at + 2], src[at + 3]].map(|b| f32::from(b) / 255.0);
            let mut rgb = [c[0], c[1], c[2]];
            if u.flag != 0 && !chrome_edge {
                rgb = rgb.map(|v| 1.0 - v);
            }
            if u.overlay != 0 && !chrome_edge {
                let (vx, vy) = (p[0], p[1] - u.visible_y);
                let edge = vx.min(u.dims[0] - vx).min(vy.min(u.visible_h - vy));
                let a = if edge < u.border_px {
                    u.border_a
                } else {
                    u.wash_a
                };
                for (v, accent) in rgb.iter_mut().zip(u.accent) {
                    *v += (accent - *v) * a;
                }
            }
            if u.encode_srgb != 0.0 {
                rgb = rgb.map(l2s);
            }
            if u.hdr != 0.0 {
                rgb = rgb.map(|c| grid(c) * u.sdr_white_scale);
            }
            let a = if u.translucent == 0.0 { 1.0 } else { c[3] };
            if u.premult != 0.0 {
                rgb = rgb.map(|v| v * a);
            }
            out.push([rgb[0], rgb[1], rgb[2], a]);
        }
    }
    out
}

/// The twin law's bar, in 8-bit codes ([`Drawn::codes`]), on any texel: the
/// fire halo's (`tests/fire_halo_gpu.rs`), the widest a parity suite holds a
/// GPU frame to against the CPU's.
const TWIN_CEILING: f32 = 24.0;

/// The channel delta, in codes, past which a texel strays: the `Linear`
/// blend's bar (`FxBlend::Linear.parity()`), the widest of the blend laws.
const TWIN_NEAR: f32 = 2.0;

/// How many texels in a thousand (at least one per frame) may stray: an arm
/// and the CPU may round a threshold apart, as the fire fixture's halo does
/// at one texel, 10 codes, on this Mac's M4 Pro.
const TWIN_STRAYS_PER_MILLE: usize = 1;

/// `drawn` against its `twin` under the twin law: its widest channel delta
/// in codes when it holds, else the finding (how many texels stray, the
/// worst and where).
fn twin_law(name: &str, drawn: &Drawn, twin: &Drawn) -> Result<f32, String> {
    let (w, h) = (drawn.width, drawn.height);
    if (w, h) != (twin.width, twin.height) {
        return Err(format!(
            "{name}: drew {w}x{h}, its twin {}x{}",
            twin.width, twin.height
        ));
    }
    let (a, b) = (drawn.codes(), twin.codes());
    if a.len() != w * h * 4 || b.len() != a.len() {
        return Err(format!(
            "{name}: {} channels drawn and {} in its twin for {w}x{h}",
            a.len(),
            b.len()
        ));
    }
    let mut strays = 0usize;
    let mut worst = (0.0f32, 0usize);
    for (i, (pa, pb)) in a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .enumerate()
    {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| {
                let d = (x - y).abs();
                if d.is_nan() { f32::INFINITY } else { d }
            })
            .fold(0.0f32, f32::max);
        if d > TWIN_NEAR {
            strays += 1;
        }
        if d > worst.0 {
            worst = (d, i);
        }
    }
    let allowed = (w * h * TWIN_STRAYS_PER_MILLE).div_ceil(1000);
    if worst.0 <= TWIN_CEILING && strays <= allowed {
        return Ok(worst.0);
    }
    let i = worst.1;
    Err(format!(
        "{name}: {strays} texels more than {TWIN_NEAR} codes off its twin (at most \
             {allowed}), the worst {:.1} codes at ({}, {}): drew {:?}, the twin {:?}",
        worst.0,
        i % w,
        i / w,
        &a[i * 4..i * 4 + 4],
        &b[i * 4..i * 4 + 4]
    ))
}

/// Each frame's row: its name, its geometry and the SHA-256 of its bytes.
fn rows_of(frames: &Named) -> Vec<(String, (String, String))> {
    frames
        .iter()
        .map(|(name, f)| (name.clone(), row_of(f)))
        .collect()
}

/// What the oracle decides for one adapter.
struct Verdict {
    /// How many frames are held to a twin.
    twinned: usize,
    /// The twin law's findings: the verdict, decided alike on every adapter.
    findings: Vec<String>,
    /// The widest channel delta, in codes, of a frame that holds to its
    /// twin, and that frame: how near the arm runs to [`TWIN_CEILING`].
    widest: Option<(f32, String)>,
    /// Where the record holds the adapter, how many rows it holds and the
    /// frames drawn otherwise: the signal that this adapter's frames moved
    /// since wgpu drew them, a change to the arm or to the system, never the
    /// verdict.
    record: Option<(usize, Vec<String>)>,
}

/// Judge `frames`, drawn on `adapter` (`arch`), against their twins and,
/// where the record holds the adapter, its rows.
fn judge(arch: &str, adapter: &str, frames: &Named, twins: &Twins) -> Verdict {
    let mut findings = Vec::new();
    let mut widest: Option<(f32, String)> = None;
    let mut twinned = 0;
    for (name, f) in frames {
        let Some(twin) = twins.get(name) else {
            continue;
        };
        twinned += 1;
        match twin_law(name, f, twin) {
            Ok(d) if widest.as_ref().is_none_or(|(w, _)| d > *w) => {
                widest = Some((d, name.clone()));
            }
            Ok(_) => {}
            Err(finding) => findings.push(finding),
        }
    }
    let recorded = recorded_rows(arch, adapter);
    let record =
        (!recorded.is_empty()).then(|| (recorded.len(), findings_of(&rows_of(frames), &recorded)));
    Verdict {
        twinned,
        findings,
        widest,
        record,
    }
}

/// The identity the negative control judges this build's frames under
/// besides the host's: an adapter the record holds no block for, as a Mac
/// with another GPU presents itself. Before the twins the oracle was COULD
/// NOT RUN on one.
const UNRECORDED_ADAPTER: &str = "Apple M5 Max (injected)";

/// This build's arm against its CPU twins, decided alike on every adapter:
/// each frame under the twin law ([`twin_law`]) against its twin, which
/// every frame has but a video take this adapter holds converted (a
/// `frame-record: untwinned:` line names it and why). Each mutant twin
/// ([`Mutant`]: without the bloom, the shimmer or the crown, a wider drop
/// border)
/// must fail its frame, so the layer it changes shows there past the law's
/// bar; the `distinct` pairs must differ and the `same` pairs be
/// byte-identical. Where it holds this adapter, the record's rows are a
/// signal: a frame drawn otherwise than wgpu drew it is printed, and the
/// verdict line counts them, but a system update moves them too, so they
/// never fail the verdict. A texel planted in one twinned frame must fail that frame's twin
/// and no other on this adapter and on [`UNRECORDED_ADAPTER`], and on D3D11,
/// whose arm has the seam, a byte flipped inside the arm must move its frame
/// off its twin.
#[test]
fn armed_frames_hold_to_their_cpu_twins() {
    let arch = std::env::consts::ARCH;
    let adapter = native_adapter();
    let Drawing {
        frames,
        distinct,
        same,
        twins,
        untwinned,
        mutants,
        shapes,
    } = draw_frames();
    let drawn = rows_of(&frames);
    for (name, (geom, sha)) in &drawn {
        crate::stderr_line!("frame-record\t{arch}\t{adapter}\t{name}\t{geom}\t{sha}");
    }
    let digests: std::collections::BTreeMap<&str, &str> = drawn
        .iter()
        .map(|(n, (_, sha))| (n.as_str(), sha.as_str()))
        .collect();
    for (a, b) in &distinct {
        assert_ne!(
            digests.get(a.as_str()),
            digests.get(b.as_str()),
            "{a} and {b} differ in their input, so their frames must differ"
        );
    }
    for (a, b) in &same {
        assert_eq!(
            digests.get(a.as_str()),
            digests.get(b.as_str()),
            "{a} and {b} draw one input over its residency, so their frames must be byte-identical"
        );
    }
    for name in twins.keys() {
        assert!(
            digests.contains_key(name.as_str()),
            "the twin of {name} stands for a frame that was drawn"
        );
    }
    for (name, why) in &untwinned {
        crate::stderr_line!("frame-record: untwinned: {name}: {why}");
    }
    let verdict = judge(arch, &adapter, &frames, &twins);
    let record = match &verdict.record {
        Some((rows, moved)) => {
            for finding in moved {
                crate::stderr_line!("frame-record: moved: {finding}");
            }
            format!(
                "the record of {adapter}: {} of {rows} rows moved",
                moved.len()
            )
        }
        None => format!("the record holds no {adapter} block"),
    };
    let widest = verdict
        .widest
        .as_ref()
        .map(|(d, name)| format!(", the widest {d:.1} codes off ({name})"))
        .unwrap_or_default();
    crate::stderr_line!(
        "frame-record: verdict: {} of {} frames compared with their CPU twins on {adapter} \
         ({arch}), {} off{widest}; {record}",
        verdict.twinned,
        frames.len(),
        verdict.findings.len()
    );
    assert!(
        verdict.findings.is_empty(),
        "THE {ARM} ARM'S FRAMES ARE NOT THEIR CPU TWINS' on {adapter} ({arch}; {} of {} twinned \
         frames): {:#?}. Find the first texel against the CPU renderer's frame (or blit_twin's \
         for a blit); the law is the parity suites' and is never loosened to pass.",
        verdict.findings.len(),
        verdict.twinned,
        with_shapes(&verdict.findings, &shapes)
    );
    mutants_fail_their_frames(&frames, &mutants);
    planted_texel_is_decided(arch, &adapter, &frames, &twins);
    #[cfg(windows)]
    flipped_instance_byte_fails_its_twin();
    end_unless_the_colour_row_ran();
}

/// The controls on the twinned layers: each frame fails its twin drawn as
/// each of its mutants, so the bloom, the shimmer, the crown on a cropped
/// destination and the drop border's width each show in the frames that run
/// them past the twin law's bar, and an arm that drops or drifts one fails
/// its twin.
fn mutants_fail_their_frames(frames: &Named, mutants: &[(String, Mutant, Drawn)]) {
    for (name, mutant, twin) in mutants {
        let (_, drawn) = frames
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("the control's frame {name} was drawn"));
        let finding = twin_law(name, drawn, twin).err().unwrap_or_else(|| {
            panic!(
                "{name} holds to its twin drawn as {mutant:?}: that layer does not show in it past \
                 the twin law's bar, so an arm that dropped or drifted it would pass"
            )
        });
        crate::stderr_line!("frame-record: control: {mutant:?} fails {finding}");
    }
}

/// The checked-in bundled face `file`'s path.
fn bundled_face(file: &str) -> String {
    let path = aterm_tempfile::manifest_dir()
        .join("../aterm-render/assets/bundled")
        .join(file);
    path.to_str().expect("a UTF-8 font path").to_owned()
}

/// Every twinned frame also holds to its CPU twin under the bundled primary
/// faces (including Windows' Iosevka Term Semibold + ExtraBold pair and, when
/// `aterm-render/embedded-font` is active via feature unification, the virtual
/// `bundled:…` identities with [`aterm_render::bundled::letter_descent_floor`]).
#[test]
fn armed_frames_hold_to_their_cpu_twins_on_bundled_faces() {
    let arch = std::env::consts::ARCH;
    let adapter = native_adapter();
    let mut faces: Vec<(&str, String)> = [
        "IosevkaTerm-SemiBold.ttf",
        "IosevkaTerm-Regular.ttf",
        "JetBrainsMono-Regular.ttf",
    ]
    .into_iter()
    .map(|file| (file, bundled_face(file)))
    .collect();
    if aterm_render::bundled::IOSEVKA_ACTIVE {
        faces.push((
            "bundled:Iosevka Term",
            aterm_render::bundled::IOSEVKA_FAMILY.to_owned(),
        ));
    }
    if aterm_render::bundled::IOSEVKA_SEMIBOLD_ACTIVE {
        faces.push((
            "bundled:Iosevka Term Semibold",
            aterm_render::bundled::IOSEVKA_SEMIBOLD_FAMILY.to_owned(),
        ));
    }
    for (label, family) in faces {
        let Drawing {
            frames,
            distinct,
            twins,
            shapes,
            ..
        } = draw_frames_with(Some(&family));
        let drawn = rows_of(&frames);
        let digests: std::collections::BTreeMap<&str, &str> = drawn
            .iter()
            .map(|(n, (_, sha))| (n.as_str(), sha.as_str()))
            .collect();
        for (a, b) in &distinct {
            assert_ne!(
                digests.get(a.as_str()),
                digests.get(b.as_str()),
                "{label}: {a} and {b} differ in their input, so their frames must differ"
            );
        }
        let verdict = judge(arch, &adapter, &frames, &twins);
        assert!(
            verdict.findings.is_empty(),
            "THE {ARM} ARM'S FRAMES ARE NOT THEIR CPU TWINS' on {adapter} ({arch}, face {label}; \
             {} of {} twinned frames): {:#?}",
            verdict.findings.len(),
            verdict.twinned,
            with_shapes(&verdict.findings, &shapes)
        );
    }
    end_unless_the_colour_row_ran();
}

/// The recorded rows for `arch` on `adapter`, by frame name.
fn recorded_rows<'a>(
    arch: &str,
    adapter: &str,
) -> std::collections::BTreeMap<&'a str, (&'a str, &'a str)> {
    RECORD
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (a, ad, name, geom, sha) = (f.next()?, f.next()?, f.next()?, f.next()?, f.next()?);
            (a == arch && ad == adapter).then_some((name, (geom, sha)))
        })
        .collect()
}

/// Each drawn row against the record: a finding per frame drawn otherwise,
/// per frame not recorded, and per recorded frame not drawn.
fn findings_of(
    drawn: &[(String, (String, String))],
    recorded: &std::collections::BTreeMap<&str, (&str, &str)>,
) -> Vec<String> {
    let mut findings = Vec::new();
    for (name, (geom, sha)) in drawn {
        match recorded.get(name.as_str()) {
            None => findings.push(format!("{name}: not recorded")),
            Some(&(g, s)) if (g, s) != (geom.as_str(), sha.as_str()) => findings.push(format!(
                "{name}: drew {geom} {sha}, the record holds {g} {s}"
            )),
            Some(_) => {}
        }
    }
    for name in recorded.keys() {
        if !drawn.iter().any(|(n, _)| n == name) {
            findings.push(format!("{name}: recorded but not drawn"));
        }
    }
    findings
}

/// The negative control through the D3D11 arm (`D3dArmLive::corrupt_next`):
/// one instance byte flipped as the first fixture's first frame stages its
/// streams moves that readback off its twin, so the law cannot pass a broken
/// arm; the next frame, unflipped, holds to it again. Metal and GL have no
/// such seam: there [`planted_texel_is_decided`] holds the law, and the
/// `distinct` pairs that inputs reach the bytes.
#[cfg(windows)]
fn flipped_instance_byte_fails_its_twin() {
    let mut gpu = record_renderer_with(None);
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let (name, input, _) = fixtures(&gpu, true)
        .into_iter()
        .next()
        .expect("a replay fixture");
    let row = format!("{name}/readback/0");
    let twin = cpu_twin(&mut gpu, &input);
    let mut win = WindowGpu::new();
    gpu.d3d11_arm.as_mut().expect("the arm minted").corrupt_next = true;
    let flipped = gpu
        .try_render_input(&mut win, &input, None)
        .expect("the flipped frame");
    drew_on(&gpu, "the flipped frame");
    let finding = twin_law(&row, &flipped.into(), &twin);
    assert!(
        finding.is_err(),
        "an instance byte flipped on the D3D11 arm must move {row} off its twin, or the law is blind"
    );
    let clean = gpu
        .try_render_input(&mut win, &input, None)
        .expect("the clean frame");
    assert!(
        twin_law(&row, &clean.into(), &twin).is_ok(),
        "the next frame, unflipped, holds to {row}'s twin again"
    );
    crate::stderr_line!(
        "frame-record: an instance byte flipped on the D3D11 arm moves {row} off its twin: {}",
        finding.err().unwrap_or_default()
    );
}

/// The negative control: one channel of one texel of a twinned frame moved
/// 128 codes fails that frame's twin and no other, judged as drawn on this
/// adapter and on [`UNRECORDED_ADAPTER`], so the verdict is decided whether
/// or not the record holds the adapter; and where it holds this one, the
/// planted frame's row is the one more row the comparison moves.
fn planted_texel_is_decided(arch: &str, adapter: &str, frames: &Named, twins: &Twins) {
    assert!(
        recorded_rows(arch, UNRECORDED_ADAPTER).is_empty(),
        "the record holds no {UNRECORDED_ADAPTER} block"
    );
    let candidates: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|(_, (name, f))| f.texel == Texel::Unorm8 && twins.contains_key(name))
        .map(|(i, _)| i)
        .collect();
    let k = candidates[candidates.len() / 2];
    let name = frames[k].0.clone();
    let mut planted = frames.clone();
    let at = planted[k].1.bytes.len() / 2;
    planted[k].1.bytes[at] = planted[k].1.bytes[at].wrapping_add(128);
    for who in [adapter, UNRECORDED_ADAPTER] {
        let clean = judge(arch, who, frames, twins);
        assert!(
            clean.findings.is_empty(),
            "{who}: the frames as drawn hold to their twins: {:#?}",
            clean.findings
        );
        let verdict = judge(arch, who, &planted, twins);
        assert!(
            verdict.findings.len() == 1 && verdict.findings[0].starts_with(&format!("{name}: ")),
            "{who}: a texel planted in {name} must fail its twin and no other: {:#?}",
            verdict.findings
        );
        if let (Some((_, before)), Some((_, after))) = (&clean.record, &verdict.record) {
            let more: Vec<&String> = after.iter().filter(|f| !before.contains(f)).collect();
            assert!(
                more.len() == 1 && more[0].starts_with(&format!("{name}: drew ")),
                "{who}: the record's comparison moves {name}'s row and no other: {more:#?}"
            );
        }
    }
    crate::stderr_line!(
        "frame-record: a texel planted in {name} fails its twin as drawn on {adapter} and on \
         {UNRECORDED_ADAPTER}, an adapter the record holds no block for"
    );
}
