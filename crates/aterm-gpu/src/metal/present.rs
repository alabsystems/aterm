// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal window surface, drawable acquisition/reconcile, and Submit B present sequence encoding.

use std::sync::Arc;

use super::acquire_worker::{AcquireOutcome, AcquireWorker, result_is_current};
use super::encoder::{CommandBuffer, PassEncoder, RenderPassDesc, StoreAction};
#[cfg(test)]
use super::ffi::PixelFormat;
use super::ffi::{self, ClearColor, LoadAction, MtlScissorRect, MtlViewport, Obj, PrimitiveType};
use super::loss::{CbOutcome, LossLatch};
use super::resources::SealedTexture;
use super::swapchain::{
    AcquiredDrawable, Frame, LayerAcquire, OwnedDrawable, Swapchain, SwapchainConfig,
};
use crate::renderer::SurfacePresentFailure;

/// Metal window swapchain paired with its last applied [`SwapchainConfig`].
pub(crate) struct MetalWindowSurface {
    swapchain: SurfaceSwapchain,
    config: SwapchainConfig,
    wanted: SwapchainConfig,
    generation: u64,
    worker: Option<DrawableWorker>,
    ready: Option<Result<OwnedDrawable, AcquireRefusal>>,
    completed_wait_ns: Option<u64>,
    completed_queue_ns: Option<u64>,
}

struct SurfaceSwapchain(Option<Swapchain>);
impl std::ops::Deref for SurfaceSwapchain {
    type Target = Swapchain;
    fn deref(&self) -> &Swapchain {
        self.0.as_ref().expect("live surface")
    }
}
impl std::ops::DerefMut for SurfaceSwapchain {
    fn deref_mut(&mut self) -> &mut Swapchain {
        self.0.as_mut().expect("live surface")
    }
}

/// Routes the final unparent and release to the UI thread after acquisition
/// has stopped.
struct RetiredSwapchain(Option<Swapchain>);
impl Drop for RetiredSwapchain {
    fn drop(&mut self) {
        let _pool = ffi::AutoreleasePool::new();
        drop(self.0.take());
    }
}
type DrawableWorker =
    AcquireWorker<LayerAcquire, AcquiredDrawable, aterm_objc::MainThreadBound<RetiredSwapchain>>;

impl Drop for MetalWindowSurface {
    fn drop(&mut self) {
        self.ready = None;
        if let Some(worker) = self.worker.take() {
            let mt = aterm_objc::MainThread::new().expect("attached surface retires on main");
            let retirement =
                aterm_objc::MainThreadBound::new(RetiredSwapchain(self.swapchain.0.take()), mt);
            drop(worker.close(retirement));
        }
    }
}

impl MetalWindowSurface {
    /// Create a surface attached under a parent `CALayer` with a background acquire worker.
    pub(crate) fn attached(
        device: &ffi::Device,
        parent: &Obj,
        config: SwapchainConfig,
        latch: Arc<LossLatch>,
    ) -> Result<Self, String> {
        Ok(Self {
            swapchain: SurfaceSwapchain(Some(Swapchain::attached(device, parent, &config, latch)?)),
            config,
            wanted: config,
            generation: 0,
            worker: None,
            ready: None,
            completed_wait_ns: None,
            completed_queue_ns: None,
        })
    }

    /// Create a headless surface without a parent layer or worker.
    #[cfg(test)]
    pub(crate) fn standalone(
        device: &ffi::Device,
        config: SwapchainConfig,
        latch: Arc<LossLatch>,
    ) -> Result<Self, String> {
        Ok(Self {
            swapchain: SurfaceSwapchain(Some(Swapchain::standalone(device, &config, latch)?)),
            config,
            wanted: config,
            generation: 0,
            worker: None,
            ready: None,
            completed_wait_ns: None,
            completed_queue_ns: None,
        })
    }

