// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Metal window surface, drawable acquisition/reconcile, and Submit B present sequence encoding.

use std::sync::Arc;

use super::acquire_worker::{AcquireOutcome, AcquireWorker, result_is_current};
use super::encoder::{CommandBuffer, RenderPassDesc, StoreAction};
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
    #[cfg(not(wgpu_arm))]
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
            // Dropping the frame returns the drawable to the pool unpresented.
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
    // SAFETY: `view` is a live `NSView` borrowed via `HasNsView` on the main thread.
    unsafe {
        let f: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::checked_msg();
        Obj::retain(f(view, ffi::sel(c"layer")))
    }
}

/// Query `maximumPotentialExtendedDynamicRangeColorComponentValue` for `target`'s window screen.
#[cfg(not(wgpu_arm))]
pub(crate) fn screen_edr_potential_of<W: aterm_objc::HasNsView>(target: &W) -> Option<f32> {
    let view: ffi::Id = target.ns_view();
    if view.is_null() {
        return None;
    }
    // SAFETY: `view` is a live `NSView` borrowed via `HasNsView` on the main thread.
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
    pub(crate) bloom: Option<BloomPasses<'a>>,
    pub(crate) shimmer: Option<ShimmerPass<'a>>,
    /// The sub-row scroll translate, over the copy (requires `present_off`;
    /// refused against the bare offscreen).
    pub(crate) band_shift: Option<BandShiftPass<'a>>,
    /// The incoming row over the strip the translate exposed (requires
    /// `band_shift`'s present copy; refused likewise).
    pub(crate) apron: Option<ApronStripPass<'a>>,
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
    // 1. Compose: clean offscreen → present copy (scissored sub-rect copy).
    if let Some(present_off) = seq.present_off {
        let [x0, y0, x1, y1] = seq.copy_rect;
        if x1 > x0 && y1 > y0 {
            cb.copy_texture_sub_rect(
                seq.offscreen,
                (x0 as usize, y0 as usize),
                present_off,
                (x0 as usize, y0 as usize),
                (x1 - x0) as usize,
                (y1 - y0) as usize,
            )?;
        }
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

    if let Some(tray) = &seq.tray {
        let pass = cb.render_pass(&RenderPassDesc {
            target: composed,
            load: LoadAction::Load,
            store: StoreAction::Store,
            viewport: None,
            scissor: None,
        })?;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::loss::{CbOutcome, LossLatch};
    use super::*;
    use crate::metal::test_support::{device, plain_calayer};

    /// A bare `CALayer` at a caller-chosen `contentsScale` — the headless
    /// stand-in for the winit view's backing layer on a Retina display.
    fn parent_at_scale(scale: f64) -> Obj {
        let layer = plain_calayer();
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: sets `contentsScale` on a live `CALayer`.
        unsafe {
            let set: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, f64) = ffi::checked_msg();
            set(layer.id(), ffi::sel(c"setContentsScale:"), scale);
        }
        layer
    }

    /// `contentsScale` is not a config axis, so the sync must run before
    /// `reconcile`'s no-drift early-out: with a constant config, `reconcile`
    /// returns `false` every time and the scale must still follow the parent.
    #[test]
    fn the_no_drift_reconcile_still_tracks_the_parents_contents_scale() {
        let dev = device();
        let _test_pool = ffi::AutoreleasePool::new();
        let parent = parent_at_scale(2.0);
        let config = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: 16,
            height: 16,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 2,
            opaque: true,
        };
        let mut surface =
            MetalWindowSurface::attached(&dev, &parent, config, Arc::new(LossLatch::new()))
                .expect("attached surface");
        assert!(
            (surface.contents_scale() - 2.0).abs() < f64::EPSILON,
            "attach adopts the Retina parent's scale — got {}",
            surface.contents_scale()
        );

        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: sets `contentsScale` on the live parent `CALayer`.
        unsafe {
            let set: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, f64) = ffi::checked_msg();
            set(parent.id(), ffi::sel(c"setContentsScale:"), 1.0);
        }
        let reconfigured = surface
            .reconcile(&dev, &config)
            .expect("a no-drift reconcile succeeds");
        assert!(
            !reconfigured,
            "nothing in the drift set moved — this is the steady-present path"
        );
        assert!(
            (surface.contents_scale() - 1.0).abs() < f64::EPSILON,
            "the steady present must still adopt the parent's new scale: \
             contentsScale is not a drift axis and cannot be one, so the poll \
             has to run ahead of the early-out — got {}",
            surface.contents_scale()
        );
    }

    #[test]
    fn terminal_worker_failure_enters_permanent_backend_recovery() {
        let latch = LossLatch::new();
        assert!(latch.reason().is_none());
        let refusal = terminal_worker_refusal(&latch);
        assert!(
            latch.reason().is_some(),
            "a Validation-only mutant would park forever"
        );
        assert!(matches!(refusal, AcquireRefusal::LatchLost));
        assert_eq!(
            surface_present_failure(&refusal, false),
            SurfacePresentFailure::Validation
        );
    }

    /// Live drift, deviceless-nil and latch refusals map to `Reconfigured`,
    /// `Timeout`/`Occluded` (by the hint) and `Validation`.
    #[test]
    fn acquire_refusals_map_to_the_wgpu_failure_contract() {
        let dev = device();
        let latch = Arc::new(LossLatch::new());
        let config = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: 16,
            height: 16,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 3,
            opaque: true,
        };
        let mut surface =
            MetalWindowSurface::standalone(&dev, config, Arc::clone(&latch)).expect("surface");

        surface.config.width = 24;
        let refusal = surface.acquire().expect_err("a drifted acquire refuses");
        assert!(
            matches!(refusal, AcquireRefusal::Drift),
            "a drifted acquire refuses as Drift: {refusal:?}"
        );
        assert_eq!(
            surface_present_failure(&refusal, false),
            SurfacePresentFailure::Reconfigured
        );
        surface.config.width = 16;

        // SAFETY: `setDevice:` accepts nil on a live `CAMetalLayer`.
        unsafe {
            let set_obj: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, ffi::Id) =
                ffi::checked_msg();
            set_obj(
                surface.swapchain.layer_ptr(),
                ffi::sel(c"setDevice:"),
                ffi::Id::NIL,
            );
        }
        let t0 = std::time::Instant::now();
        let nil = surface
            .acquire()
            .expect_err("a deviceless layer must not vend");
        let waited = t0.elapsed();
        assert!(
            matches!(nil, AcquireRefusal::AcquireNil),
            "the deviceless refusal is the nil arm, not drift/latch: {nil:?}"
        );
        assert!(
            waited < std::time::Duration::from_secs(60),
            "the live nil is BOUNDED — {waited:?} is not a parked thread"
        );
        assert_eq!(
            surface_present_failure(&nil, false),
            SurfacePresentFailure::Timeout
        );
        assert_eq!(
            surface_present_failure(&nil, true),
            SurfacePresentFailure::Occluded
        );
        // SAFETY: `setDevice:` restores the valid `MTLDevice` on the live `CAMetalLayer`.
        unsafe {
            let set_obj: unsafe extern "C-unwind" fn(ffi::Id, ffi::Sel, ffi::Id) =
                ffi::checked_msg();
            set_obj(
                surface.swapchain.layer_ptr(),
                ffi::sel(c"setDevice:"),
                dev.id(),
            );
        }
        drop(
            surface
                .acquire()
                .expect("restoring the device must restore acquire"),
        );

        latch.record(&CbOutcome::Lost {
            code: None,
            name: "injected",
        });
        let refusal = surface.acquire().expect_err("a latched acquire refuses");
        assert!(matches!(refusal, AcquireRefusal::LatchLost));
        assert_eq!(
            surface_present_failure(&refusal, false),
            SurfacePresentFailure::Validation
        );
        crate::stderr_line!(
            "failure mapping on {}: drift->Reconfigured (live), deviceless \
             nil->Timeout/Occluded (live, hint-split), latch->Validation (live)",
            dev.name()
        );
    }
}
