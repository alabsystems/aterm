// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Differential tests replaying recorded wgpu frame plans on the GL arm.

use super::replay_fixtures::*;
use super::*;
use crate::device_layer::{
    DeviceHandle, FrameCopyTexture, FrameEncoder, LayerBuffer, LayerSampler, LayerTexture,
    SamplerKind, SubmittedFrame, TexUsage, TexelFormat, gl_try_read_back,
};
use crate::gl::device::GlDevice;

impl GpuRenderer {
    /// The GL half of the differential: replay the last recorded frame plan (`frame_plan`, `last_frame_load`/`last_frame_scissor`, the staged instance bytes and atlases) on `dev` and read the result back.
    pub(crate) fn gl_replay_recorded_plan_for_test(
        &mut self,
        dev: &GlDevice,
        win: &WindowGpu,
        input: &RenderInput,
        (w, h): (u32, u32),
        opts: ReplayOpts<'_>,
    ) -> Result<Frame, String> {
        let ReplayOpts {
            seed,
            corrupt,
            downlevel,
        } = opts;
        assert!(
            !self.enable_bloom && !self.enable_shimmer,
            "the replay covers the ENCODE plan; bloom and shimmer are present-time \
             layers — call set_bloom(false)/set_shimmer(false)"
        );
        let plan = self.frame_plan.clone();
        let load0 = self.last_frame_load;
        let scissor = self.last_frame_scissor;
        assert!(
            matches!(load0, crate::device_layer::FrameLoad::Clear(_)) || seed.is_some(),
            "a recorded LOAD plan reads the prior frame's texels — seed the GL offscreen"
        );
        let mut apron_win = WindowGpu::default();
        let apron_plan = self.apron_strip_plan(&mut apron_win, input, h as usize);
        let native = self.ctx.srgb_offscreen && !downlevel;
        let handle = DeviceHandle::Gl(dev);

        let (format, alias) = if native {
            (TexelFormat::Rgba8Unorm, Some(TexelFormat::Rgba8UnormSrgb))
        } else {
            (TexelFormat::Rgba8UnormSrgb, None)
        };
        let off = handle.try_create_texture_2d(
            "gl replay offscreen",
            format,
            w,
            h,
            TexUsage::OFFSCREEN,
            alias,
        )?;
        if let Some(seed) = seed {
            assert_eq!((seed.width as u32, seed.height as u32), (w, h));
            handle.upload_texture_full(&off, &seed.rgba_bytes(), w * 4, w, h);
        }
        let LayerTexture::Gl(offscreen) = off else {
            unreachable!("the GL handle mints GL textures")
        };

        let mut pipelines: [Option<crate::gl::pipelines::GlPipeline>;
            pipeline_table::PIPELINE_COUNT] = std::array::from_fn(|_| None);
        let mut uniform_binding: Option<u32> = None;
        for item in &plan {
            let row = item.pipe.row();
            let spec = row.spec();
            assert!(
                matches!(spec.library, ShaderLibrary::Cell),
                "every frame-plan row lives in cell.glsl; {row:?} says {:?}",
                spec.library
            );
            let vu = spec
                .binds
                .vertex_uniform
                .unwrap_or_else(|| panic!("frame row {row:?} declares no vertex uniform"));
            match uniform_binding {
                None => uniform_binding = Some(vu),
                Some(prev) => assert_eq!(prev, vu, "one uniform binding per frame; {row:?}"),
            }
            if pipelines[row as usize].is_none() {
                pipelines[row as usize] = Some(dev.pipeline(spec)?);
            }
        }

        let (uw, uh, tb) = self
            .uniform_written
            .expect("encode_frame wrote the shared uniforms");
        assert_eq!(
            (uw, uh),
            (w, h),
            "the replay follows the frame just encoded"
        );
        let ubuf = handle.try_create_uniform_buffer(
            "gl replay uniforms",
            std::mem::size_of::<Uniforms>() as u64,
        )?;
        // SAFETY: the GL arm's write has no GPU-exclusivity precondition (GL orders it after every issued read).
        unsafe {
            handle.write_buffer_from_zero(
                &ubuf,
                aterm_bits::bytes_of(&Uniforms {
                    screen: [w as f32, h as f32],
                    text_blend: tb as f32,
                    _pad: 0.0,
                }),
            );
        }
        let LayerBuffer::Gl(uniform) = ubuf else {
            unreachable!("the GL handle mints GL buffers")
        };

        let mut streams: [Option<crate::gl::resources::GlBuffer>; STREAM_COUNT] =
            std::array::from_fn(|_| None);
        let mut corrupted = !corrupt;
        for item in &plan {
            let idx = item.stream as usize;
            if streams[idx].is_some() {
                continue;
            }
            let (bytes, _) = self.inst.stream_bytes(item.stream);
            assert!(!bytes.is_empty(), "a planned stream has bytes");
            let mut bytes = bytes.to_vec();
            if !corrupted {
                bytes[8] ^= 0x5a;
                corrupted = true;
            }
            let buf = handle.try_create_instance_buffer("gl replay stream", bytes.len() as u64)?;
            // SAFETY: as above.
            unsafe { handle.write_buffer_from_zero(&buf, &bytes) };
            let LayerBuffer::Gl(b) = buf else {
                unreachable!("the GL handle mints GL buffers")
            };
            streams[idx] = Some(b);
        }

        let mut atlases: [Option<(
            crate::gl::resources::GlTexture,
            crate::gl::resources::GlSampler,
        )>; DRAW_ATLAS_COUNT] = std::array::from_fn(|_| None);
        for item in &plan {
            let Some(a) = item.atlas else { continue };
            if atlases[a as usize].is_some() {
                continue;
            }
            let (format, tw, th, occupied): (TexelFormat, u32, u32, &[u8]) = match a {
                DrawAtlas::Mono => {
                    let res = self.mono_res.as_ref().expect("encode set mono_res");
                    (
                        TexelFormat::R8Unorm,
                        res.atlas.width,
                        res.tex_h,
                        &res.atlas.data[..(res.atlas.width * res.atlas.height) as usize],
                    )
                }
                DrawAtlas::Color => {
                    let res = self.color_res.as_ref().expect("encode set color_res");
                    (
                        TexelFormat::Rgba8Unorm,
                        res.atlas.width,
                        res.tex_h,
                        &res.atlas.data[..(res.atlas.width * res.atlas.height * 4) as usize],
                    )
                }
                DrawAtlas::Wallpaper => {
                    let src = win
                        .wallpaper_tex
                        .as_ref()
                        .unwrap_or_else(|| panic!("plan samples {a:?} but its SpriteTex is absent"))
                        .src
                        .as_ref();
                    (
                        TexelFormat::Rgba8Unorm,
                        src.width,
                        src.height,
                        &src.rgba[..(src.width * src.height * 4) as usize],
                    )
                }
                DrawAtlas::Fx => continue,
                DrawAtlas::Deco | DrawAtlas::Image => {
                    return Err(format!(
                        "the GL replay reached {a:?}; keep fixtures to the mono, colour and \
                         sprite atlases"
                    ));
                }
            };
            let bpp = match format {
                TexelFormat::R8Unorm => 1,
                _ => 4,
            };
            let mut data = vec![0u8; (tw * th * bpp) as usize];
            data[..occupied.len()].copy_from_slice(occupied);
            let tex = handle.try_create_texture_2d(
                "gl replay atlas",
                format,
                tw,
                th,
                TexUsage::UPLOADED_SAMPLED,
                None,
            )?;
            handle.upload_texture_full(&tex, &data, tw * bpp, tw, th);
            let LayerSampler::Gl(sampler) =
                handle.try_create_sampler("gl replay atlas", SamplerKind::NearestClamp)?
            else {
                unreachable!("the GL handle mints GL samplers")
            };
            let LayerTexture::Gl(tex) = tex else {
                unreachable!("the GL handle mints GL textures")
            };
            atlases[a as usize] = Some((tex, sampler));
        }

        let mut fx_tex: [Option<fx_gpu::FxArmTex>; 2] = [None, None];
        let mut fx_palette: Option<crate::gl::resources::GlTexture> = None;
        let fx_sampler = if plan.iter().any(|i| i.atlas == Some(DrawAtlas::Fx)) {
            for kind in [fx_gpu::FxAtlasKind::R8, fx_gpu::FxAtlasKind::Rgba] {
                fx_gpu::sync_fx_tex(
                    handle,
                    &mut fx_tex[kind as usize],
                    self.fx.atlas.side(kind),
                    |_| true,
                    || {},
                )?;
            }
            gl_arm::write_fx_palette(dev, &mut fx_palette, &self.fx.palette)?;
            let LayerSampler::Gl(sampler) =
                handle.try_create_sampler("gl replay fx", SamplerKind::NearestClamp)?
            else {
                unreachable!("the GL handle mints GL samplers")
            };
            Some(sampler)
        } else {
            None
        };

        let rig = GlFrameRig {
            offscreen: &offscreen,
            pipelines: std::array::from_fn(|i| pipelines[i].as_ref()),
            uniform: &uniform,
            uniform_binding: uniform_binding.unwrap_or(0),
            streams: std::array::from_fn(|i| streams[i].as_ref()),
            atlases: std::array::from_fn(|i| atlases[i].as_ref().map(|(t, s)| (t, s))),
            fx: fx_sampler.as_ref().map(|sampler| GlFxBind {
                r8: fx_tex[0].as_ref().expect("synced above").tex.gl(),
                rgba: fx_tex[1].as_ref().expect("synced above").tex.gl(),
                palette: fx_palette.as_ref().expect("written above"),
                sampler,
            }),
        };

        let mut enc = FrameEncoder::gl(dev)?;
        let res = FrameRes::Gl(&rig);
        let passes = run_frame_plan(&mut enc, &res, &plan, load0, scissor)?;
        assert_eq!(
            passes as u32, self.last_frame_passes,
            "same plan + same pure coalescer must yield the same pass count"
        );
        match enc.submit() {
            SubmittedFrame::Gl(outcome) => outcome?,
            #[allow(unreachable_patterns, reason = "the other arms of a multi-arm build")]
            _ => unreachable!("a GL encoder commits a GL frame"),
        }

        if input.scroll_frac_px != 0 {
            let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
                self.cpu.grid_top(),
                self.cell_size().1,
                input.grid_top_row,
                input.grid_bot_row,
                h as usize,
            );
            if let Some(shift) = band_shift_ops(y0, y1, i64::from(input.scroll_frac_px)) {
                let scratch = handle.try_create_texture_2d(
                    "gl replay scroll scratch",
                    format,
                    w,
                    h,
                    TexUsage::COPY_STAGING,
                    alias,
                )?;
                let mut enc = FrameEncoder::gl(dev)?;
                enc.copy_texture_rect(
                    FrameCopyTexture::Gl(rig.offscreen),
                    (0, shift.src_y),
                    FrameCopyTexture::Gl(scratch.gl()),
                    (0, 0),
                    w,
                    shift.moved,
                )?;
                enc.copy_texture_rect(
                    FrameCopyTexture::Gl(scratch.gl()),
                    (0, 0),
                    FrameCopyTexture::Gl(rig.offscreen),
                    (0, shift.dst_y),
                    w,
                    shift.moved,
                )?;
                match enc.submit() {
                    SubmittedFrame::Gl(outcome) => outcome?,
                    #[allow(unreachable_patterns, reason = "the other arms of a multi-arm build")]
                    _ => unreachable!("a GL encoder commits a GL frame"),
                }
            }
        }
        if let Some(strip) = apron_plan {
            let tex = handle.try_create_texture_2d(
                "gl replay apron row",
                format,
                strip.w,
                strip.cell_h,
                TexUsage::COPY_STAGING,
                alias,
            )?;
            handle.upload_texture_full(
                &tex,
                &apron_win.apron_rgba,
                strip.w * 4,
                strip.w,
                strip.cell_h,
            );
            let mut enc = FrameEncoder::gl(dev)?;
            enc.copy_texture_rect(
                FrameCopyTexture::Gl(tex.gl()),
                (0, 0),
                FrameCopyTexture::Gl(rig.offscreen),
                (0, strip.dst_y),
                strip.w.min(w),
                strip.rows,
            )?;
            match enc.submit() {
                SubmittedFrame::Gl(outcome) => outcome?,
                #[allow(unreachable_patterns, reason = "the other arms of a multi-arm build")]
                _ => unreachable!("a GL encoder commits a GL frame"),
            }
        }
        gl_try_read_back(dev, rig.offscreen, w, h)
    }
}