    pub(crate) fn set_acquire_ready_callback(
        &mut self,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(), String> {
        if self.worker.is_some() {
            return Err("drawable worker already registered".into());
        }
        let _mt = aterm_objc::MainThread::new()
            .ok_or("attached drawable worker must be registered on main")?;
        self.worker = Some(
            AcquireWorker::spawn(LayerAcquire::run, notify)
                .map_err(|error| format!("cannot start drawable worker: {error}"))?,
        );
        Ok(())
    }

    #[inline]
    pub(crate) const fn async_acquire(&self) -> bool {
        self.worker.is_some()
    }

    /// Request the next frame's drawable asynchronously on the worker thread.
    pub(crate) fn prefetch_drawable(&mut self) {
        if self.ready.is_some() {
            return;
        }
        let Some(worker) = self.worker.as_mut() else {
            return;
        };
        if worker.is_pending() {
            return;
        }
        let _ = worker.try_request(self.generation, self.swapchain.acquire_request());
    }

    /// Advance the acquire epoch to invalidate any in-flight worker result.
    pub(crate) fn discard_pending_acquire(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("surface generation exhausted");
        self.ready = None;
        self.collect_acquire();
    }

    #[inline]
    pub(crate) const fn take_completed_acquire_wait_ns(&mut self) -> Option<u64> {
        self.completed_wait_ns.take()
    }

    /// Take the latest completed drawable acquisition's queue wait duration in nanoseconds.
    #[inline]
    pub(crate) const fn take_completed_acquire_queue_ns(&mut self) -> Option<u64> {
        self.completed_queue_ns.take()
    }

    fn collect_acquire(&mut self) {
        let Some(completed) = self.worker.as_mut().and_then(AcquireWorker::try_take) else {
            return;
        };
        match completed.outcome {
            AcquireOutcome::Ready(output) => {
                self.completed_wait_ns = Some(output.wait_ns);
                self.completed_queue_ns = Some(completed.queue_ns);
                if result_is_current(completed.generation, self.generation, false) {
                    self.ready = Some(output.drawable.ok_or(AcquireRefusal::AcquireNil));
                }
            }
            AcquireOutcome::Panicked | AcquireOutcome::Disconnected => {
                self.ready = Some(Err(terminal_worker_refusal(self.swapchain.latch())));
            }
        }
    }

    /// Current swapchain configuration.
    #[inline]
    pub(crate) const fn config(&self) -> &SwapchainConfig {
        &self.config
    }

    /// Live `contentsScale` of the underlying `CAMetalLayer`.
    #[cfg(test)]
    pub(crate) fn contents_scale(&self) -> f64 {
        self.swapchain.contents_scale()
    }

    /// Reconfigure the swapchain when `want` drifts from the retained config and sync `contentsScale`.
    pub(crate) fn reconcile(
        &mut self,
        device: &ffi::Device,
        want: &SwapchainConfig,
    ) -> Result<bool, String> {
        if self.wanted != *want {
            self.wanted = *want;
            self.discard_pending_acquire();
        }
        self.collect_acquire();
        if self.worker.as_ref().is_some_and(AcquireWorker::is_pending) {
            return Ok(false);
        }
        self.swapchain.sync_layer_geometry();
        let drift = self.config != *want;
        if !drift {
            return Ok(false);
        }
        self.swapchain.reconfigure(device, want)?;
        self.config = *want;
        Ok(true)
    }

    /// Acquire the next drawable, returning a typed [`AcquireRefusal`] on pending, loss, drift, or nil.
    pub(crate) fn acquire(&mut self) -> Result<Frame<'_>, AcquireRefusal> {
        if self.swapchain.latch().reason().is_some() {
            return Err(AcquireRefusal::LatchLost);
        }
        let (want_w, want_h) = (self.config.width, self.config.height);
        let frame = if let Some(worker) = self.worker.as_mut() {
            if let Some(ready) = self.ready.take() {
                self.swapchain
                    .frame_from_acquired(ready?)
                    .map_err(|_| AcquireRefusal::LatchLost)?
            } else {
                if !worker.is_pending() {
                    worker
                        .try_request(self.generation, self.swapchain.acquire_request())
                        .map_err(|_| terminal_worker_refusal(self.swapchain.latch()))?;
                }
                return Err(AcquireRefusal::Pending);
            }
        } else {
            self.swapchain
                .acquire()
                .map_err(|_| AcquireRefusal::AcquireNil)?
        };
        let (tw, th) = (
            ffi::texture_width(frame.texture()),
            ffi::texture_height(frame.texture()),
        );
        if (tw, th) != (want_w, want_h) {
            return Err(AcquireRefusal::Drift);
        }
        Ok(frame)
    }
}

