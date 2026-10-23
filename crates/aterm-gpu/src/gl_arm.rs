// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resident GL renderer state and frame-encode tail.

use super::*;
use crate::device_layer::{
    DeviceHandle, FrameCopyTexture, FrameEncoder, SamplerKind, SubmittedFrame, TexUsage,
    TexelFormat,
};
use crate::gl::device::GlDevice;
use crate::gl::pipelines::GlPipeline;
use crate::gl::resources::{GlBuffer, GlSampler, GlTexture};

/// A named GL-arm degrade note, printed once per message per process.
pub(crate) fn gl_arm_note(what: &str) {
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
    if seen
        .lock()
        .expect("the note set is never poisoned (no panics under the lock)")
        .insert(what.to_owned())
    {
        crate::stderr_line!("aterm-gpu (gl arm): {what}");
    }
}

/// The armed frame's offscreen on the GL arm.
#[derive(Debug)]
pub(crate) struct GlOffscreen {
    pub(crate) tex: GlTexture,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

/// One instance stream: a buffer grown geometrically, rewritten from 0.
struct GlStream {
    buf: GlBuffer,
    cap: usize,
}

/// Resident atlas texture with structural key and shelf write state.
pub(crate) struct GlArmAtlas {
    tex: GlTexture,
    key: (u32, u32, u64),
    write_gen: u64,
    uploaded_py: u32,
}

/// The GL arm's resident state.
pub(crate) struct GlArmLive {
    pub(crate) dev: GlDevice,
    /// Programs by table row, built on first use.
    pipelines: [Option<GlPipeline>; pipeline_table::PIPELINE_COUNT],
    /// The NEAREST clamp sampler every frame atlas binds with.
    nearest: GlSampler,
    /// The LINEAR clamp sampler the post passes (bloom, shimmer, card) bind.
    linear: GlSampler,
    /// The present's resident uniform blocks and glow streams, minted at the
    /// first present.
    pub(crate) present: Option<super::gl_present::GlPresentBufs>,
    /// The cell `Uniforms` block and the `(w, h, text_blend)` it holds.
    uniform: GlBuffer,
    uniform_key: Option<(u32, u32, u32)>,
    streams: [Option<GlStream>; STREAM_COUNT],
    /// Glyph (mono, colour) and decoration coverage. Sprite textures live with
    /// their source (`SpriteTex::gl_atlas`).
    atlases: [Option<GlArmAtlas>; DRAW_ATLAS_COUNT],
    /// The fx atlases (`fx_gpu::FxAtlasKind` order), synced from the
    /// renderer's append-only `FxAtlas`.
    fx_tex: [Option<fx_gpu::FxArmTex>; 2],
    /// The fx palette ring: one RGBA8 texture (1024 entries a row) per slot,
    /// written in turn.
    fx_palettes: [Option<GlTexture>; fx_gpu::FX_PALETTE_RING],
    fx_palette_next: usize,
}

impl GlArmLive {
    /// A GL device with the frame's fixed resources.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn new(ws: Option<crate::gl::device::WindowSystem>) -> Result<Self, String> {
        Self::on(match ws {
            Some(ws) => GlDevice::on_window_system(ws)?,
            None => GlDevice::headless()?,
        })
    }

    /// The frame's fixed resources on `dev`.
    pub(crate) fn on(dev: GlDevice) -> Result<Self, String> {
        let nearest = DeviceHandle::Gl(&dev)
            .try_create_sampler("aterm-gpu gl nearest", SamplerKind::NearestClamp)?
            .into_gl();
        let linear = dev.sampler(SamplerKind::LinearClamp)?;
        let uniform = dev.buffer(
            crate::gl::resources::GlBufferRole::Uniform,
            std::mem::size_of::<Uniforms>(),
        )?;
        Ok(Self {
            dev,
            pipelines: std::array::from_fn(|_| None),
            nearest,
            linear,
            present: None,
            uniform,
            uniform_key: None,
            streams: std::array::from_fn(|_| None),
            atlases: std::array::from_fn(|_| None),
            fx_tex: std::array::from_fn(|_| None),
            fx_palettes: std::array::from_fn(|_| None),
            fx_palette_next: 0,
        })
    }

    /// Bring both fx atlases up to date with `atlas` (re-minted on a new
    /// generation or another context, else the log's tail appended in place)
    /// when the frame draws fx or prefetches.
    fn sync_fx(&mut self, atlas: &fx_gpu::FxAtlas, drawing: bool) -> Result<u64, String> {
        let mut bytes = 0;
        for kind in [fx_gpu::FxAtlasKind::R8, fx_gpu::FxAtlasKind::Rgba] {
            let side = atlas.side(kind);
            let slot = &mut self.fx_tex[kind as usize];
            if !fx_gpu::fx_tex_due(slot, side, drawing) {
                continue;
            }
            let dev = &self.dev;
            bytes += fx_gpu::sync_fx_tex(
                DeviceHandle::Gl(dev),
                slot,
                side,
                |t| t.tex.gl().on(dev),
                || {},
            )?
            .bytes;
        }
        Ok(bytes)
    }

    /// Drop the fx textures (the renderer's residency test seam).
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn drop_fx_residency(&mut self) {
        self.fx_tex = [None, None];
        self.fx_palettes = std::array::from_fn(|_| None);
    }

    /// Write the frame's palette into the ring's next texture. The slot
    /// written.
    fn upload_fx_palette(&mut self, words: &[u32]) -> Result<usize, String> {
        let slot = self.fx_palette_next;
        self.fx_palette_next = (slot + 1) % fx_gpu::FX_PALETTE_RING;
        write_fx_palette(&self.dev, &mut self.fx_palettes[slot], words)?;
        Ok(slot)
    }