/// The replay's inputs beyond the recorded frame.
#[derive(Clone, Copy, Default)]
pub(crate) struct ReplayOpts<'a> {
    /// The prior frame's texels, for a recorded `Load` plan.
    pub(crate) seed: Option<&'a Frame>,
    /// Flip one byte of the first planned stream (a negative control).
    pub(crate) corrupt: bool,
    /// Run the downlevel plan whatever the oracle's (a negative control for the view emulation: additive rows then blend in linear light).
    pub(crate) downlevel: bool,
}

fn wgpu_ctx_on(backends: wgpu::Backends) -> Result<GpuContext, String> {
    GpuContext::one_at_a_time(crate::instance_descriptor(backends))
}

fn arms_on(backends: wgpu::Backends) -> (GpuRenderer, GlDevice) {
    let ctx = wgpu_ctx_on(backends)
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no wgpu {backends:?}: {e}")));
    let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    gpu.disarm_gl_for_oracle();
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    (gpu, crate::gl::test_device())
}

/// Render every fixture on the oracle, replay it on the GL arm, compare.
/// Returns one summary line per fixture.
fn run_differential(backends: wgpu::Backends, glow_add: bool) -> Vec<String> {
    let (mut gpu, dev) = arms_on(backends);
    let mut win = WindowGpu::new();
    let mut fixtures = fixtures(&gpu, glow_add);
    let device = gpu.ctx.device.clone();
    gpu.effects.ensure(&device, EffectPipeline::Fx);
    gpu.effects.ensure(&device, EffectPipeline::FxLinear);
    match gpu.effects.fx_layouts(&device).err().map(str::to_owned) {
        None => fixtures.push(fx_fixture(&gpu)),
        Some(why) => crate::stderr_line!("gl replay on {backends:?}: no fx fixture: {why}"),
    }
    let mut lines = Vec::new();
    let mut run = |gpu: &mut GpuRenderer,
                   win: &mut WindowGpu,
                   name: &str,
                   input: &RenderInput,
                   drive: &[StreamId]| {
        for pass in 0..3 {
            let expected = gpu
                .try_render_input(win, input, None)
                .unwrap_or_else(|e| panic!("{name}: the wgpu frame: {e}"));
            let drawn: Vec<StreamId> = gpu.frame_plan.iter().map(|i| i.stream).collect();
            for want in drive {
                assert!(
                    drawn.contains(want),
                    "{name}: the fixture claims {want:?} but the plan never drew it"
                );
            }
            let (w, h) = (expected.width as u32, expected.height as u32);
            let actual = gpu
                .gl_replay_recorded_plan_for_test(&dev, win, input, (w, h), ReplayOpts::default())
                .unwrap_or_else(|e| panic!("{name}: the GL replay: {e}"));
            assert_frames_identical(&format!("{name} (pass {pass})"), &expected, &actual);
        }
        lines.push(format!("{name}: byte-identical x3 ({:?})", drive));
    };
    for (name, input, drive) in fixtures.drain(..) {
        run(&mut gpu, &mut win, name, &input, &drive);
    }
    gpu.set_cursor_opacity(0.5);
    let input = fixtures_base_for_cursor();
    run(
        &mut gpu,
        &mut win,
        "cursor_blend",
        &input,
        &[StreamId::CursorBlock],
    );
    assert!(
        gpu.frame_plan
            .iter()
            .any(|i| i.stream == StreamId::CursorBlock && i.pipe == DrawPipe::CursorBlend),
        "cursor_opacity 0.5 routes the cursor fill through CursorBlend"
    );
    lines
}

