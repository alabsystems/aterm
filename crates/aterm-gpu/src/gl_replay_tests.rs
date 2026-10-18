// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm against wgpu on the same driver: a frame rendered by the wgpu
//! path records its plan (draw items, load, scissor, instance bytes, atlases,
//! uniforms, band shift, apron), and [`GpuRenderer::gl_replay_recorded_plan_for_test`]
//! replays it through the GL arm via the shared `run_frame_plan`. Pipelines,
//! bindings, the sRGB view selection, clears, scissors, copies, sampling and
//! the GLSL twins are the GL arm's own; only the plan and its inputs are
//! shared. The frames must be byte-identical.
//!
//! Two oracles, one driver (Mesa: llvmpipe for GL, lavapipe for Vulkan, both
//! gallivm):
//!
//! * wgpu's GL backend (`Backends::GL`), the downlevel format plan
//!   (`srgb_offscreen` false): the GL arm runs the same plan. wgpu's GL backend
//!   cannot build the glow row at all (naga refuses `@interpolate(linear)` for
//!   every GLES target), so these fixtures leave the `GlowAdd` streams out.
//! * wgpu's Vulkan backend (`Backends::VULKAN`), Linux's production renderer
//!   before the GL flip, on the native plan: the GL arm emulates the
//!   Unorm/sRGB view pair with `EXT_sRGB_write_control` and
//!   `EXT_texture_sRGB_decode`. Every row, `GlowAdd` included.

use super::*;
use crate::device_layer::{
    DeviceHandle, FrameCopyTexture, FrameEncoder, LayerBuffer, LayerSampler, LayerTexture,
    SamplerKind, SubmittedFrame, TexUsage, TexelFormat, gl_try_read_back,
};
use crate::gl::device::GlDevice;

impl GpuRenderer {
    /// The GL half of the differential: replay the last recorded frame plan
    /// (`frame_plan`, `last_frame_load`/`last_frame_scissor`, the staged
    /// instance bytes and atlases) on `dev` and read the result back.
    ///
    /// The GL rig follows the oracle's format plan: the native plan
    /// (`srgb_offscreen`) gets one `SRGB8_ALPHA8` texture drawn through both
    /// views, the downlevel plan an sRGB texture that always encodes.
    ///
    /// `opts.seed` supplies the prior frame's texels for a recorded `Load`
    /// plan; `opts.corrupt` and `opts.downlevel` are the negative controls.
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

        // Programs for exactly the rows the plan draws; one uniform binding.
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
        // SAFETY: the GL arm's write has no GPU-exclusivity precondition (GL
        // orders it after every issued read).
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

        // The instance streams: the SAME bytes the wgpu arm uploaded.
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
                // A colour byte of the first instance (every layout keeps a
                // colour at offset 8 or later, and bytes 8..12 are colour or
                // geometry in each).
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

        // The atlases the plan samples, from the resident CPU-side texels.
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
                DrawAtlas::Rain | DrawAtlas::Cat | DrawAtlas::Free | DrawAtlas::Wallpaper => {
                    let src = match a {
                        DrawAtlas::Rain => &win.rain_atlas,
                        DrawAtlas::Cat => &win.cat_atlas,
                        DrawAtlas::Free => &win.free_atlas,
                        _ => &win.wallpaper_tex,
                    }
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

        let rig = GlFrameRig {
            offscreen: &offscreen,
            pipelines: std::array::from_fn(|i| pipelines[i].as_ref()),
            uniform: &uniform,
            uniform_binding: uniform_binding.unwrap_or(0),
            streams: std::array::from_fn(|i| streams[i].as_ref()),
            atlases: std::array::from_fn(|i| atlases[i].as_ref().map(|(t, s)| (t, s))),
        };

        // --- the replay: ONE walker, the GL arm --------------------------
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

        // --- the scroll shift's GL twin ----------------------------------
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
        // --- the incoming-row apron's GL twin (after the scroll twin) -----
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
    /// Run the downlevel plan whatever the oracle's (a negative control for
    /// the view emulation: additive rows then blend in linear light).
    pub(crate) downlevel: bool,
}

/// A wgpu context on exactly `backends` (the oracle's driver choice), or why
/// there is none.
fn wgpu_ctx_on(backends: wgpu::Backends) -> Result<GpuContext, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    crate::block_on::block_on(GpuContext::from_instance(instance))
}

