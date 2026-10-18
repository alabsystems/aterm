// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GL arm's renderer side (Linux, docs/AUDIT-third-party-2026-10-15.md
//! GW.4, and the browser, wasm-platform WP.8), the twin of the armed Metal path in `renderer.rs`: the arm's
//! resident state ([`GlArmLive`]: device, programs by table row, the uniform
//! block, instance streams, glyph and decoration atlases) and the encode tail
//! [`encode_tail`] that `encode_frame` forks into once the shared instance
//! building is done.
//!
//! An armed renderer (`GpuRenderer::arm_gl`; armed at construction on Linux
//! and wasm32)
//! draws every frame here: readbacks, presents (`gl_present.rs`) and the
//! taps. In a build that also carries the wgpu arm (Linux's `--features
//! wgpu-oracle` lane, where wgpu is the oracle) a frame that fails on GL falls
//! through to the wgpu tail by name; in the shipped Linux build it is a lost frame the
//! device-loss poll owns.
//!
//! GL is synchronous to the CPU for buffer and texture updates (the driver
//! orders a write after every draw already issued that reads the object), so
//! the arm needs none of the Metal arm's frame-slot rings.

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

/// One resident atlas texture with the structural key and shelf write state
/// it was uploaded under (the Metal arm's `ArmAtlas` law: re-mint on a new
/// key, upload only the appended row band on an advanced `write_gen`).
pub(crate) struct GlArmAtlas {
    tex: GlTexture,
    key: (u32, u32, u64),
    write_gen: u64,
    uploaded_py: u32,
}

/// The GL arm's resident state, minted lazily at the first armed frame (or at
/// a window's attach, on the window system's display).
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
}

impl GlArmLive {
    /// A GL device with the frame's fixed resources: on the window system's
    /// display when `ws` names one (window surfaces need it), else headless.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn new(ws: Option<crate::gl::device::WindowSystem>) -> Result<Self, String> {
        Self::on(match ws {
            Some(ws) => GlDevice::on_window_system(ws)?,
            None => GlDevice::headless()?,
        })
    }

    /// The frame's fixed resources on `dev`.
    pub(crate) fn on(dev: GlDevice) -> Result<Self, String> {
        // Minted through the device layer, as the Metal arm mints.
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
        })
    }

    /// The program for `row`, built on first use.
    pub(crate) fn ensure_pipeline(&mut self, row: Pipeline) -> Result<(), String> {
        if self.pipelines[row as usize].is_none() {
            self.pipelines[row as usize] = Some(self.dev.pipeline(row.spec())?);
        }
        Ok(())
    }

    /// The program for `row` if built.
    pub(crate) fn pipeline(&self, row: Pipeline) -> Option<&GlPipeline> {
        self.pipelines[row as usize].as_ref()
    }

    /// The NEAREST clamp sampler.
    pub(crate) fn nearest(&self) -> &GlSampler {
        &self.nearest
    }

    /// The LINEAR clamp sampler.
    pub(crate) fn linear(&self) -> &GlSampler {
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
        // In usize: a 32768-square RGBA atlas (NVIDIA's ceiling) is 4 GiB.
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

/// The offscreen storage of a device's format plan: the native plan
/// (`Rgba8Unorm` with its sRGB alias, both views on one `SRGB8_ALPHA8`
/// texture) where the device has the sRGB view extensions, else the
/// downlevel plan's sRGB texture.
pub(crate) fn offscreen_format(dev: &GlDevice) -> (TexelFormat, Option<TexelFormat>) {
    if dev.caps().srgb_views {
        (TexelFormat::Rgba8Unorm, Some(TexelFormat::Rgba8UnormSrgb))
    } else {
        (TexelFormat::Rgba8UnormSrgb, None)
    }
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
}

/// What the tail drew: the pass count, pass 0's load and the scissor.
pub(crate) struct GlTailOut {
    pub(crate) passes: usize,
    pub(crate) load0: crate::device_layer::FrameLoad,
    pub(crate) scissor: Option<(u32, u32, u32, u32)>,
}

/// The GL encode tail: stage the instance streams, build the plan, resolve it
/// through a [`GlFrameRig`] over the arm's residency and run the shared
/// `run_frame_plan` on the GL encoder. `plan` is the renderer's plan buffer,
/// rebuilt here. `Err` names why the frame could not draw on GL; the caller
/// falls through to the wgpu tail.
pub(crate) fn encode_tail(
    live: &mut GlArmLive,
    win: &mut WindowGpu,
    plan: &mut Vec<DrawItem>,
    t: GlTailIn<'_>,
) -> Result<GlTailOut, String> {
    let (w, h) = t.dims;
    // 1. Stage the streams: `Some` count iff the stream uploaded.
    let mut counts: [Option<u32>; STREAM_COUNT] = [None; STREAM_COUNT];
    for id in StreamId::ALL {
        let (bytes, count) = t.inst.stream_bytes(id);
        if live.upload_stream(id, bytes)? {
            counts[id as usize] = Some(count);
        }
    }
    // 2. The plan. The image plane's CPU-side texels are retained at build
    //    time while armed (`ImagePlane::gl_texels`); a plane built before
    //    arming withholds its draws by name rather than draw stale texels.
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
    build_frame_plan(
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
        },
    );
    // 3. The offscreen, reused at the same dims.
    let recreate = win
        .gl_offscreen
        .as_ref()
        .is_none_or(|o| o.w != w || o.h != h || !o.tex.on(&live.dev));
    if recreate {
        let (format, alias) = offscreen_format(&live.dev);
        let tex = live.dev.texture_2d(format, w, h, alias)?;
        win.gl_offscreen = Some(GlOffscreen { tex, w, h });
    }
    // 4. Load and scissor, shared with the other arms.
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
    // 5. Residency: the uniform block, a program per drawn row, the atlases.
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
    // The sprites' textures on this device, held for the frame (a sprite's
    // upload is per device; see `SpriteTex::gl_atlas`).
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
    // 6. The rig, borrowed from the residency, and the shared walk.
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
        load0,
        scissor,
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
    /// Arm the GL arm for this renderer's readback frames. Staged: until the
    /// Linux flip a renderer is unarmed unless a test arms it; presents stay on
    /// wgpu either way.
    #[doc(hidden)]
    pub fn arm_gl(&mut self) {
        self.gl_armed = true;
    }

    /// Disarm the GL arm, for a suite that reads the wgpu offscreen or drives
    /// wgpu-typed seams (the Linux twin of `disarm_metal_for_oracle`). Only a
    /// build carrying the oracle arm has it.
    #[doc(hidden)]
    #[cfg(wgpu_arm)]
    pub fn disarm_gl_for_oracle(&mut self) {
        self.gl_armed = false;
    }

    /// Whether the last encoded frame drew on the GL arm.
    #[doc(hidden)]
    pub fn last_frame_on_gl(&self) -> bool {
        self.last_frame_arm_gl
    }

    /// Mint the GL arm on first use; `false` (with the cause kept) disarms.
    /// In a browser the arm lives on the canvas's context, the one the
    /// context was made for.
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

    /// Mint the GL arm on first use; `false` (with the cause kept) disarms.
    ///
    /// Once a window has attached (`gl_window_system`), the arm lives on that
    /// window system's display; an arm minted headless before it is dropped
    /// and minted again there (its window resources re-mint on their next
    /// use, each checking its device).
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
                // The context's facts came from a headless probe, whose
                // device can be another GPU than the window system's (a
                // hybrid laptop): the device that renders answers them from
                // its mint on. The oracle build's facts are wgpu's.
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

    /// The sub-row glide on the GL offscreen (the readback path): the band
    /// shift, then the incoming-row apron over the exposed strip.
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

    /// Shift `[y0, y1)` of the GL offscreen by the signed `delta` device pixels
    /// (positive = up): the shared `BandShift` plan as two staged copies through
    /// the resident scratch, then `apron`'s rows over the exposed strip. Shared
    /// by the sub-row glide and the whole-row scroll rescue
    /// (`shift_offscreen_rows`), as `shift_offscreen_band_px` is on the wgpu
    /// arm. A failure leaves the band unshifted with one named note.
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
        // The shift moves the offscreen outside any scissor.
        note_offscreen_written(win, None, (w, h));
    }
}