    /// The program for `row`, built on first use.
    pub(crate) fn ensure_pipeline(&mut self, row: Pipeline) -> Result<(), String> {
        if self.pipelines[row as usize].is_none() {
            self.pipelines[row as usize] = Some(self.dev.pipeline(row.spec())?);
        }
        Ok(())
    }

    /// The program for `row` if built.
    #[inline]
    pub(crate) fn pipeline(&self, row: Pipeline) -> Option<&GlPipeline> {
        self.pipelines[row as usize].as_ref()
    }

    /// The NEAREST clamp sampler.
    #[inline]
    pub(crate) const fn nearest(&self) -> &GlSampler {
        &self.nearest
    }

    /// The LINEAR clamp sampler.
    #[inline]
    pub(crate) const fn linear(&self) -> &GlSampler {
        &self.linear
    }

    /// Rewrite the uniform block when `(w, h, text_blend)` moved.
    fn ensure_uniform(&mut self, w: u32, h: u32, text_blend: u32) -> Result<(), String> {
        if self.uniform_key == Some((w, h, text_blend)) {
            return Ok(());
        }
        self.dev.write_buffer(
            &self.uniform,
            aterm_bits::bytes_of(&Uniforms {
                screen: [w as f32, h as f32],
                text_blend: text_blend as f32,
                _pad: 0.0,
            }),
        )?;
        self.uniform_key = Some((w, h, text_blend));
        Ok(())
    }

    /// Stage one stream's bytes; `Ok(false)` for an empty stream (no draw).
    fn upload_stream(&mut self, id: StreamId, bytes: &[u8]) -> Result<bool, String> {
        if bytes.is_empty() {
            return Ok(false);
        }
        let slot = &mut self.streams[id as usize];
        if slot.as_ref().is_none_or(|s| s.cap < bytes.len()) {
            let cap = bytes.len().next_power_of_two().max(256);
            *slot = Some(GlStream {
                buf: self
                    .dev
                    .buffer(crate::gl::resources::GlBufferRole::Instance, cap)?,
                cap,
            });
        }
        let stream = slot.as_ref().expect("sized above");
        self.dev.write_buffer(&stream.buf, bytes)?;
        Ok(true)
    }

    /// Ensure the resident texture for `atlas` holds `bytes` (`occupied rows
    /// x row_bytes`, tightly packed) under `key = (width, tex_h, salt)` and
    /// shelf write state `(write_gen, cur_py)`.
    #[allow(clippy::too_many_arguments)]
    fn ensure_atlas(
        dev: &GlDevice,
        slot: &mut Option<GlArmAtlas>,
        atlas: DrawAtlas,
        key: (u32, u32, u64),
        write_gen: u64,
        cur_py: u32,
        format: TexelFormat,
        bytes: &[u8],
    ) -> Result<(), String> {
        let (tw, th, _) = key;
        let bpp: u32 = match format {
            TexelFormat::R8Unorm => 1,
            _ => 4,
        };
        let row_bytes = tw as usize * bpp as usize;
        let full = row_bytes.saturating_mul(th as usize);
        if bytes.len() > full || (row_bytes > 0 && !bytes.len().is_multiple_of(row_bytes)) {
            return Err(format!(
                "gl arm: {atlas:?} atlas bytes ({}) invalid for {tw}x{th}x{bpp}",
                bytes.len()
            ));
        }
        let occupied = bytes.len().checked_div(row_bytes).unwrap_or(0) as u32;
        if let Some(a) = slot.as_mut()
            && a.key == key
            && a.tex.on(dev)
        {
            if a.write_gen == write_gen {
                return Ok(());
            }
            let y0 = a.uploaded_py.min(occupied);
            let rows = occupied.saturating_sub(y0);
            if rows > 0 {
                let start = y0 as usize * row_bytes;
                dev.upload_rows(&a.tex, y0, rows, &bytes[start..], row_bytes as u32, tw)?;
            }
            a.write_gen = write_gen;
            a.uploaded_py = cur_py;
            return Ok(());
        }
        let tex = dev.texture_2d(format, tw, th.max(1), None)?;
        dev.upload_rows(&tex, 0, occupied, bytes, row_bytes as u32, tw)?;
        *slot = Some(GlArmAtlas {
            tex,
            key,
            write_gen,
            uploaded_py: cur_py,
        });
        Ok(())
    }
}

/// The offscreen storage of a device's format plan.
#[inline]
pub(crate) fn offscreen_format(dev: &GlDevice) -> (TexelFormat, Option<TexelFormat>) {
    if dev.caps().srgb_views {
        (TexelFormat::Rgba8Unorm, Some(TexelFormat::Rgba8UnormSrgb))
    } else {
        (TexelFormat::Rgba8UnormSrgb, None)
    }
}

/// Write `words` into `tex` (`FX_PALETTE_ROW` entries per row).
pub(crate) fn write_fx_palette(
    dev: &GlDevice,
    tex: &mut Option<GlTexture>,
    words: &[u32],
) -> Result<(), String> {
    let row = fx_gpu::FX_PALETTE_ROW;
    let rows = u32::try_from(words.len().div_ceil(row as usize).max(1))
        .map_err(|_| "gl arm: the fx palette outgrows a texture".to_owned())?;
    if tex.as_ref().is_none_or(|t| t.height < rows || !t.on(dev)) {
        *tex =
            Some(dev.texture_2d(TexelFormat::Rgba8Unorm, row, rows.next_power_of_two(), None)?);
    }
    let tex = tex.as_ref().expect("minted above");
    let bytes: &[u8] = aterm_bits::cast_slice(words);
    let full = (words.len() / row as usize) as u32;
    dev.upload_rows(tex, 0, full, bytes, row * 4, row)?;
    let rem = (words.len() % row as usize) as u32;
    let tail = &bytes[(full * row * 4) as usize..];
    dev.upload_region(tex, 0, full, rem, 1, tail, rem * 4)
}