/// A dead acquisition worker makes this backend unusable: feed the permanent
/// loss route (CPU fallback) rather than returning Validation and retrying on
/// every keystroke.
fn terminal_worker_refusal(latch: &LossLatch) -> AcquireRefusal {
    latch.record(&CbOutcome::Lost {
        code: None,
        name: "drawable acquisition worker stopped; backend unavailable",
    });
    AcquireRefusal::LatchLost
}

/// Return the backing `CALayer` of `target`'s `NSView`.
pub(crate) fn parent_layer_of<W: aterm_objc::HasNsView>(target: &W) -> Option<Obj> {
    let view: ffi::Id = target.ns_view();
    if view.is_null() {
        return None;
    }
    // SAFETY: `view` is a live `NSView` borrowed via `HasNsView` on the main thread;
    // `layer` is a +0 property read retained to +1 by `Obj::retain`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::checked_msg();
        Obj::retain(f(view, ffi::sel(c"layer")))
    }
}

/// Query `maximumPotentialExtendedDynamicRangeColorComponentValue` for `target`'s window screen.
pub(crate) fn screen_edr_potential_of<W: aterm_objc::HasNsView>(target: &W) -> Option<f32> {
    let view: ffi::Id = target.ns_view();
    if view.is_null() {
        return None;
    }
    // SAFETY: `view` is a live `NSView` borrowed via `HasNsView` on the main thread; `-window` and
    // `-screen` are +0 reads, nil-checked; the getter exists on macOS 10.15+ (the app requires
    // 11.0) and returns a `CGFloat`. Nothing is retained.
    unsafe {
        let obj: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::checked_msg();
        let window = obj(view, ffi::sel(c"window"));
        if window.is_null() {
            return None;
        }
        let screen = obj(window, ffi::sel(c"screen"));
        if screen.is_null() {
            return None;
        }
        let f64_of: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> f64 = ffi::checked_msg();
        let potential = f64_of(
            screen,
            ffi::sel(c"maximumPotentialExtendedDynamicRangeColorComponentValue"),
        );
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a headroom multiplier of a few units (1.0 to 16.0); the f32 rounding is far below any display step"
        )]
        let potential = potential as f32;
        Some(potential)
    }
}

/// Reason a Metal drawable acquisition returned no frame.
#[derive(Debug)]
pub(crate) enum AcquireRefusal {
    /// A demand-driven acquisition is queued or running off the UI thread.
    Pending,
    /// The process device-loss latch is set (acquire refuses before FFI).
    LatchLost,
    /// The drawable's geometry no longer matches the retained config.
    Drift,
    /// `nextDrawable` returned nil after the bounded wait: pool exhausted or
    /// the window server throttling an invisible window.
    AcquireNil,
}

/// Map an [`AcquireRefusal`] to a [`SurfacePresentFailure`].
#[inline]
pub(crate) const fn surface_present_failure(
    refusal: &AcquireRefusal,
    occluded_hint: bool,
) -> SurfacePresentFailure {
    match refusal {
        AcquireRefusal::Pending => SurfacePresentFailure::AcquirePending,
        AcquireRefusal::LatchLost => SurfacePresentFailure::Validation,
        AcquireRefusal::Drift => SurfacePresentFailure::Reconfigured,
        AcquireRefusal::AcquireNil if occluded_hint => SurfacePresentFailure::Occluded,
        AcquireRefusal::AcquireNil => SurfacePresentFailure::Timeout,
    }
}

/// Fullscreen post-processing pass bindings.
pub(crate) struct PostPass<'a> {
    pub(crate) pso: &'a Obj,
    /// Fragment texture at the row's `fragment_textures[0]` slot.
    pub(crate) tex: &'a Obj,
    pub(crate) tex_slot: usize,
    /// Fragment sampler at the row's `fragment_samplers[0]` slot.
    pub(crate) sampler: &'a Obj,
    pub(crate) sampler_slot: usize,
    /// The pass's own uniform block at the row's `fragment_buffers[0]` slot.
    pub(crate) uniform: &'a Obj,
    pub(crate) uniform_slot: usize,
}