/// Against wgpu's GL backend on the same driver: the downlevel plan, every
/// row wgpu's GL backend can build.
#[test]
fn gl_frames_match_wgpu_gles_byte_for_byte() {
    for line in run_differential(wgpu::Backends::GL, false) {
        crate::stderr_line!("gles oracle: {line}");
    }
    end_unless_the_colour_row_ran();
}

#[test]
fn gl_frames_match_wgpu_vulkan_byte_for_byte() {
    for line in run_differential(wgpu::Backends::VULKAN, true) {
        crate::stderr_line!("vulkan oracle: {line}");
    }
    end_unless_the_colour_row_ran();
}

#[test]
fn a_flipped_instance_byte_breaks_the_gl_replay() {
    let (mut gpu, dev) = arms_on(wgpu::Backends::GL);
    let mut win = WindowGpu::new();
    let input = fixtures_base_for_cursor();
    let expected = gpu
        .try_render_input(&mut win, &input, None)
        .expect("the wgpu frame");
    let (w, h) = (expected.width as u32, expected.height as u32);
    let clean = gpu
        .gl_replay_recorded_plan_for_test(&dev, &win, &input, (w, h), ReplayOpts::default())
        .expect("the clean replay");
    assert_frames_identical("clean replay", &expected, &clean);
    let corrupt = gpu
        .gl_replay_recorded_plan_for_test(
            &dev,
            &win,
            &input,
            (w, h),
            ReplayOpts {
                corrupt: true,
                ..ReplayOpts::default()
            },
        )
        .expect("the corrupted replay");
    assert_ne!(
        expected.pixels, corrupt.pixels,
        "a flipped instance byte must change the frame, or the comparison is blind"
    );
}