/// What the shared part of `encode_frame` hands the GL tail.
pub(crate) struct GlTailIn<'a> {
    pub(crate) inst: &'a Instances,
    pub(crate) mono: &'a ResidentAtlas,
    pub(crate) color: &'a ResidentAtlas,
    pub(crate) atlas_epoch: u64,
    pub(crate) deco: Option<&'a DecoAtlas>,
    pub(crate) scope: &'a RepaintScope<'a>,
    pub(crate) dirty_band: Option<(usize, usize)>,
    pub(crate) grid_top: usize,
    pub(crate) ch: usize,
    pub(crate) rows: usize,
    pub(crate) dims: (u32, u32),
    pub(crate) clear: crate::device_layer::ClearColor4,
    pub(crate) text_blend: u32,
    pub(crate) cursor_opaque: bool,
    /// The pass-economy ablation (`FramePlanCtx::ablation`).
    pub(crate) ablation: crate::renderer::UnormAblation,
    /// Route (1) of the pass economy may store the Exact Under fx in the base pass (`FramePlanCtx::under_opaque`).
    pub(crate) under_opaque: bool,
    /// The fx engine: its atlas and this frame's palette.
    pub(crate) fx: &'a fx_gpu::FxGpu,
}

/// What the GL encode tail drew.
pub(crate) struct GlTailOut {
    pub(crate) passes: usize,
    pub(crate) stage_ns: u64,
    /// What the plan held on the Unorm view, under the ablation it took.
    pub(crate) unorm: crate::renderer::PlanUnorm,
    pub(crate) load0: crate::device_layer::FrameLoad,
    pub(crate) scissor: Option<(u32, u32, u32, u32)>,
    /// The fx atlas texel bytes uploaded this frame.
    pub(crate) fx_atlas_bytes: u64,
    /// Whether the fx streams were staged (the residency and the palette
    /// went up; otherwise the frame's fx failed closed).
    pub(crate) fx_staged: bool,
}