/// Half-resolution bloom extract and fullscreen composite passes.
pub(crate) struct BloomPasses<'a> {
    /// Half-res target, cleared transparent, drawn with row 4 `GlowAdd`.
    pub(crate) half_target: &'a SealedTexture,
    pub(crate) extract_pso: &'a Obj,
    /// The 16-byte cell `Uniforms` block at row 4's `vertex_uniform` slot.
    pub(crate) cell_uniform: &'a Obj,
    pub(crate) cell_uniform_slot: usize,
    /// The ungated glow stream; the extract draws from `extract_offset`.
    pub(crate) extract_stream: &'a Obj,
    /// Byte offset of the extract window's first instance
    /// (`extract_first * stride`); the crown draws the exact set from 0.
    pub(crate) extract_offset: usize,
    pub(crate) extract_count: usize,
    /// Row 12 `bloom` composited onto the present copy (SCREEN blend), with
    /// the byte-invisible dilated-bbox scissor.
    pub(crate) composite: PostPass<'a>,
    pub(crate) composite_scissor: Option<[u32; 4]>,
}

/// Stage copy and scissored refraction pass for shimmer.
pub(crate) struct ShimmerPass<'a> {
    /// Frame-sized scratch the pass samples (stage-copied below).
    pub(crate) scratch: &'a SealedTexture,
    /// The stage copy's rect (region + `SHIMMER_COPY_MARGIN`), half-open.
    pub(crate) stage_rect: [u32; 4],
    /// The scissored refract (row 13, no blend, POST_FS binds).
    pub(crate) refract: PostPass<'a>,
    /// The pass rect == the scissor (x0, y0, x1, y1 half-open).
    pub(crate) region: [u32; 4],
}

/// Staged sub-row scroll translation pass over the present copy.
pub(crate) struct BandShiftPass<'a> {
    pub(crate) shift: crate::renderer::BandShift,
    /// Frame-sized resident scratch (`WindowGpu::metal_shift_scratch`); an
    /// overlapping same-texture copy is undefined in Metal.
    pub(crate) scratch: &'a SealedTexture,
}

/// Whole-width apron strip copy filling the gap exposed by [`BandShiftPass`].
pub(crate) struct ApronStripPass<'a> {
    pub(crate) tex: &'a SealedTexture,
    /// First framebuffer row of the exposed strip (`y1 - frac`).
    pub(crate) dst_y: usize,
    /// Strip height (`frac`, clamped to the apron's rows).
    pub(crate) rows: usize,
}

/// Straight-alpha tray overlay pass.
pub(crate) struct TrayPass<'a> {
    pub(crate) pso: &'a Obj,
    pub(crate) card_tex: &'a Obj,
    pub(crate) tex_slot: usize,
    pub(crate) sampler: &'a Obj,
    pub(crate) sampler_slot: usize,
    /// `TrayUniform` at row 17's `vertex_uniform` slot (2 — the one row off
    /// slot 0).
    pub(crate) uniform: &'a Obj,
    pub(crate) uniform_slot: usize,
}

/// A window's tray stack folded into its stack card before the tray pass draws that card
/// (`shaders/tray_prims.metal`'s `fs_tray_layer`, design §3.3): the card cleared, then each card
/// of the stack, bottom first, by the CPU fold's law.
pub(crate) struct TrayStackPass<'a> {
    pub(crate) target: &'a SealedTexture,
    pub(crate) pso: &'a Obj,
    /// The composite law at [`crate::pipeline_table::TRAY_LAYER`]'s `fragment_buffers[1]`.
    pub(crate) law: &'a Obj,
    /// Each card's texture and `TrayLayerUniform`, bottom first.
    pub(crate) cards: Vec<(&'a Obj, &'a crate::tray_prims::TrayLayerUniform)>,
}