#[test]
fn the_raw_view_is_what_makes_the_native_plan_match() {
    let (mut gpu, dev) = arms_on(wgpu::Backends::VULKAN);
    if !gpu.ctx.srgb_offscreen || !dev.caps().srgb_views {
        aterm_spec::host::could_not_run(
            "the native plan needs VIEW_FORMATS on wgpu and the sRGB view extensions on GL",
        );
    }
    let mut win = WindowGpu::new();
    let (_, input, _) = fixtures(&gpu, true)
        .into_iter()
        .find(|(name, ..)| *name == "glow_halo")
        .expect("the glow fixture");
    let expected = gpu
        .try_render_input(&mut win, &input, None)
        .expect("the wgpu frame");
    let (w, h) = (expected.width as u32, expected.height as u32);
    let native = gpu
        .gl_replay_recorded_plan_for_test(&dev, &win, &input, (w, h), ReplayOpts::default())
        .expect("the native replay");
    assert_frames_identical("native replay", &expected, &native);
    let downlevel = gpu
        .gl_replay_recorded_plan_for_test(
            &dev,
            &win,
            &input,
            (w, h),
            ReplayOpts {
                downlevel: true,
                ..ReplayOpts::default()
            },
        )
        .expect("the downlevel replay");
    assert_ne!(
        expected.pixels, downlevel.pixels,
        "the additive rows must differ when drawn encoded, or the raw view is not exercised"
    );
}