/// Stage instance streams, build the frame plan, and execute it on the GL encoder.
pub(crate) fn encode_tail(
    live: &mut GlArmLive,
    win: &mut WindowGpu,
    plan: &mut Vec<DrawItem>,
    t: GlTailIn<'_>,
) -> Result<GlTailOut, String> {
    let (w, h) = t.dims;
    // A frame no present followed leaves what its resize replaced.
    live.dev.free_retired();
    let stage_started = aterm_time::Instant::now();
    let drawing = t.inst.fx_instances() > 0;
    let (fx_atlas_bytes, fx_palette) = match live.sync_fx(&t.fx.atlas, drawing).and_then(|b| {
        Ok((
            b,
            if drawing {
                Some(live.upload_fx_palette(&t.fx.palette)?)
            } else {
                None
            },
        ))
    }) {
        Ok(v) => v,
        Err(e) => {
            gl_arm_note(&format!("fx residency: {e}; fx draws nothing"));
            (0, None)
        }
    };
    let mut counts: [Option<u32>; STREAM_COUNT] = [None; STREAM_COUNT];
    for id in StreamId::ALL {
        if id.is_fx() && fx_palette.is_none() {
            continue;
        }
        let (bytes, count) = t.inst.stream_bytes(id);
        if live.upload_stream(id, bytes)? {
            counts[id as usize] = Some(count);
        }
    }
    let stage_ns = u64::try_from(stage_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    let image_plane = win
        .image_plane
        .as_ref()
        .is_some_and(|p| p.gl_texels.is_some());
    if win.image_plane.is_some() && !image_plane {
        gl_arm_note(
            "armed frame: an image plane without resident texels; image draws withheld \
             (built before arming?)",
        );
    }
    let unorm = build_frame_plan(
        plan,
        &counts,
        FramePlanCtx {
            has_image_plane: image_plane,
            has_deco: t.deco.is_some(),
            has_rain: win.rain_atlas.is_some(),
            has_cat: win.cat_atlas.is_some(),
            has_free: win.free_atlas.is_some(),
            has_wallpaper: win.wallpaper_tex.is_some(),
            cursor_opaque: t.cursor_opaque,
            ablation: t.ablation,
            under_opaque: t.under_opaque,
        },
    );
    let recreate = win
        .gl_offscreen
        .as_ref()
        .is_none_or(|o| o.w != w || o.h != h || !o.tex.on(&live.dev));
    if recreate {
        let (format, alias) = offscreen_format(&live.dev);
        let mut tex = win.gl_offscreen.take().map(|o| o.tex);
        live.dev
            .texture_2d_resizing(&mut tex, format, w, h, alias)?;
        win.gl_offscreen = tex.map(|tex| GlOffscreen { tex, w, h });
    }
    let (load0, scissor) = frame_load_and_scissor(
        t.scope,
        recreate,
        t.dirty_band,
        t.grid_top,
        t.ch,
        t.rows,
        (w, h),
        t.clear,
    );
    note_offscreen_written(
        win,
        scissor.map(|(sx, sy, sw, sh)| [sx, sy, sx + sw, sy + sh]),
        (w, h),
    );
    live.ensure_uniform(w, h, t.text_blend)?;
    let mut uniform_binding: Option<u32> = None;
    for item in plan.iter() {
        let row = item.pipe.row();
        let vu = row
            .spec()
            .binds
            .vertex_uniform
            .unwrap_or_else(|| panic!("frame row {row:?} declares no vertex uniform"));
        match uniform_binding {
            None => uniform_binding = Some(vu),
            Some(prev) => assert_eq!(prev, vu, "one uniform binding per frame; {row:?}"),
        }
        live.ensure_pipeline(row)?;
    }
    let mut sprites: [Option<Arc<GlTexture>>; DRAW_ATLAS_COUNT] = std::array::from_fn(|_| None);
    for item in plan.iter() {
        let Some(a) = item.atlas else { continue };
        let (format, key, write_gen, cur_py, bytes) = match a {
            DrawAtlas::Mono => (
                TexelFormat::R8Unorm,
                (t.mono.atlas.width, t.mono.tex_h, t.atlas_epoch),
                t.mono.atlas.write_gen,
                t.mono.atlas.py,
                &t.mono.atlas.data[..(t.mono.atlas.width * t.mono.atlas.height) as usize],
            ),
            DrawAtlas::Color => (
                TexelFormat::Rgba8Unorm,
                (t.color.atlas.width, t.color.tex_h, t.atlas_epoch),
                t.color.atlas.write_gen,
                t.color.atlas.py,
                &t.color.atlas.data[..(t.color.atlas.width * t.color.atlas.height * 4) as usize],
            ),
            DrawAtlas::Deco => {
                let d = t.deco.expect("has_deco gated this item");
                (
                    TexelFormat::R8Unorm,
                    (
                        d.atlas_w as u32,
                        d.ch as u32,
                        ((d.curl_band.0 as u64) << 32) | (d.curl_band.1 as u64 & 0xffff_ffff),
                    ),
                    0,
                    0,
                    &d.data[..],
                )
            }
            DrawAtlas::Rain | DrawAtlas::Cat | DrawAtlas::Free | DrawAtlas::Wallpaper => {
                let sprite = match a {
                    DrawAtlas::Rain => &win.rain_atlas,
                    DrawAtlas::Cat => &win.cat_atlas,
                    DrawAtlas::Free => &win.free_atlas,
                    _ => &win.wallpaper_tex,
                }
                .as_ref()
                .unwrap_or_else(|| panic!("plan samples {a:?} but its SpriteTex is absent"));
                sprites[a as usize] = Some(sprite.gl_atlas(&live.dev)?);
                continue;
            }
            DrawAtlas::Fx => continue,
            DrawAtlas::Image => {
                let p = win
                    .image_plane
                    .as_mut()
                    .expect("has_image_plane gated this item");
                GlArmLive::ensure_atlas(
                    &live.dev,
                    &mut p.gl_atlas,
                    a,
                    (p.w, p.h, p.gl_epoch),
                    0,
                    0,
                    TexelFormat::Rgba8Unorm,
                    p.gl_texels
                        .as_deref()
                        .expect("has_image_plane requires resident texels"),
                )?;
                continue;
            }
        };
        GlArmLive::ensure_atlas(
            &live.dev,
            &mut live.atlases[a as usize],
            a,
            key,
            write_gen,
            cur_py,
            format,
            bytes,
        )?;
    }
    let live = &*live;
    let off = &win.gl_offscreen.as_ref().expect("ensured above").tex;
    let rig = GlFrameRig {
        offscreen: off,
        pipelines: std::array::from_fn(|i| live.pipelines[i].as_ref()),
        uniform: &live.uniform,
        uniform_binding: uniform_binding.unwrap_or(0),
        streams: std::array::from_fn(|i| {
            counts[i].and_then(|_| live.streams[i].as_ref().map(|s| &s.buf))
        }),
        atlases: std::array::from_fn(|i| {
            let tex = match sprites[i].as_deref() {
                Some(sprite) => Some(sprite),
                None if i == DrawAtlas::Image as usize => win
                    .image_plane
                    .as_ref()
                    .and_then(|p| p.gl_atlas.as_ref())
                    .map(|a| &a.tex),
                None => live.atlases[i].as_ref().map(|a| &a.tex),
            };
            tex.map(|tex| (tex, &live.nearest))
        }),
        fx: fx_palette.map(|slot| GlFxBind {
            r8: live.fx_tex[0].as_ref().expect("synced in step 1").tex.gl(),
            rgba: live.fx_tex[1].as_ref().expect("synced in step 1").tex.gl(),
            palette: live.fx_palettes[slot].as_ref().expect("written in step 1"),
            sampler: &live.nearest,
        }),
    };
    let mut enc = FrameEncoder::gl(&live.dev)?;
    let passes = run_frame_plan(&mut enc, &FrameRes::Gl(&rig), plan, load0, scissor)?;
    match enc.submit() {
        SubmittedFrame::Gl(outcome) => outcome?,
        #[allow(unreachable_patterns, reason = "the other arms of a multi-arm build")]
        _ => unreachable!("a GL encoder commits a GL frame"),
    }
    Ok(GlTailOut {
        passes,
        stage_ns,
        unorm,
        load0,
        scissor,
        fx_atlas_bytes,
        fx_staged: fx_palette.is_some(),
    })
}

impl SpriteTex {
    /// This sprite's texture on `dev`, uploaded on first use and again when
    /// the renderer's device changed (a GL name means nothing on another
    /// context; the arm re-mints on a window system's display at the first
    /// window's attach).
    fn gl_atlas(&self, dev: &GlDevice) -> Result<Arc<GlTexture>, String> {
        let mut slot = self
            .gl_atlas
            .lock()
            .map_err(|_| "gl arm: a sprite's texture lock is poisoned".to_owned())?;
        if let Some(t) = slot.as_ref().filter(|t| t.on(dev)) {
            return Ok(Arc::clone(t));
        }
        let tex = dev.texture_2d(TexelFormat::Rgba8Unorm, self.w, self.h, None)?;
        dev.upload_rows(
            &tex,
            0,
            self.h,
            &self.src.rgba[..(self.w as usize * self.h as usize * 4)],
            self.w * 4,
            self.w,
        )?;
        let tex = Arc::new(tex);
        *slot = Some(Arc::clone(&tex));
        Ok(tex)
    }
}

impl GpuRenderer {
    /// Arm the GL arm for this renderer's readback frames.
    #[doc(hidden)]
    #[inline]
    pub const fn arm_gl(&mut self) {
        self.gl_armed = true;
    }

    /// Disarm the GL arm for oracle comparisons.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    #[inline]
    pub const fn disarm_gl_for_oracle(&mut self) {
        self.gl_armed = false;
    }

    /// Whether the last encoded frame drew on the GL arm.
    #[doc(hidden)]
    #[inline]
    pub const fn last_frame_on_gl(&self) -> bool {
        self.last_frame_arm_gl
    }

    /// Mint the GL arm on first use; `false` disarms.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn gl_arm_ready(&mut self) -> bool {
        if !self.gl_armed {
            return false;
        }
        if self.gl_arm.is_some() {
            return true;
        }
        match GlArmLive::on(self.ctx.gl_canvas.clone()) {
            Ok(live) => {
                self.gl_arm = Some(live);
                true
            }
            Err(e) => {
                gl_arm_note(&format!("the GL renderer could not start: {e}"));
                self.gl_mint_error = Some(e);
                self.gl_armed = false;
                false
            }
        }
    }

    /// Mint the GL arm on first use; `false` disarms.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn gl_arm_ready(&mut self) -> bool {
        if !self.gl_armed {
            return false;
        }
        let want = self.gl_window_system;
        if let Some(live) = self.gl_arm.as_ref() {
            if want.is_none() || live.dev.shared().window_system() == want {
                return true;
            }
            self.gl_arm = None;
        }
        match GlArmLive::new(want) {
            Ok(live) => {
                #[cfg(not(wgpu_arm))]
                {
                    let caps = live.dev.caps();
                    self.ctx.gl_max_texture_dim = caps.max_texture_dim;
                    self.ctx.srgb_offscreen = caps.srgb_views;
                    self.ctx.adapter_name.clone_from(&caps.renderer);
                }
                self.gl_arm = Some(live);
                true
            }
            Err(e) => {
                gl_arm_note(&format!("the GL renderer could not start: {e}"));
                self.gl_mint_error = Some(e);
                self.gl_armed = false;
                false
            }
        }
    }

    /// Read the armed frame back from `win.gl_offscreen`.
    pub(crate) fn gl_read_back_offscreen(
        &self,
        win: &WindowGpu,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        let off = win
            .gl_offscreen
            .as_ref()
            .ok_or("armed frame lacks its GL offscreen")?;
        let live = self.gl_arm.as_ref().ok_or("the GL arm is not minted")?;
        crate::device_layer::gl_try_read_back(&live.dev, &off.tex, width, height)
    }

    /// Shift the GL offscreen band for sub-row glide and paint the apron strip.
    pub(crate) fn gl_shift_offscreen_band(&mut self, win: &mut WindowGpu, input: &RenderInput) {
        let Some(h) = win.gl_offscreen.as_ref().map(|o| o.h) else {
            return;
        };
        let (y0, y1) = aterm_render::scroll_translate::grid_band_px(
            self.cpu.grid_top(),
            self.cell_size().1,
            input.grid_top_row,
            input.grid_bot_row,
            h as usize,
        );
        let apron = self.apron_strip_plan(win, input, h as usize);
        self.gl_shift_offscreen_band_px(win, y0, y1, i64::from(input.scroll_frac_px), apron);
    }

    /// Shift `[y0, y1)` of the GL offscreen by signed `delta` device pixels.
    pub(crate) fn gl_shift_offscreen_band_px(
        &mut self,
        win: &mut WindowGpu,
        y0: usize,
        y1: usize,
        delta: i64,
        apron: Option<ApronStrip>,
    ) {
        let Some((w, h)) = win.gl_offscreen.as_ref().map(|o| (o.w, o.h)) else {
            return;
        };
        let Some(shift) = band_shift_ops(y0, y1, delta) else {
            return;
        };
        let Some(live) = self.gl_arm.as_ref() else {
            return;
        };
        let shifted = (|| -> Result<(), String> {
            let handle = DeviceHandle::Gl(&live.dev);
            let (format, alias) = offscreen_format(&live.dev);
            if win
                .gl_shift_scratch
                .as_ref()
                .is_none_or(|t| (t.width, t.height) != (w, h) || !t.on(&live.dev))
            {
                win.gl_shift_scratch = None;
                win.gl_shift_scratch = Some(
                    handle
                        .try_create_texture_2d(
                            "aterm-gpu gl scroll shift scratch",
                            format,
                            w,
                            h,
                            TexUsage::COPY_STAGING,
                            alias,
                        )?
                        .into_gl(),
                );
            }
            let off = &win.gl_offscreen.as_ref().expect("checked above").tex;
            let scratch = win.gl_shift_scratch.as_ref().expect("ensured above");
            let mut enc = FrameEncoder::gl(&live.dev)?;
            enc.copy_texture_rect(
                FrameCopyTexture::Gl(off),
                (0, shift.src_y),
                FrameCopyTexture::Gl(scratch),
                (0, 0),
                w,
                shift.moved,
            )?;
            enc.copy_texture_rect(
                FrameCopyTexture::Gl(scratch),
                (0, 0),
                FrameCopyTexture::Gl(off),
                (0, shift.dst_y),
                w,
                shift.moved,
            )?;
            if let Some(strip) = apron {
                let tex = handle.try_create_texture_2d(
                    "aterm-gpu gl apron row",
                    format,
                    strip.w,
                    strip.cell_h,
                    TexUsage::COPY_STAGING,
                    alias,
                )?;
                handle.upload_texture_full(
                    &tex,
                    &win.apron_rgba,
                    strip.w * 4,
                    strip.w,
                    strip.cell_h,
                );
                enc.copy_texture_rect(
                    FrameCopyTexture::Gl(tex.gl()),
                    (0, 0),
                    FrameCopyTexture::Gl(off),
                    (0, strip.dst_y),
                    strip.w.min(w),
                    strip.rows,
                )?;
            }
            match enc.submit() {
                SubmittedFrame::Gl(outcome) => outcome,
                #[allow(unreachable_patterns, reason = "the other arms of a multi-arm build")]
                _ => unreachable!("a GL encoder commits a GL frame"),
            }
        })();
        if let Err(e) = shifted {
            gl_arm_note(&format!(
                "armed scroll shift failed ({e}); band left unshifted"
            ));
        }
        note_offscreen_written(win, None, (w, h));
    }
}