/// Scissored instanced glow crown pass drawn during blit.
pub(crate) struct CrownPass<'a> {
    pub(crate) pso: &'a Obj,
    /// `HdrGlowUniform` at the row's `vertex_uniform` slot and its
    /// `fragment_buffers[0]` slot (`BindSpec::CROWN`: one block, two stages).
    pub(crate) uniform: &'a Obj,
    pub(crate) vertex_uniform_slot: usize,
    pub(crate) fragment_uniform_slot: usize,
    pub(crate) stream: &'a Obj,
    pub(crate) count: usize,
    /// `visible_source_scissor` — (x, y, w, h).
    pub(crate) scissor: (u32, u32, u32, u32),
}

/// Submit B pass sequence inputs from offscreen composition through final drawable blit.
pub(crate) struct PresentSequence<'a> {
    /// The clean offscreen (base + aurora — the scissor base).
    pub(crate) offscreen: &'a SealedTexture,
    /// The present copy the effects composite over and the blit samples.
    pub(crate) present_off: Option<&'a SealedTexture>,
    /// The compose copy's half-open rect from `renderer::present_copy_rect`;
    /// full frame only for the first sync into a fresh copy.
    pub(crate) copy_rect: [u32; 4],
    /// The compose copy's draw: the blit row at the present copy's format over the offscreen,
    /// with a uniform that makes it a 1:1 texel copy (`BlitUniform::compose_copy`). Required
    /// with `present_off`.
    pub(crate) compose: Option<PostPass<'a>>,
    pub(crate) bloom: Option<BloomPasses<'a>>,
    pub(crate) shimmer: Option<ShimmerPass<'a>>,
    /// The sub-row scroll translate, over the copy (requires `present_off`;
    /// refused against the bare offscreen).
    pub(crate) band_shift: Option<BandShiftPass<'a>>,
    /// The incoming row over the strip the translate exposed (requires
    /// `band_shift`'s present copy; refused likewise).
    pub(crate) apron: Option<ApronStripPass<'a>>,
    /// The tray stack's fold, before [`Self::tray`] draws the stack card.
    pub(crate) stack: Option<TrayStackPass<'a>>,
    pub(crate) tray: Option<TrayPass<'a>>,
    /// The letterbox blit (row 16) onto the drawable: POST_FS binds; the
    /// clear is the live terminal background.
    pub(crate) blit: PostPass<'a>,
    pub(crate) clear: ClearColor,
    pub(crate) crown: Option<CrownPass<'a>>,
}