#[test]
fn armed_gl_readbacks_match_the_wgpu_arm_byte_for_byte() {
    let (mut oracle, _dev) = arms_on(wgpu::Backends::VULKAN);
    let (mut armed, _dev2) = arms_on(wgpu::Backends::VULKAN);
    armed.arm_gl();
    let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
    let mut fixtures = fixtures(&oracle, true);
    fixtures.push(image_fixture(&oracle));
    fixtures.push(fx_fixture(&oracle));
    for (name, input, drive) in fixtures {
        for pass in 0..3 {
            let expected = oracle
                .try_render_input(&mut win_o, &input, None)
                .unwrap_or_else(|e| panic!("{name}: the wgpu frame: {e}"));
            assert!(!oracle.last_frame_on_gl(), "the oracle is unarmed");
            let actual = armed
                .try_render_input(&mut win_a, &input, None)
                .unwrap_or_else(|e| panic!("{name}: the armed frame: {e}"));
            assert!(
                armed.last_frame_on_gl(),
                "{name}: the armed renderer fell back to wgpu (see the gl arm note)"
            );
            if pass == 0 {
                let drawn: Vec<StreamId> = armed.frame_plan.iter().map(|i| i.stream).collect();
                for want in &drive {
                    assert!(
                        drawn.contains(want),
                        "{name}: the fixture claims {want:?} but the armed plan never drew it"
                    );
                }
            }
            assert_frames_identical(&format!("armed {name} (frame {pass})"), &expected, &actual);
        }
        crate::stderr_line!("armed gl: {name}: byte-identical x3 ({drive:?})");
    }
    end_unless_the_colour_row_ran();
}