/// The armed window's resident settings card on the GL arm, the twin of
/// `MetalTrayCard`: re-uploaded only when the card's bytes change.
pub(crate) struct GlTrayCard {
    pub(crate) tex: GlTexture,
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) pixels: Vec<u8>,
}

impl GpuRenderer {
    /// The offscreen's storage, view rule and dims, for a post pass: `(format,
    /// alias, raw)`, where `raw` is the Unorm view's read and write (the
    /// native plan) and false on the downlevel plan, which samples decoded
    /// and always encodes, as wgpu's GL backend does.
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

    /// Armed arm of `composite_bloom_in_place`: the glow extract into a
    /// half-resolution target, then the screen composite back onto the
    /// offscreen's Unorm view under the same scissor, fx clip and written-rect
    /// footprint as the wgpu and Metal arms.
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
                strength: self.bloom_strength,
                radius: self.bloom_radius,
                chroma_radius: bloom_chroma_radius(self.bloom_radius),
                _pad: [0.0; 3],
            }),
        )?;
        // The composite scissor: `encode_bloom_halo`'s spelling, clip and
        // collapse arms included (the Metal arm's twin).
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
            // An empty pane intersection paints nothing: the empty footprint.
            (Some(_), Some(_)) => Some([0, 0, 0, 0]),
            // Collapse fallback: the unscissored draw covers the whole target.
            (Some(_), None) | (None, _) => {
                composite(&mut frame, None)?;
                None
            }
        };
        frame.submit()?;
        note_offscreen_written(win, halo, (fw, fh));
        Ok(())
    }

    /// Armed arm of `shimmer_offscreen_in_place`: stage-copy the sample rect,
    /// then the scissored refract in place on the GL offscreen.
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

    /// Armed arm of `draw_tray_into_offscreen`: the resident card composites
    /// straight-alpha source-over onto the GL offscreen's Unorm view.
    pub(crate) fn gl_tray_into_offscreen(
        &mut self,
        win: &mut WindowGpu,
        t: TrayQuad<'_>,
    ) -> Result<(), String> {
        let (fw, fh) = win
            .gl_offscreen
            .as_ref()
            .map(|o| (o.w, o.h))
            .ok_or("armed frame lacks its GL offscreen")?;
        // The card composites outside any encode scissor.
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