/// Encode the Submit B post-processing, blit, and tap-copy sequence onto `cb`.
pub(crate) fn encode_present_sequence(
    cb: &mut CommandBuffer<'_>,
    dest: &SealedTexture,
    (dest_w, dest_h): (u32, u32),
    seq: &PresentSequence<'_>,
) -> Result<(), String> {
    // 1. Compose: clean offscreen → present copy, scissored to the copy rect. It is a draw in a
    //    render pass, never a blit: the first blit (or compute) work wakes a second class of the
    //    GPU driver's working memory, which it keeps until the GPU has been idle for a second or
    //    two (about 128 MB of an idle window's footprint on an M5 Max), so a blit on every
    //    frame would keep it resident in every window that draws at least once a second. Where
    //    the tray is the copy's only writer the copy is the tray pass's first draw; otherwise it
    //    is a pass of its own, ahead of the writers.
    let compose = match seq.present_off {
        Some(copy) => {
            let [x0, y0, x1, y1] = seq.copy_rect;
            let draw = seq.compose.as_ref().ok_or_else(|| {
                "the present copy needs its compose draw (the blit row at the copy's format)"
                    .to_owned()
            })?;
            // The draw `read`s the offscreen at each target texel, and a read outside a
            // texture is undefined in Metal: the rect stays inside both, as the blit's did.
            for (what, tex) in [("present copy", copy), ("offscreen", seq.offscreen)] {
                if x1 as usize > tex.width() || y1 as usize > tex.height() {
                    return Err(format!(
                        "the compose rect {:?} leaves the {}x{} {what}",
                        seq.copy_rect,
                        tex.width(),
                        tex.height()
                    ));
                }
            }
            (x1 > x0 && y1 > y0).then_some((
                MtlScissorRect {
                    x: x0 as usize,
                    y: y0 as usize,
                    width: (x1 - x0) as usize,
                    height: (y1 - y0) as usize,
                },
                draw,
            ))
        }
        None => None,
    };
    let compose_in_tray = seq.tray.is_some()
        && seq.bloom.is_none()
        && seq.shimmer.is_none()
        && seq.band_shift.is_none()
        && seq.apron.is_none();
    if let (Some(present_off), Some((rect, draw))) = (seq.present_off, compose)
        && !compose_in_tray
    {
        let pass = cb.render_pass(&RenderPassDesc {
            target: present_off,
            load: LoadAction::Load,
            store: StoreAction::Store,
            viewport: None,
            scissor: Some(rect),
        })?;
        draw_post(&pass, draw)?;
    }
    let composed = seq.present_off.unwrap_or(seq.offscreen);

    if let Some(bloom) = &seq.bloom {
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: bloom.half_target,
                load: LoadAction::Clear(ClearColor {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }),
                store: StoreAction::Store,
                viewport: None,
                scissor: None,
            })?;
            pass.set_pipeline(bloom.extract_pso);
            pass.set_vertex_buffer(bloom.cell_uniform, bloom.cell_uniform_slot)?;
            pass.set_instance_stream_at(bloom.extract_stream, bloom.extract_offset);
            pass.draw_instanced(PrimitiveType::Triangle, 6, bloom.extract_count)?;
        }
        {
            let pass = cb.render_pass(&RenderPassDesc {
                target: composed,
                load: LoadAction::Load,
                store: StoreAction::Store,
                viewport: None,
                scissor: bloom
                    .composite_scissor
                    .map(|[x0, y0, x1, y1]| MtlScissorRect {
                        x: x0 as usize,
                        y: y0 as usize,
                        width: (x1 - x0) as usize,
                        height: (y1 - y0) as usize,
                    }),
            })?;
            let c = &bloom.composite;
            pass.set_pipeline(c.pso);
            pass.set_fragment_texture(c.tex, c.tex_slot);
            pass.set_fragment_sampler(c.sampler, c.sampler_slot);
            pass.set_fragment_buffer(c.uniform, c.uniform_slot);
            pass.draw_fullscreen_triangle()?;
        }
    }

    if let Some(shimmer) = &seq.shimmer {
        let [sx0, sy0, sx1, sy1] = shimmer.stage_rect;
        cb.copy_texture_sub_rect(
            composed,
            (sx0 as usize, sy0 as usize),
            shimmer.scratch,
            (sx0 as usize, sy0 as usize),
            (sx1 - sx0) as usize,
            (sy1 - sy0) as usize,
        )?;
        let [rx0, ry0, rx1, ry1] = shimmer.region;
        let pass = cb.render_pass(&RenderPassDesc {
            target: composed,
            load: LoadAction::Load,
            store: StoreAction::Store,
            viewport: None,
            scissor: Some(MtlScissorRect {
                x: rx0 as usize,
                y: ry0 as usize,
                width: (rx1 - rx0) as usize,
                height: (ry1 - ry0) as usize,
            }),
        })?;
        let r = &shimmer.refract;
        pass.set_pipeline(r.pso);
        pass.set_fragment_texture(r.tex, r.tex_slot);
        pass.set_fragment_sampler(r.sampler, r.sampler_slot);
        pass.set_fragment_buffer(r.uniform, r.uniform_slot);
        pass.draw_fullscreen_triangle()?;
    }

    if let Some(bs) = &seq.band_shift {
        if seq.present_off.is_none() {
            return Err(
                "the band shift needs the present copy, never the offscreen (the \
                 scissor base stays untranslated)"
                    .to_owned(),
            );
        }
        let w = composed.width();
        let (src_y, dst_y, moved) = (
            bs.shift.src_y as usize,
            bs.shift.dst_y as usize,
            bs.shift.moved as usize,
        );
        cb.copy_texture_sub_rect(composed, (0, src_y), bs.scratch, (0, 0), w, moved)?;
        cb.copy_texture_sub_rect(bs.scratch, (0, 0), composed, (0, dst_y), w, moved)?;
    }

    if let Some(ap) = &seq.apron {
        if seq.present_off.is_none() {
            return Err(
                "the apron strip needs the present copy, never the offscreen (the \
                 scissor base stays untranslated)"
                    .to_owned(),
            );
        }
        let w = composed.width().min(ap.tex.width());
        let rows = ap.rows.min(ap.tex.height());
        if w > 0 && rows > 0 {
            cb.copy_texture_sub_rect(ap.tex, (0, 0), composed, (0, ap.dst_y), w, rows)?;
        }
    }

    if let Some(stack) = &seq.stack {
        let binds = crate::pipeline_table::TRAY_LAYER.binds;
        let pass = cb.render_pass(&RenderPassDesc {
            target: stack.target,
            load: LoadAction::Clear(ClearColor {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            }),
            store: StoreAction::Store,
            viewport: None,
            scissor: None,
        })?;
        pass.set_pipeline(stack.pso);
        pass.set_fragment_buffer(stack.law, binds.fragment_buffers[1] as usize);
        for (tex, uniform) in &stack.cards {
            let bytes = aterm_bits::bytes_of(*uniform);
            pass.set_vertex_bytes(
                bytes,
                binds
                    .vertex_uniform
                    .expect("the tray layer row binds a vertex uniform") as usize,
            )?;
            pass.set_fragment_bytes(bytes, binds.fragment_buffers[0] as usize);
            pass.set_fragment_texture(tex, binds.fragment_textures[0] as usize);
            pass.draw_strip_quad()?;
        }
    }

    if let Some(tray) = &seq.tray {
        let pass = cb.render_pass(&RenderPassDesc {
            target: composed,
            load: LoadAction::Load,
            store: StoreAction::Store,
            viewport: None,
            scissor: None,
        })?;
        if compose_in_tray && let Some((rect, draw)) = compose {
            pass.set_scissor(rect);
            draw_post(&pass, draw)?;
            pass.set_scissor(MtlScissorRect {
                x: 0,
                y: 0,
                width: composed.width(),
                height: composed.height(),
            });
        }
        pass.set_pipeline(tray.pso);
        pass.set_vertex_buffer(tray.uniform, tray.uniform_slot)?;
        pass.set_fragment_texture(tray.card_tex, tray.tex_slot);
        pass.set_fragment_sampler(tray.sampler, tray.sampler_slot);
        pass.draw_strip_quad()?;
    }

    {
        let pass = cb.render_pass(&RenderPassDesc {
            target: dest,
            load: LoadAction::Clear(seq.clear),
            store: StoreAction::Store,
            viewport: Some(MtlViewport::full_2d(dest_w as usize, dest_h as usize)),
            scissor: Some(MtlScissorRect {
                x: 0,
                y: 0,
                width: dest_w as usize,
                height: dest_h as usize,
            }),
        })?;
        let b = &seq.blit;
        pass.set_pipeline(b.pso);
        pass.set_fragment_texture(b.tex, b.tex_slot);
        pass.set_fragment_sampler(b.sampler, b.sampler_slot);
        pass.set_fragment_buffer(b.uniform, b.uniform_slot);
        pass.draw_fullscreen_triangle()?;
        if let Some(crown) = &seq.crown {
            let (x, y, w, h) = crown.scissor;
            pass.set_scissor(MtlScissorRect {
                x: x as usize,
                y: y as usize,
                width: w as usize,
                height: h as usize,
            });
            pass.set_pipeline(crown.pso);
            pass.set_vertex_buffer(crown.uniform, crown.vertex_uniform_slot)?;
            pass.set_fragment_buffer(crown.uniform, crown.fragment_uniform_slot);
            pass.set_instance_stream(crown.stream);
            pass.draw_instanced(PrimitiveType::Triangle, 6, crown.count)?;
        }
    }
    Ok(())
}

/// One fullscreen-triangle post draw (`draw`'s pipeline, texture, sampler and uniform) into
/// the open `pass`, under whatever scissor the pass holds.
fn draw_post(pass: &PassEncoder<'_, '_>, draw: &PostPass<'_>) -> Result<(), String> {
    pass.set_pipeline(draw.pso);
    pass.set_fragment_texture(draw.tex, draw.tex_slot);
    pass.set_fragment_sampler(draw.sampler, draw.sampler_slot);
    pass.set_fragment_buffer(draw.uniform, draw.uniform_slot);
    pass.draw_fullscreen_triangle()
}

#[cfg(test)]
#[path = "present_tests.rs"]
mod tests;