#[test]
fn armed_gl_effect_layers_match_the_wgpu_arm_byte_for_byte() {
    let (mut oracle, _d1) = arms_on(wgpu::Backends::VULKAN);
    let (mut armed, _d2) = arms_on(wgpu::Backends::VULKAN);
    armed.arm_gl();
    for g in [&mut oracle, &mut armed] {
        g.set_bloom(true);
        g.set_shimmer(true);
        g.set_shimmer_phase_for_test(Some(12.5));
    }
    let input = heat_input(&oracle);
    let card: Vec<u8> = (0..24 * 10 * 4).map(|i| (i * 37 % 256) as u8).collect();
    let tray = || TrayQuad {
        rgba: &card,
        pw: 24,
        ph: 10,
        dx: 5,
        dy: 3,
        scrim: None,
    };
    assert!(
        armed.shimmer_live(&input) && oracle.shimmer_live(&input),
        "the fixture drives the shimmer"
    );
    assert!(
        armed.shimmer_region_for_test(&input).is_some(),
        "the heat scene's haze has blank rows to rise through"
    );
    let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
    let mut plain = None;
    for pass in 0..3 {
        let expected = oracle
            .try_render_input(&mut win_o, &input, Some(tray()))
            .expect("the wgpu frame");
        let actual = armed
            .try_render_input(&mut win_a, &input, Some(tray()))
            .expect("the armed frame");
        assert!(armed.last_frame_on_gl(), "the layered readback drew on GL");
        assert_frames_identical(&format!("layered (frame {pass})"), &expected, &actual);
        plain.get_or_insert_with(|| {
            armed.set_bloom(false);
            armed.set_shimmer(false);
            let f = armed
                .try_render_input(&mut win_a, &input, None)
                .expect("the plain frame");
            armed.set_bloom(true);
            armed.set_shimmer(true);
            f
        });
    }
    assert_ne!(
        plain.expect("taken").pixels,
        armed
            .try_render_input(&mut win_a, &input, Some(tray()))
            .expect("the layered frame")
            .pixels,
        "the layers change the frame, so the comparison above covers them"
    );
}

/// The production present path, armed: a renderer on the GL arm composes its
/// presents (the dirty-row scissor over the resident offscreen, the present
/// copy, bloom, shimmer, the sub-row glide with its apron, the card, the
/// letterbox blit with the bell inversion and the drop overlay, the SDR
/// crown) into its virtual target, and the destination must equal an unarmed
/// renderer's wgpu Vulkan present byte for byte: every fixture presented three
/// times (the second and third take the scissored path over the resident
/// frame), then a changed row, then a cropped, letterboxed destination.
#[test]
fn armed_gl_presents_match_the_wgpu_arm_byte_for_byte() {
    let (mut oracle, _d1) = arms_on(wgpu::Backends::VULKAN);
    let (mut armed, _d2) = arms_on(wgpu::Backends::VULKAN);
    armed.arm_gl();
    for g in [&mut oracle, &mut armed] {
        g.set_bloom(true);
        g.set_shimmer(true);
        g.set_shimmer_phase_for_test(Some(7.25));
        g.set_sdr_glow_boost(1.0);
    }
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
    let mut fixtures = fixtures(&oracle, true);
    fixtures.push(image_fixture(&oracle));
    fixtures.push(fx_fixture(&oracle));
    fixtures.push(("heat", heat_input(&oracle), Vec::new()));
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
    for (name, input, _) in fixtures {
        let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
        for (pass, (invert, overlay, with_tray)) in [
            (false, None, false),
            (false, None, true),
            (true, Some(overlay), false),
        ]
        .into_iter()
        .enumerate()
        {
            let card_o = with_tray.then(tray);
            let card_a = with_tray.then(tray);
            let expected = presented(
                &mut oracle,
                &mut win_o,
                &input,
                invert,
                overlay,
                card_o,
                None,
            );
            assert!(!oracle.last_frame_on_gl(), "the oracle is unarmed");
            let actual = presented(
                &mut armed, &mut win_a, &input, invert, overlay, card_a, None,
            );
            assert!(
                armed.last_frame_on_gl(),
                "{name}: the armed present fell back (see the gl arm note)"
            );
            assert_frames_identical(&format!("present {name} (pass {pass})"), &expected, &actual);
        }
        let mut changed = input.clone();
        changed.row_rev_lane = 0;
        if let Some(cell) = changed.cells.get_mut(1).and_then(|r| r.get_mut(2)) {
            cell.ch = 'Z';
        }
        let expected = presented(&mut oracle, &mut win_o, &changed, false, None, None, None);
        let actual = presented(&mut armed, &mut win_a, &changed, false, None, None, None);
        assert_frames_identical(&format!("present {name} (changed row)"), &expected, &actual);
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
        assert_frames_identical(&format!("present {name} (cropped)"), &expected, &actual);
        crate::stderr_line!("armed gl present: {name}: byte-identical x5");
    }
    end_unless_the_colour_row_ran();
}