/// The oracle renderer on `backends`, and the GL device; a machine without
/// either could not run the differential.
fn arms_on(backends: wgpu::Backends) -> (GpuRenderer, GlDevice) {
    let ctx = wgpu_ctx_on(backends)
        .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no wgpu {backends:?}: {e}")));
    let cpu = Renderer::from_system_with_family(None, 18.0, Theme::default())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no system monospace font"));
    let mut gpu = GpuRenderer::from_parts(ctx, cpu, None, Theme::default())
        .expect("the GPU renderer assembles on an acquired device");
    // The oracle draws on wgpu; a test that wants the production arm re-arms
    // it (`arm_gl`).
    gpu.disarm_gl_for_oracle();
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    (gpu, crate::gl::test_device())
}

const ROWS: usize = 6;
const COLS: usize = 24;

/// The differential's fixtures, each with the streams it claims to drive.
/// `glow_add` includes the `GlowAdd` streams (cursor aurora, supernova, the
/// under-glyph light), which wgpu's GL backend cannot draw.
fn fixtures(gpu: &GpuRenderer, glow_add: bool) -> Vec<(&'static str, RenderInput, Vec<StreamId>)> {
    use StreamId as S;
    use aterm_core::render::{
        FireHaloCell, FireMode, FirePatch, FreeSampler, FreeSprite, FreeZ, HaloMode, RainHalo,
        SpriteQuad,
    };
    use aterm_core::terminal::Terminal;
    use aterm_render::{CharFg, GlowQuad, premul_rgb};

    let (cw, ch) = gpu.cell_size();
    let (cw16, ch16) = (cw as u16, ch as u16);
    let base_input = || -> RenderInput {
        let mut term = Terminal::new(ROWS as u16, COLS as u16);
        term.process(b"$ gl differential >_\r\n");
        term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[4munder\x1b[0m\r\n");
        term.process(b"\x1b[1mbold\x1b[0m plain 0123456789 \xf0\x9f\x99\x82\r\n");
        term.cell_frame(ROWS, COLS)
    };
    let scene_atlas = |seed: usize| {
        Arc::new(SceneAtlas {
            width: 8,
            height: 8,
            rgba: (0..8 * 8 * 4)
                .map(|i| ((i * 13 + seed) % 256) as u8)
                .collect(),
            version: 1,
        })
    };
    let glow = |row: usize, x: usize, w: usize, color: u32| GlowQuad {
        row: row as u16,
        x: x as u16,
        y: (row * ch) as u16,
        w: w as u16,
        h: ch16,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    };
    let halo = |row: usize, col: usize, color: u32, rx: usize, mode: HaloMode| RainHalo {
        row: row as u16,
        x: (col * cw) as u16,
        y: (row * ch) as u16,
        w: cw16,
        h: ch16,
        color,
        cx: (col * cw + cw / 2) as u16,
        cy: (row * ch + ch / 2) as u16,
        rx: rx as u16,
        ry: ch16,
        mode,
    };

    // The emoji draws through the colour row where the host has a colour font
    // (the Linux lane's guest has none), so the fixture claims only what
    // every host draws.
    let mut out: Vec<(&'static str, RenderInput, Vec<StreamId>)> = vec![(
        "text_cursor",
        base_input(),
        vec![S::Bg, S::Glyph, S::Deco, S::CursorBlock],
    )];

    // Halos in both modes (and the LUMEN aurora when the oracle can draw it).
    {
        let mut input = base_input();
        input.glow_halo.push(halo(
            2,
            6,
            premul_rgb(0x00ff_c060, 200),
            cw * 2,
            HaloMode::Add,
        ));
        input
            .glow_halo
            .push(halo(2, 9, 0x0040_30a0, cw * 2, HaloMode::Over));
        input
            .glow_halo
            .push(halo(3, 3, premul_rgb(0x00ff_3020, 150), cw, HaloMode::Add));
        input
            .glow_halo
            .push(halo(3, 7, 0x0040_1030, cw * 2, HaloMode::Over));
        let mut drive = vec![S::GlowHalo, S::GlowHaloOver];
        if glow_add {
            for (i, a) in [40u8, 160, 255].iter().enumerate() {
                input
                    .cursor_glow_add
                    .push(glow(1, (i + 1) * cw, cw, premul_rgb(0x0050_fa7b, *a)));
            }
            input
                .nova_add
                .push(glow(4, 2 * cw, 2 * cw, premul_rgb(0x00ff_79c6, 220)));
            drive.push(S::GlowAdd);
            drive.push(S::NovaAdd);
        }
        out.push(("glow_halo", input, drive));
    }

    // Rain: bright-head halos in both modes and sprites through the rain atlas.
    {
        let mut input = base_input();
        input.rain_atlas = Some(scene_atlas(5));
        for i in 0..3u16 {
            input.rain_quads.push(SpriteQuad {
                row: 4,
                x: (i * 5) * cw16,
                y: 4 * ch16,
                w: 8.min(cw16),
                h: 8.min(ch16),
                ax: 0,
                ay: 0,
                aw: 8,
                ah: 8,
                tint: 0x00ff_ffff,
                alpha: 255,
                flip_x: i % 2 == 1,
            });
        }
        input.rain_add.push(halo(
            4,
            2,
            premul_rgb(0x0060_ff60, 180),
            cw * 2,
            HaloMode::Add,
        ));
        input
            .rain_add
            .push(halo(4, 6, 0x0020_2040, cw, HaloMode::Over));
        input
            .rain_add
            .push(halo(5, 3, premul_rgb(0x00ff_3020, 150), cw, HaloMode::Add));
        input
            .rain_add
            .push(halo(5, 7, 0x0040_1030, cw * 2, HaloMode::Over));
        out.push((
            "rain",
            input,
            vec![S::RainAdd, S::RainAddOver, S::RainUnder],
        ));
    }

    // Fire: Add and Over patches, two instances per mode, the contrast halo.
    {
        let mut input = base_input();
        for (i, mode) in [FireMode::Add, FireMode::Over].into_iter().enumerate() {
            for (row, col, wcells, phase, temp, strength, lean, cov_cap, base, peak) in [
                (
                    2usize,
                    1 + i * 4,
                    3usize,
                    512u32,
                    200u8,
                    220u8,
                    -2i8,
                    200u8,
                    3usize,
                    2usize,
                ),
                (4, 2 + i * 4, 2, 233, 160, 180, 1, 170, 5, 1),
            ] {
                input.fire_patch.push(FirePatch {
                    row: row as u16,
                    x: (col * cw) as u16,
                    y: (row * ch) as u16,
                    w: (wcells * cw) as u16,
                    h: ch16,
                    base_y: (base * ch) as u16,
                    peak_h: (peak * ch) as u16,
                    phase,
                    temp,
                    strength,
                    lean,
                    cov_cap,
                    cell_h: ch16,
                    mode,
                });
            }
        }
        input.fire_halo.push(FireHaloCell {
            row: 2,
            col: 2,
            strength: 200,
        });
        input.char_fg.push(CharFg {
            row: 2,
            col: 2,
            fg: 0x0020_1008,
        });
        let mut drive = vec![S::FireAdd, S::FireOver, S::GlyphHalo];
        if glow_add {
            input
                .glow_under
                .push(glow(2, cw, 6 * cw, premul_rgb(0x00ff_6020, 140)));
            drive.push(S::GlowUnder);
        }
        out.push(("fire", input, drive));
    }

    // Sprites: free sprites under and over the text, and a cat quad.
    {
        let mut input = base_input();
        input.free_atlas = Some(scene_atlas(11));
        input.cat_atlas = Some(scene_atlas(23));
        for (i, z) in [FreeZ::UnderText, FreeZ::OverText].into_iter().enumerate() {
            input.free_sprites.push(FreeSprite {
                x: (2 + i as i32) * cw as i32,
                y: (ch / 2) as i32,
                w: 8,
                h: 8,
                ax: 0,
                ay: 0,
                aw: 8,
                ah: 8,
                tint: 0x00ff_ffff,
                alpha: 255,
                flip_x: false,
                z,
                sampler: FreeSampler::Nearest,
            });
        }
        input.cat_quads.push(SpriteQuad {
            row: 5,
            x: 10 * cw16,
            y: 5 * ch16,
            w: 8.min(cw16),
            h: 8.min(ch16),
            ax: 0,
            ay: 0,
            aw: 8,
            ah: 8,
            tint: 0x00ff_ffff,
            alpha: 255,
            flip_x: false,
        });
        out.push((
            "sprites",
            input,
            vec![S::FreeUnder, S::FreeOver, S::CatOver],
        ));
    }

    // Scroll glides both ways: the band shift and the incoming-row apron.
    for (name, frac) in [("scroll", 1i32), ("scroll_down", -1)] {
        let mut input = base_input();
        input.scroll_frac_px = frac * (ch as i32 / 2).max(1);
        input.grid_top_row = 0;
        input.grid_bot_row = ROWS;
        out.push((name, input, vec![S::Bg, S::Glyph]));
    }
    out
}

/// Every pixel equal, else a panic naming the first differing texel.
fn assert_frames_identical(name: &str, expected: &Frame, actual: &Frame) {
    assert_eq!(
        (expected.width, expected.height),
        (actual.width, actual.height),
        "{name}: frame geometry diverged"
    );
    if expected.pixels == actual.pixels {
        return;
    }
    let (e, a) = (expected.rgba_bytes(), actual.rgba_bytes());
    let mut diffs = 0usize;
    let mut first = None;
    let mut max_delta = 0u8;
    for (i, (pe, pa)) in e.chunks_exact(4).zip(a.chunks_exact(4)).enumerate() {
        if pe != pa {
            diffs += 1;
            for (x, y) in pe.iter().zip(pa) {
                max_delta = max_delta.max(x.abs_diff(*y));
            }
            first.get_or_insert((i, pe.to_vec(), pa.to_vec()));
        }
    }
    let (i, pe, pa) = first.expect("pixels differ, so a texel differs");
    panic!(
        "{name}: THE GL FRAME IS NOT BYTE-IDENTICAL TO wgpu: {diffs} of {} texels differ, max \
         byte delta {max_delta}; first at ({},{}): wgpu {pe:02x?} != gl {pa:02x?}. Measure the \
         cause (twin, blend, view, clear, sampler, copy); never loosen the comparison.",
        e.len() / 4,
        i % expected.width,
        i / expected.width,
    );
}

/// Render every fixture on the oracle, replay it on the GL arm, compare.
/// Returns one summary line per fixture.
fn run_differential(backends: wgpu::Backends, glow_add: bool) -> Vec<String> {
    let (mut gpu, dev) = arms_on(backends);
    let mut win = WindowGpu::new();
    let mut fixtures = fixtures(&gpu, glow_add);
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
    // The translucent cursor fill, through the CursorBlend row.
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

/// The plain text frame the cursor fixture draws.
fn fixtures_base_for_cursor() -> RenderInput {
    let mut term = aterm_core::terminal::Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ cursor blend >_\r\n");
    term.cell_frame(ROWS, COLS)
}

/// Against wgpu's GL backend on the same driver: the downlevel plan, every
/// row wgpu's GL backend can build.
#[test]
fn gl_frames_match_wgpu_gles_byte_for_byte() {
    for line in run_differential(wgpu::Backends::GL, false) {
        crate::stderr_line!("gles oracle: {line}");
    }
}

/// Against wgpu's Vulkan backend on the same driver (Linux's production
/// renderer before the flip): the native plan, every row.
#[test]
fn gl_frames_match_wgpu_vulkan_byte_for_byte() {
    for line in run_differential(wgpu::Backends::VULKAN, true) {
        crate::stderr_line!("vulkan oracle: {line}");
    }
}

/// The negative control: one flipped instance byte in the GL replay is a
/// visible difference, so the differentials above cannot pass vacuously.
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

/// The negative control for the view emulation: on the native plan, the
/// additive rows draw through the raw (Unorm) view. Replaying the same glow
/// frame on the downlevel plan (every pass encoded and blended in linear)
/// must differ, so the Vulkan differential's agreement means the sRGB write
/// control was honoured, not ignored.
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

/// The production path, armed: a renderer with the GL arm armed draws every
/// readback frame on GL (`encode_frame`'s GL tail, the glide and apron on the
/// GL offscreen, the GL readback) and must equal an unarmed renderer's wgpu
/// Vulkan frame byte for byte, on every fixture, three frames each (the
/// second and third reuse the arm's residency: streams, atlases, programs,
/// the offscreen).
#[test]
fn armed_gl_readbacks_match_the_wgpu_arm_byte_for_byte() {
    let (mut oracle, _dev) = arms_on(wgpu::Backends::VULKAN);
    let (mut armed, _dev2) = arms_on(wgpu::Backends::VULKAN);
    armed.arm_gl();
    let (mut win_o, mut win_a) = (WindowGpu::new(), WindowGpu::new());
    for (name, input, _) in fixtures(&oracle, true) {
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
            assert_frames_identical(&format!("armed {name} (frame {pass})"), &expected, &actual);
        }
        crate::stderr_line!("armed gl: {name}: byte-identical x3");
    }
}

/// The readback's present-time layers on the armed GL arm: bloom (the glow
/// extract into a half-resolution target and the screen composite), heat
/// shimmer (stage copy and refract, phase pinned) and the tray card, each in
/// the shared order, against an unarmed renderer's wgpu Vulkan frame byte for
/// byte. The plain frame is the control: without the layers the two arms'
/// frames differ from the layered ones.
#[test]
fn armed_gl_effect_layers_match_the_wgpu_arm_byte_for_byte() {
    use aterm_core::render::{FireMode, FirePatch};
    use aterm_render::{GlowQuad, premul_rgb};
    let (mut oracle, _d1) = arms_on(wgpu::Backends::VULKAN);
    let (mut armed, _d2) = arms_on(wgpu::Backends::VULKAN);
    armed.arm_gl();
    for g in [&mut oracle, &mut armed] {
        g.set_bloom(true);
        g.set_shimmer(true);
        g.set_shimmer_phase_for_test(Some(12.5));
    }
    let (cw, ch) = oracle.cell_size();
    let mut term = aterm_core::terminal::Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ heat over the comet >_\r\n0123456789 abcdef\r\n");
    let mut input = term.cell_frame(ROWS, COLS);
    for (i, a) in [80u8, 200, 255].iter().enumerate() {
        input.cursor_glow_add.push(GlowQuad {
            row: 2,
            x: ((i + 2) * cw) as u16,
            y: (2 * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x00ff_ffff, *a),
            alpha: 0,
            color2: premul_rgb(0x0050_fa7b, *a),
            alpha2: 0,
        });
    }
    input.fire_patch.push(FirePatch {
        row: 3,
        x: (2 * cw) as u16,
        y: (3 * ch) as u16,
        w: (4 * cw) as u16,
        h: ch as u16,
        base_y: (4 * ch) as u16,
        peak_h: (2 * ch) as u16,
        phase: 300,
        temp: 220,
        strength: 230,
        lean: 1,
        cov_cap: 220,
        cell_h: ch as u16,
        mode: FireMode::Add,
    });
    let card: Vec<u8> = (0..24 * 10 * 4).map(|i| (i * 37 % 256) as u8).collect();
    let tray = || TrayQuad {
        rgba: &card,
        pw: 24,
        ph: 10,
        dx: 5,
        dy: 3,
    };
    assert!(
        armed.shimmer_live(&input) && oracle.shimmer_live(&input),
        "the fixture drives the shimmer"
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

/// One virtual present's exact destination bytes (straight sRGB RGBA8, the
/// tap's decode of either arm's destination format).
fn presented(
    gpu: &mut GpuRenderer,
    win: &mut WindowGpu,
    input: &RenderInput,
    invert: bool,
    overlay: Option<DropOverlay>,
    tray: Option<TrayQuad<'_>>,
    crop: Option<(PresentCrop, (u32, u32))>,
) -> Frame {
    // The SDR crown eases on the wall clock; each present starts its envelope
    // fresh so both arms draw the same crown level.
    gpu.reset_glow_ease_for_test(win);
    let ok = match crop {
        None => gpu.present_virtual(win, input, invert, overlay, tray),
        Some((crop, dest)) => gpu.present_virtual_cropped_with_effects_transport(
            win, input, invert, overlay, tray, crop, 0, dest,
        ),
    };
    assert!(ok, "the virtual present refused (see the arm's note)");
    let shot = gpu
        .virtual_presented_snapshot_current(win, win.resident_input_epoch(), 1)
        .expect("the presented destination reads back");
    Frame {
        width: shot.w as usize,
        height: shot.h as usize,
        pixels: shot
            .rgba
            .chunks_exact(4)
            .map(|c| {
                ((255 - u32::from(c[3])) << 24)
                    | (u32::from(c[0]) << 16)
                    | (u32::from(c[1]) << 8)
                    | u32::from(c[2])
            })
            .collect(),
    }
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
    };
    let overlay = DropOverlay {
        accent: 0x0040_a0ff,
        wash_a: 40,
        border_a: 200,
        border_scale_q4: 16,
    };
    let mut fixtures = fixtures(&oracle, true);
    fixtures.push(("heat", heat_input(&oracle), Vec::new()));
    // The layers the fixtures drive: the bloom halo and the SDR crown (cursor
    // glow), the shimmer (an additive fire patch), the sub-row glide (scroll).
    assert!(
        fixtures
            .iter()
            .any(|(_, i, _)| !i.cursor_glow_add.is_empty())
    );
    assert!(fixtures.iter().any(|(_, i, _)| armed.shimmer_live(i)));
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
        // A changed row over the resident frame (the scissored repaint).
        // A host write after the fill: the row stamps are not trusted, so the
        // diff compares content.
        let mut changed = input.clone();
        changed.row_rev_lane = 0;
        if let Some(cell) = changed.cells.get_mut(1).and_then(|r| r.get_mut(2)) {
            cell.ch = 'Z';
        }
        let expected = presented(&mut oracle, &mut win_o, &changed, false, None, None, None);
        let actual = presented(&mut armed, &mut win_a, &changed, false, None, None, None);
        assert_frames_identical(&format!("present {name} (changed row)"), &expected, &actual);
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
        assert_frames_identical(&format!("present {name} (cropped)"), &expected, &actual);
        crate::stderr_line!("armed gl present: {name}: byte-identical x5");
    }
}

/// The negative control for the present differential: the bell inversion and
/// a changed row each change the armed GL present, so the agreement above is
/// the composition's, not two blank destinations'.
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

/// The GL arm's device-loss route: the injected-loss drill fires after the
/// first armed present, the next frame's first `eglMakeCurrent` reports
/// `EGL_CONTEXT_LOST`, that present drops, and the renderer reports the device
/// lost with its cause, which is what sends the frontend to the CPU renderer
/// (`recover_from_gpu_loss`). The negative control: the same presents on an
/// undrilled renderer stay on GL and the device stays live.
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
    // `present_virtual`, not `presented`: the drill arms the device's next
    // bind, which the helper's snapshot readback would take.
    assert!(
        drilled.present_virtual(&mut win, &input, false, None, None),
        "the first present draws"
    );
    assert!(drilled.last_frame_on_gl(), "the first present draws on GL");
    assert!(
        !drilled.device_lost(),
        "the drill fires after the present, not during it"
    );
    // A second window, so the lost frame's scope is Full: this build falls a
    // failed GL frame through to the wgpu tail, which repaints a Dirty scope
    // only over a wgpu offscreen of its own, and the first window has none.
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

/// The heat-shimmer scene: an additive fire patch under a comet's glow, the
/// fixture `armed_gl_effect_layers_match_the_wgpu_arm_byte_for_byte` draws.
fn heat_input(gpu: &GpuRenderer) -> RenderInput {
    use aterm_core::render::{FireMode, FirePatch};
    use aterm_render::{GlowQuad, premul_rgb};
    let (cw, ch) = gpu.cell_size();
    let mut term = aterm_core::terminal::Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"$ heat over the comet >_\r\n0123456789 abcdef\r\n");
    let mut input = term.cell_frame(ROWS, COLS);
    for (i, a) in [80u8, 200, 255].iter().enumerate() {
        input.cursor_glow_add.push(GlowQuad {
            row: 2,
            x: ((i + 2) * cw) as u16,
            y: (2 * ch) as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x00ff_ffff, *a),
            alpha: 0,
            color2: premul_rgb(0x0050_fa7b, *a),
            alpha2: 0,
        });
    }
    input.fire_patch.push(FirePatch {
        row: 3,
        x: (2 * cw) as u16,
        y: (3 * ch) as u16,
        w: (4 * cw) as u16,
        h: ch as u16,
        base_y: (4 * ch) as u16,
        peak_h: (2 * ch) as u16,
        phase: 300,
        temp: 220,
        strength: 230,
        lean: 1,
        cov_cap: 220,
        cell_h: ch as u16,
        mode: FireMode::Add,
    });
    input
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
        takes.push(gpu.video_finish(&mut win).expect("a recording was live"));
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