/// The armed window's resident settings card on the GL arm.
pub(crate) struct GlTrayCard {
    pub(crate) tex: GlTexture,
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) pixels: Vec<u8>,
    /// Which upload made it (`GpuRenderer::tray_uploads` after it): a card
    /// with the same serial shows the same pixels.
    pub(crate) serial: u64,
}

impl GpuRenderer {
    /// The offscreen's storage, view rule and dims, for a post pass: `(format,
    /// alias, raw)`, where `raw` is the Unorm view's read and write (the
    /// native plan) and false on the downlevel plan, which samples decoded
    /// and always encodes, as wgpu's GL backend does.
    #[inline]
    fn gl_post_plan(live: &GlArmLive) -> (TexelFormat, Option<TexelFormat>, bool) {
        let (format, alias) = offscreen_format(&live.dev);
        (format, alias, alias.is_some())
    }

    /// A post pass's uniform, minted for the call (the readback is the cold
    /// path).
    fn gl_post_uniform(live: &GlArmLive, bytes: &[u8]) -> Result<GlBuffer, String> {
        let buf = live
            .dev
            .buffer(crate::gl::resources::GlBufferRole::Uniform, bytes.len())?;
        live.dev.write_buffer(&buf, bytes)?;
        Ok(buf)
    }

    /// Composite bloom onto the GL offscreen in place.
    pub(crate) fn gl_composite_bloom_in_place(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Result<(), String> {
        let (glow_count, bbox, extract_first) = self.build_bloom_glow(input);
        if glow_count == 0 {
            return Ok(());
        }
        let (fw, fh) = win
            .gl_offscreen
            .as_ref()
            .map(|o| (o.w, o.h))
            .ok_or("armed frame lacks its GL offscreen")?;
        let text_blend =
            u32::from(self.cpu.text_blending() == aterm_render::TextBlending::LinearCorrected);
        let live = self.gl_arm.as_mut().ok_or("the GL arm is not minted")?;
        live.ensure_pipeline(Pipeline::GlowAdd)?;
        live.ensure_pipeline(Pipeline::Bloom)?;
        let (format, alias, raw) = Self::gl_post_plan(live);
        let (bw, bh) = ((fw / BLOOM_DOWNSCALE).max(1), (fh / BLOOM_DOWNSCALE).max(1));
        let half = live.dev.texture_2d(format, bw, bh, alias)?;
        let insts = &self.bloom_glow_scratch[extract_first as usize..];
        let stream = live.dev.buffer(
            crate::gl::resources::GlBufferRole::Instance,
            std::mem::size_of_val(insts),
        )?;
        live.dev
            .write_buffer(&stream, aterm_bits::cast_slice(insts))?;
        let cell_u = Self::gl_post_uniform(
            live,
            aterm_bits::bytes_of(&Uniforms {
                screen: [fw as f32, fh as f32],
                text_blend: text_blend as f32,
                _pad: 0.0,
            }),
        )?;
        let bu = Self::gl_post_uniform(
            live,
            aterm_bits::bytes_of(&BloomUniform {
                texel: [1.0 / bw as f32, 1.0 / bh as f32],
                strength: page_bloom_strength(self.bloom_strength, input, self.theme.bg),
                radius: self.bloom_radius,
                chroma_radius: bloom_chroma_radius(self.bloom_radius),
                extent: [1.0; 2],
                _pad: 0.0,
            }),
        )?;
        let clip = input.fx_clip.map(|(cx0, cy0, cx1, cy1)| {
            [
                u32::from(cx0),
                u32::from(cy0),
                u32::from(cx1),
                u32::from(cy1),
            ]
        });
        let scissor = if let Some([x0, y0, x1, y1]) = bbox {
            let d = ((2.0 * self.bloom_radius + 1.0) * BLOOM_DOWNSCALE as f32).ceil() as u32 + 1;
            Some([
                x0.saturating_sub(d),
                y0.saturating_sub(d),
                (x1 + d).min(fw),
                (y1 + d).min(fh),
            ])
        } else {
            clip.map(|_| [0, 0, fw, fh])
        };
        let scissor = match (scissor, clip) {
            (Some([sx0, sy0, sx1, sy1]), Some([cx0, cy0, cx1, cy1])) => Some([
                sx0.max(cx0),
                sy0.max(cy0),
                sx1.min(cx1).min(fw),
                sy1.min(cy1).min(fh),
            ]),
            (s, None) => s,
            (None, Some(_)) => unreachable!("clip-only case handled above"),
        };
        let live = &*live;
        let extract = live.pipelines[Pipeline::GlowAdd as usize]
            .as_ref()
            .expect("ensured above");
        let bloom = live.pipelines[Pipeline::Bloom as usize]
            .as_ref()
            .expect("ensured above");
        let off = &win.gl_offscreen.as_ref().expect("checked above").tex;
        let cell_binding = Pipeline::GlowAdd
            .spec()
            .binds
            .vertex_uniform
            .expect("row 4 has a vertex uniform");
        let binds = Pipeline::Bloom.spec().binds;
        let linear = &live.linear;
        let mut frame = crate::gl::encoder::GlFrame::begin(&live.dev)?;
        {
            let mut pass = frame.begin_pass(
                &half,
                false,
                crate::device_layer::FrameLoad::Clear(crate::device_layer::ClearColor4 {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }),
                None,
                (&cell_u, cell_binding),
            )?;
            pass.set_pipeline(extract);
            pass.set_stream(&stream);
            pass.draw(6, glow_count)?;
        }
        let composite = |frame: &mut crate::gl::encoder::GlFrame<'_>,
                         sc: Option<(u32, u32, u32, u32)>|
         -> Result<(), String> {
            let mut pass = frame.begin_pass(
                off,
                false,
                crate::device_layer::FrameLoad::Load,
                sc,
                (&bu, binds.fragment_buffers[0]),
            )?;
            pass.set_pipeline(bloom);
            pass.bind_texture(&half, linear, binds.fragment_textures[0], raw);
            pass.draw(3, 1)
        };
        let halo = match (scissor, clip) {
            (Some([sx0, sy0, sx1, sy1]), _) if sx1 > sx0 && sy1 > sy0 => {
                composite(&mut frame, Some((sx0, sy0, sx1 - sx0, sy1 - sy0)))?;
                Some([sx0, sy0, sx1, sy1])
            }
            (Some(_), Some(_)) => Some([0, 0, 0, 0]),
            (Some(_), None) | (None, _) => {
                composite(&mut frame, None)?;
                None
            }
        };
        frame.submit()?;
        note_offscreen_written(win, halo, (fw, fh));
        Ok(())
    }

    /// Apply shimmer refraction onto the GL offscreen in place.
    pub(crate) fn gl_shimmer_in_place(
        &mut self,
        win: &mut WindowGpu,
        input: &RenderInput,
    ) -> Result<(), String> {
        let (fw, fh) = win
            .gl_offscreen
            .as_ref()
            .map(|o| (o.w, o.h))
            .ok_or("armed frame lacks its GL offscreen")?;
        let Some(region) = self.shimmer_region(input, fw, fh) else {
            return Ok(());
        };
        let phase = self.shimmer_phase();
        let (cell_w, cell_h) = self.cpu.cell_size();
        let live = self.gl_arm.as_mut().ok_or("the GL arm is not minted")?;
        live.ensure_pipeline(Pipeline::Shimmer)?;
        let (format, alias, raw) = Self::gl_post_plan(live);
        let scratch = live.dev.texture_2d(format, fw, fh, alias)?;
        let mut heat = [[0f32; 4]; SHIMMER_BANDS / 4];
        for (i, v) in region.heat.iter().enumerate() {
            heat[i / 4][i % 4] = *v;
        }
        let su = ShimmerUniform {
            frame: [fw as f32, fh as f32],
            region_min: [region.x0 as f32, region.y0 as f32],
            region_max: [region.x1 as f32, region.y1 as f32],
            hot_top: region.hot_top,
            rise: region.rise,
            amp: (cell_h as f32 / 18.0).clamp(0.75, SHIMMER_AMP_PX),
            period: (cell_h as f32).max(4.0),
            phase,
            band_x0: region.band_x0,
            band_w: region.band_w,
            rolloff: (cell_w as f32).max(1.0),
            _pad: [0.0; 2],
            heat,
        };
        let ub = Self::gl_post_uniform(live, aterm_bits::bytes_of(&su))?;
        let m = SHIMMER_COPY_MARGIN;
        let [sx0, sy0, sx1, sy1] = [
            region.x0.saturating_sub(m),
            region.y0.saturating_sub(m),
            region.x1.saturating_add(m).min(fw),
            region.y1.saturating_add(m).min(fh),
        ];
        let live = &*live;
        let pipe = live.pipelines[Pipeline::Shimmer as usize]
            .as_ref()
            .expect("ensured above");
        let binds = Pipeline::Shimmer.spec().binds;
        let linear = &live.linear;
        let off = &win.gl_offscreen.as_ref().expect("checked above").tex;
        let mut frame = crate::gl::encoder::GlFrame::begin(&live.dev)?;
        frame.copy_rect(off, (sx0, sy0), &scratch, (sx0, sy0), sx1 - sx0, sy1 - sy0)?;
        {
            let mut pass = frame.begin_pass(
                off,
                false,
                crate::device_layer::FrameLoad::Load,
                Some((
                    region.x0,
                    region.y0,
                    region.x1 - region.x0,
                    region.y1 - region.y0,
                )),
                (&ub, binds.fragment_buffers[0]),
            )?;
            pass.set_pipeline(pipe);
            pass.bind_texture(&scratch, linear, binds.fragment_textures[0], raw);
            pass.draw(3, 1)?;
        }
        frame.submit()?;
        note_offscreen_written(
            win,
            Some([region.x0, region.y0, region.x1, region.y1]),
            (fw, fh),
        );
        Ok(())
    }

    /// Composite the resident tray card onto the GL offscreen.
    pub(crate) fn gl_tray_into_offscreen(
        &mut self,
        win: &mut WindowGpu,
        t: TrayQuad<'_>,
    ) -> Result<(), String> {
        debug_assert!(
            t.scrim.is_none(),
            "the GL arm draws no tray scrim (`TrayQuad::scrim`): Linux shows none"
        );
        let (fw, fh) = win
            .gl_offscreen
            .as_ref()
            .map(|o| (o.w, o.h))
            .ok_or("armed frame lacks its GL offscreen")?;
        note_offscreen_written(win, None, (fw, fh));
        let live = self.gl_arm.as_mut().ok_or("the GL arm is not minted")?;
        live.ensure_pipeline(Pipeline::Tray)?;
        let unchanged = win
            .gl_tray_card
            .as_ref()
            .is_some_and(|c| c.w == t.pw && c.h == t.ph && c.pixels.as_slice() == t.rgba);
        if !unchanged {
            let tex = live
                .dev
                .texture_2d(TexelFormat::Rgba8Unorm, t.pw, t.ph, None)?;
            live.dev
                .upload_rows(&tex, 0, t.ph, t.rgba, t.pw * 4, t.pw)?;
            self.tray_uploads += 1;
            let mut pixels = win.gl_tray_card.take().map_or_else(Vec::new, |c| c.pixels);
            pixels.clear();
            pixels.extend_from_slice(t.rgba);
            win.gl_tray_card = Some(GlTrayCard {
                tex,
                w: t.pw,
                h: t.ph,
                pixels,
                serial: self.tray_uploads,
            });
        }
        let tu = Self::gl_post_uniform(
            live,
            aterm_bits::bytes_of(&TrayUniform {
                rect: [t.dx as f32, t.dy as f32, t.pw as f32, t.ph as f32],
                fb: [fw as f32, fh as f32],
                _pad: [0.0, 0.0],
            }),
        )?;
        let live = &*live;
        let pipe = live.pipelines[Pipeline::Tray as usize]
            .as_ref()
            .expect("ensured above");
        let binds = Pipeline::Tray.spec().binds;
        let linear = &live.linear;
        let card = win.gl_tray_card.as_ref().expect("set above");
        let off = &win.gl_offscreen.as_ref().expect("checked above").tex;
        let mut frame = crate::gl::encoder::GlFrame::begin(&live.dev)?;
        {
            let mut pass = frame.begin_pass(
                off,
                false,
                crate::device_layer::FrameLoad::Load,
                None,
                (&tu, binds.vertex_uniform.expect("row 17 binds at 2")),
            )?;
            pass.set_pipeline(pipe);
            pass.bind_texture(&card.tex, linear, binds.fragment_textures[0], false);
            pass.draw(4, 1)?;
        }
        frame.submit()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// The frame palette's texture is the layout `cell.glsl`'s `fx_ink` reads:
    /// entry `j` at `(j % FX_PALETTE_ROW, j / FX_PALETTE_ROW)`, holding the
    /// word's little-endian bytes, over whole rows and a partial last row.
    #[test]
    fn the_fx_palette_texture_holds_entry_j_at_its_texel() {
        let dev = crate::gl::test_device();
        let row = fx_gpu::FX_PALETTE_ROW;
        let mut tex = None;
        let mut heights = Vec::new();
        for n in [3usize, 1024, 2500, 7] {
            let words: Vec<u32> = (0..n as u32)
                .map(|j| j.wrapping_mul(0x9e37_79b9) ^ (j << 7))
                .collect();
            write_fx_palette(&dev, &mut tex, &words).expect("written");
            let t = tex.as_ref().expect("minted");
            heights.push(t.height);
            let rows = n.div_ceil(row as usize) as u32;
            let bytes = dev
                .read_rgba8(t, row, rows, row as usize * 4)
                .expect("read back");
            for (j, w) in words.iter().enumerate() {
                assert_eq!(bytes[4 * j..4 * j + 4], w.to_le_bytes(), "entry {j} of {n}");
            }
        }
        assert_eq!(heights, [1, 1, 4, 4], "reused while it fits");
    }

    /// The palette ring writes three distinct textures in turn, so no frame
    /// rewrites a texture either of the two frames before it bound.
    #[test]
    fn the_fx_palette_ring_never_rewrites_the_last_two_frames_texture() {
        let mut live = GlArmLive::on(crate::gl::test_device()).expect("the arm's resources");
        let names: Vec<u32> = (0..7u32)
            .map(|f| {
                let slot = live.upload_fx_palette(&[f]).expect("written");
                live.fx_palettes[slot].as_ref().expect("written").name
            })
            .collect();
        for w in names.windows(3) {
            assert!(
                w[0] != w[1] && w[1] != w[2] && w[0] != w[2],
                "three frames in a row wrote {w:?} ({names:?})"
            );
        }
        assert_eq!(names[..4], [names[0], names[1], names[2], names[0]]);
    }
}