#[test]
fn a_changed_present_changes_the_gl_destination() {
    let (mut armed, _d) = arms_on(wgpu::Backends::VULKAN);
    armed.arm_gl();
    let mut win = WindowGpu::new();
    let input = fixtures_base_for_cursor();
    let plain = presented(&mut armed, &mut win, &input, false, None, None, None);
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
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';
    let other = presented(&mut armed, &mut win, &changed, false, None, None, None);
    assert_ne!(
        plain.pixels, other.pixels,
        "a changed cell reaches the destination"
    );
}

#[test]
fn a_lost_gl_context_reaches_device_lost() {
    let input = fixtures_base_for_cursor();
    let mut changed = input.clone();
    changed.row_rev_lane = 0;
    changed.cells[0][0].ch = 'Q';

    let (mut control, _d1) = arms_on(wgpu::Backends::VULKAN);
    control.arm_gl();
    let mut win = WindowGpu::new();
    for i in [&input, &changed] {
        presented(&mut control, &mut win, i, false, None, None, None);
        assert!(control.last_frame_on_gl(), "the control presents on GL");
    }
    assert!(!control.device_lost(), "an undrilled device stays live");
    assert_eq!(control.device_loss_reason(), None);

    let (mut drilled, _d2) = arms_on(wgpu::Backends::VULKAN);
    drilled.arm_gl();
    drilled.gl_inject_loss_after_for_test(1);
    let mut win = WindowGpu::new();
    assert!(
        drilled.present_virtual(&mut win, &input, false, None, None),
        "the first present draws"
    );
    assert!(drilled.last_frame_on_gl(), "the first present draws on GL");
    assert!(
        !drilled.device_lost(),
        "the drill fires after the present, not during it"
    );
    let mut win2 = WindowGpu::new();
    assert!(
        !drilled.present_virtual(&mut win2, &changed, false, None, None),
        "the present after the loss drops"
    );
    assert!(
        !drilled.last_frame_on_gl(),
        "the lost frame did not draw on GL"
    );
    assert!(
        drilled.device_lost(),
        "a lost context at a frame's first eglMakeCurrent reaches device_lost"
    );
    let reason = drilled.device_loss_reason().unwrap_or_default();
    assert!(reason.contains("EGL_CONTEXT_LOST"), "{reason}");
}

/// The video tap on the GL arm records exactly what the GL arm presents: a
/// recording of three headless presents (a static frame, a glide frame, a
/// changed row) equals, frame for frame, the wgpu arm's recording of the same
/// presents, and its format names the GL destination.
#[test]
fn the_gl_video_tap_records_what_the_wgpu_tap_records() {
    let (mut oracle, _d1) = arms_on(wgpu::Backends::VULKAN);
    let (mut armed, _d2) = arms_on(wgpu::Backends::VULKAN);
    armed.arm_gl();
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
    let (wgpu_take, gl_take) = (&takes[0], &takes[1]);
    assert!(armed.last_frame_on_gl(), "the armed recording drew on GL");
    assert_eq!(gl_take.format, "rgba8", "the GL destination is RGBA8 sRGB");
    assert_eq!((gl_take.dropped, gl_take.frames.len()), (0, 3));
    assert_eq!(wgpu_take.frames.len(), 3);
    for (e, a) in wgpu_take.frames.iter().zip(&gl_take.frames) {
        assert_eq!((e.w, e.h, e.t_us), (a.w, a.h, a.t_us));
        assert!(
            e.rgba == a.rgba,
            "recorded frame {} differs between the arms",
            a.seq
        );
    }
    assert_ne!(
        gl_take.frames[0].rgba, gl_take.frames[2].rgba,
        "the recording follows the presents (a changed row is a changed frame)"
    );
}
