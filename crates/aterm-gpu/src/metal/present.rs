// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Metal present arm, twin of the wgpu present path's seams:
//!
//! 1. [`MetalWindowSurface`]: one [`Swapchain`] plus the config it was last
//!    configured to, with the per-present reconcile
//!    ([`MetalWindowSurface::reconcile`]) mapping alpha-mode, usage, HDR and
//!    size drift onto `Swapchain::reconfigure`, and a drift-checked acquire.
//! 2. [`AcquireRefusal`] and [`surface_present_failure`]: the
//!    `SurfacePresentFailure` mapping (table on `surface_present_failure`).
//! 3. [`encode_present_sequence`]: Submit B as one command buffer, in order:
//!    compose copy, bloom extract, bloom composite, shimmer stage and refract,
//!    tray, then the blit pass onto the drawable (viewport and scissor
//!    letterbox, with the HDR/SDR crown in the same pass), and the tap copy
//!    appended before commit. Binding indices come from the table's
//!    `BindSpec` via the caller.
//!
//! Attached GUI surfaces use a demand-driven drawable worker; standalone tests
//! acquire synchronously.

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

/// The Metal `GpuSurface` shape: one swapchain plus the config it holds, so the
/// reconcile detects drift without re-reading the layer.
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

// The holder permits retirement without changing the borrow-shaped Frame API.
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
/// has stopped. Only the worker waits for that dispatch. The outer pool covers
/// `Obj`'s driver-side dealloc work.
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
            // Disconnected means acquisition already ended; the returned
            // payload can be dropped here. No blocking send or worker join.
            drop(worker.close(retirement));
        }
    }
}

impl MetalWindowSurface {
    /// Attach under a parent `CALayer` (the winit view's layer — the stacking
    /// tests prove coexistence with a leftover wgpu layer).
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

    /// Headless twin — the shape every test drives (a `CAMetalLayer` vends
    /// drawables without a window).
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

    pub(crate) fn async_acquire(&self) -> bool {
        self.worker.is_some()
    }

    /// Request the next frame's drawable now.
    ///
    /// Without this, the only request happened inside [`Self::acquire`], so
    /// every frame composed once, found no drawable, requested one and
    /// composed again. Called after a successful present, not at the top of a
    /// redraw: a redraw that returns early would hand back an unpresented
    /// drawable, which is not immediately re-vendable and stalls later acquires
    /// for about a display period.
    ///
    /// Idempotent and non-blocking.
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

    /// Invalidating never touches the layer or waits for the worker. In-flight
    /// results retain their old epoch, including resize-away-and-back cases.
    pub(crate) fn discard_pending_acquire(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("surface generation exhausted");
        self.ready = None;
        self.collect_acquire();
    }

    pub(crate) fn take_completed_acquire_wait_ns(&mut self) -> Option<u64> {
        self.completed_wait_ns.take()
    }

    /// The latest completed acquisition's queue span (admission to worker
    /// dequeue), published separately from
    /// [`Self::take_completed_acquire_wait_ns`] because the two legs have
    /// different causes (scheduling vs CoreAnimation).
    pub(crate) fn take_completed_acquire_queue_ns(&mut self) -> Option<u64> {
        self.completed_queue_ns.take()
    }

    fn collect_acquire(&mut self) {
        let Some(completed) = self.worker.as_mut().and_then(AcquireWorker::try_take) else {
            return;
        };
        match completed.outcome {
            AcquireOutcome::Ready(output) => {
                self.completed_wait_ns = Some(output.wait_ns);
                // Paired with the wait, so both legs of one acquisition are
                // read together. Terminal outcomes book neither.
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

    /// The config the swapchain currently holds.
    #[cfg(not(wgpu_arm))]
    pub(crate) const fn config(&self) -> &SwapchainConfig {
        &self.config
    }

    /// The live `contentsScale` read off the `CAMetalLayer`; must equal the
    /// parent view's backing scale or the drawable composites as points.
    #[cfg(test)]
    pub(crate) fn contents_scale(&self) -> f64 {
        self.swapchain.contents_scale()
    }

    /// Whether this is the EDR (`Rgba16Float` extended-linear) target;
    /// `GpuSurface::is_hdr`'s twin.
    #[cfg(test)]
    pub(crate) fn is_hdr(&self) -> bool {
        self.config.format == PixelFormat::Rgba16Float
    }

    /// Per-present reconcile, Metal twin of the wgpu drift check in
    /// `present_input_with_crop`: reconfigure only when the wanted config
    /// differs from the retained one. Axis mapping:
    ///
    /// * wgpu `alpha_mode` Opaque/PostMultiplied -> `opaque`;
    /// * wgpu `usage` `COPY_SRC` -> `framebuffer_only` (the tap needs a
    ///   non-framebufferOnly drawable);
    /// * wgpu format `Bgra8`/`Rgba16Float` -> `format`;
    /// * `resize_surface` -> `width`/`height`.
    ///
    /// Returns whether a reconfigure happened.
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
        // nextDrawable and geometry/configuration setters never overlap. The
        // UI records its latest desires and returns immediately while pending.
        if self.worker.as_ref().is_some_and(AcquireWorker::is_pending) {
            return Ok(false);
        }
        // `contentsScale` is not a config axis: the parent layer owns it and it
        // changes with no resize when a window moves between displays. Poll it
        // here, before the no-drift early-out. Change-gated.
        self.swapchain.sync_layer_geometry();
        let drift = self.config != *want;
        if !drift {
            return Ok(false);
        }
        self.swapchain.reconfigure(device, want)?;
        self.config = *want;
        Ok(true)
    }

    /// Acquire the next drawable with typed refusals: the loss latch and the
    /// bounded nil acquire each have an arm, and a drawable whose texture no
    /// longer matches the retained config is [`AcquireRefusal::Drift`] (the
    /// caller reconfigures and skips the frame, like wgpu's Outdated).
    pub(crate) fn acquire(&mut self) -> Result<Frame<'_>, AcquireRefusal> {
        if self.swapchain.latch().reason().is_some() {
            return Err(AcquireRefusal::LatchLost);
        }
        // One acquire, geometry-checked on the vended frame. A probe-then-acquire
        // shape stalls: a just-dropped drawable is not immediately re-vendable,
        // so the second `nextDrawable` waits about a display period. The wanted
        // geometry is copied first so the error arm borrows nothing.
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

/// The window view's backing `CALayer` from a raw-window-handle target: the
/// parent [`MetalWindowSurface::attached`] attaches under. `None` for a
/// non-AppKit handle or a layerless view. Main thread only.
pub(crate) fn parent_layer_of<W: raw_window_handle::HasWindowHandle>(target: &W) -> Option<Obj> {
    let raw = target.window_handle().ok()?.as_raw();
    let raw_window_handle::RawWindowHandle::AppKit(h) = raw else {
        return None;
    };
    let view: ffi::Id = h.ns_view.as_ptr().cast();
    // SAFETY: `ns_view` is a live NSView pointer for the attach call; `layer`
    // is a +0 property read retained to +1 by `Obj::retain`.
    unsafe {
        let f: unsafe extern "C" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::msg();
        Obj::retain(f(view, ffi::sel(c"layer")))
    }
}

/// The EDR potential of the target window's screen
/// (`NSScreen.maximumPotentialExtendedDynamicRangeColorComponentValue`, 1.0 on
/// an SDR-only screen). `None` for a non-AppKit handle, a view with no window,
/// or a window on no screen.
///
/// The attach gates the `Rgba16Float` pick through
/// `format_plan::hdr_swapchain_wants_f16_on_screen`, where `None` keeps the
/// pick. `GpuRenderer::upgrade_surface_for_screen` gates an 8-bit window's
/// upgrade through `format_plan::hdr_screen_upgrade_wants_f16`, where `None`
/// upgrades nothing. Not cached: each call reads `-window` and `-screen`
/// afresh. A not-yet-ordered-front window already reports its screen. Main
/// thread only.
#[cfg(not(wgpu_arm))]
pub(crate) fn screen_edr_potential_of<W: raw_window_handle::HasWindowHandle>(
    target: &W,
) -> Option<f32> {
    let raw = target.window_handle().ok()?.as_raw();
    let raw_window_handle::RawWindowHandle::AppKit(h) = raw else {
        return None;
    };
    let view: ffi::Id = h.ns_view.as_ptr().cast();
    // SAFETY: `ns_view` is live while `target` is borrowed (the window handle
    // borrows it). Callers run on the main thread. `-window` and `-screen` are
    // +0 reads, nil-checked. The potential getter exists on macOS 10.15+ (the
    // app requires 11.0) and returns a `CGFloat` in the FP register plain
    // `objc_msgSend` returns. Nothing is retained.
    unsafe {
        let obj: unsafe extern "C" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::msg();
        let window = obj(view, ffi::sel(c"window"));
        if window.is_null() {
            return None;
        }
        let screen = obj(window, ffi::sel(c"screen"));
        if screen.is_null() {
            return None;
        }
        let f64_of: unsafe extern "C" fn(ffi::Id, ffi::Sel) -> f64 = ffi::msg();
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

/// Why a Metal acquire vended nothing — the typed pre-image of
/// [`SurfacePresentFailure`].
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

/// The `SurfacePresentFailure` mapping:
///
/// | failure | Metal signal |
/// |---|---|
/// | `Validation` | the process [`LossLatch`] is set; acquire and present refuse before FFI |
/// | `Reconfigured` | drawable texture extent differs from the retained config |
/// | `Occluded` | nil acquire with the frontend's occlusion hint set (`NSWindow.occlusionState`; the layer has no such signal, and wgpu-hal reads the same window bit) |
/// | `Timeout` | `nextDrawable` nil after the bounded wait (`allowsNextDrawableTimeout=YES`) with the hint clear |
///
/// The occlusion hint is a parameter because the frontend owns the window
/// (winit's `WindowEvent::Occluded`). `Occluded` parks until a stimulus;
/// `Timeout` retries on a bound.
pub(crate) fn surface_present_failure(
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

/// One fullscreen post pass over the present copy (bloom composite, shimmer)
/// or the drawable (blit): the row's PSO and its `BindSpec::POST_FS` bindings.
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

/// The bloom pair: extract pass and composite pass.
pub(crate) struct BloomPasses<'a> {
    /// Half-res target, cleared TRANSPARENT, drawn with row 4 `GlowAdd`.
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

/// The shimmer pass: stage copy plus scissored refract (row 13).
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

/// The sub-row band shift: two staged whole-width copies over the present
/// copy, between the shimmer and the tray, using the same `BandShift` plan as
/// the wgpu arm (`shift_present_copy_band`). It never touches the offscreen,
/// which stays the untranslated scissor base during a glide.
pub(crate) struct BandShiftPass<'a> {
    pub(crate) shift: crate::renderer::BandShift,
    /// Frame-sized resident scratch (`WindowGpu::metal_shift_scratch`); an
    /// overlapping same-texture copy is undefined in Metal.
    pub(crate) scratch: &'a SealedTexture,
}

/// The incoming-row apron: one whole-width copy over the present copy after the
/// band shift and before the tray. The top `rows` px of the CPU-rastered
/// incoming row (`WindowGpu::metal_apron_tex`) land on the exposed strip at
/// `[dst_y, dst_y + rows)`, the same bytes as CPU `paint_incoming_strip`.
/// Refused without a present copy.
pub(crate) struct ApronStripPass<'a> {
    pub(crate) tex: &'a SealedTexture,
    /// First framebuffer row of the exposed strip (`y1 - frac`).
    pub(crate) dst_y: usize,
    /// Strip height (`frac`, clamped to the apron's rows).
    pub(crate) rows: usize,
}

/// The tray pass: row 17, 4-vertex strip, straight-alpha src-over.
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

/// The crown, drawn in the blit pass (`hdr_glow` on f16, else `sdr_glow`),
/// crown-scissored, instanced over the ungated glow stream.
pub(crate) struct CrownPass<'a> {
    pub(crate) pso: &'a Obj,
    /// `HdrGlowUniform` at the row's `vertex_uniform` slot AND its
    /// `fragment_buffers[0]` slot (`BindSpec::CROWN`: one block, two stages).
    pub(crate) uniform: &'a Obj,
    pub(crate) vertex_uniform_slot: usize,
    pub(crate) fragment_uniform_slot: usize,
    pub(crate) stream: &'a Obj,
    pub(crate) count: usize,
    /// `visible_source_scissor` — (x, y, w, h).
    pub(crate) scissor: (u32, u32, u32, u32),
}

/// Submit B, minus acquire/present (the caller owns the frame boundary).
pub(crate) struct PresentSequence<'a> {
    /// The clean offscreen (base + aurora — the scissor base).
    pub(crate) offscreen: &'a SealedTexture,
    /// The present copy the effects composite over and the blit samples. `None`
    /// for an effect-free present: the blit samples `offscreen` and the compose
    /// copy is skipped.
    pub(crate) present_off: Option<&'a SealedTexture>,
    /// The compose copy's half-open rect from `renderer::present_copy_rect`;
    /// full frame only for the first sync into a fresh copy. Ignored when
    /// `present_off` is `None`.
    pub(crate) copy_rect: [u32; 4],
    pub(crate) bloom: Option<BloomPasses<'a>>,
    pub(crate) shimmer: Option<ShimmerPass<'a>>,
    /// The sub-row scroll translate, over the copy (requires `present_off`;
    /// the sequence REFUSES it against the bare offscreen).
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

/// Encode Submit B onto `cb`, targeting `dest` (the drawable's sealed render
/// target) at `dest_w`×`dest_h`, in the wgpu path's order: compose copy, bloom
/// extract, bloom composite, shimmer, tray, then one blit pass carrying the
/// letterbox blit and the crown. The caller appends the tap copy before
/// commit.
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

    // 2. Bloom: extract into the cleared half-res target, then composite
    //    (SCREEN) over the copy, scissored to the dilated glow bbox.
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

    // 3. Shimmer: stage the sample rect, then the scissored refract — AFTER
    //    the halo, so the haze refracts the finished frame.
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

    // 3b. Sub-row band shift over the copy: after halo and haze (they ride the
    //     glide), before the tray (chrome stays pinned). Refused without a
    //     present copy, so it never lands on the scissor base.
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
        // 1. Stage the moved rows `[src_y, src_y+moved)` into the scratch.
        cb.copy_texture_sub_rect(composed, (0, src_y), bs.scratch, (0, 0), w, moved)?;
        // 2. Lay them back at `[dst_y, dst_y+moved)`; the exposed strip keeps
        //    the copy's pixels until 3c.
        cb.copy_texture_sub_rect(bs.scratch, (0, 0), composed, (0, dst_y), w, moved)?;
    }

    // 3c. The incoming row over the exposed strip, inside the band
    //     (`ApronStripPass`).
    if let Some(ap) = &seq.apron {
        if seq.present_off.is_none() {
            return Err(
                "the apron strip needs the present copy, never the offscreen (the \
                 scissor base stays untranslated)"
                    .to_owned(),
            );
        }
        // Clamp to the apron's width, as the CPU and wgpu arms do, so an
        // oversized copy does not make the copy verb refuse Submit B.
        let w = composed.width().min(ap.tex.width());
        let rows = ap.rows.min(ap.tex.height());
        if w > 0 && rows > 0 {
            cb.copy_texture_sub_rect(ap.tex, (0, 0), composed, (0, ap.dst_y), w, rows)?;
        }
    }

    // 4. Tray: the card over the finished copy (straight-alpha src-over,
    //    4-vertex strip) — chrome above halo and haze, the z-order law.
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

    // 5. The blit pass: Clear (live bg), full-destination viewport and scissor
    //    (`fs_blit` places the frame and paints the bands), the fullscreen
    //    triangle, then the crown in the same pass.
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

    use super::super::encoder::EncodeSession;
    use super::super::loss::{CbOutcome, LossLatch};
    use super::*;
    use crate::metal::test_support::{as_bytes, device, plain_calayer};
    use crate::pipeline_table::Pipeline;

    /// A bare `CALayer` at a caller-chosen `contentsScale` — the headless
    /// stand-in for the winit view's backing layer on a Retina display.
    fn parent_at_scale(scale: f64) -> Obj {
        let layer = plain_calayer();
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: a scalar `CGFloat` setter on the live layer.
        unsafe {
            let set: unsafe extern "C" fn(ffi::Id, ffi::Sel, f64) = ffi::msg();
            set(layer.id(), ffi::sel(c"setContentsScale:"), scale);
        }
        layer
    }

    /// `contentsScale` is not a config axis, so the sync must run before
    /// `reconcile`'s no-drift early-out: with a constant config, `reconcile`
    /// returns `false` every time and the scale must still follow the parent.
    #[test]
    fn the_no_drift_reconcile_still_tracks_the_parents_contents_scale() {
        let Some(dev) = device() else { return };
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

        // The display drag, with the config held EXACTLY where it was.
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: scalar `CGFloat` setter on the live parent layer.
        unsafe {
            let set: unsafe extern "C" fn(ffi::Id, ffi::Sel, f64) = ffi::msg();
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

    /// The 112-byte blit uniform as `repr(C)`; the production layout is checked
    /// by `MetalBlit::UNIFORM_BYTES` and the differentials.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BlitU {
        flag: u32,
        overlay: u32,
        border_px: f32,
        encode_srgb: f32,
        accent: [f32; 3],
        chrome_y1: f32,
        dims: [f32; 2],
        wash_a: f32,
        border_a: f32,
        band: [f32; 4],
        content_off: [f32; 2],
        hdr: f32,
        translucent: f32,
        sdr_white_scale: f32,
        visible_y: f32,
        visible_h: f32,
        premult: f32,
        chrome_y0: f32,
        chrome_pad: [f32; 3],
    }

    /// The 32-byte crown uniform (`HdrGlowUniform`'s layout).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CrownU {
        screen: [f32; 2],
        content_off: [f32; 2],
        boost: f32,
        headroom: f32,
        _pad: [f32; 2],
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

    /// The failure mapping:
    ///
    /// * latch lost -> `Validation`: an injected `Lost` makes the next acquire
    ///   refuse before FFI;
    /// * config drift -> `Reconfigured`: the drawable extent disagrees with a
    ///   desynced retained config;
    /// * bounded nil -> `Timeout`/`Occluded`: with the layer's device stripped
    ///   the acquire returns a real `AcquireNil`; the hint set maps to
    ///   `Occluded`, clear to `Timeout`, and restoring the device restores
    ///   acquire.
    #[test]
    fn acquire_refusals_map_to_the_wgpu_failure_contract() {
        let Some(dev) = device() else { return };
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

        // Drift: the layer's drawable is 16x16 but the retained config says
        // 24x16.
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

        // The nil arm: strip the layer's device so the acquire returns nil.
        // SAFETY: `setDevice:` accepts nil; the layer stays live.
        unsafe {
            let set_obj: unsafe extern "C" fn(ffi::Id, ffi::Sel, ffi::Id) = ffi::msg();
            set_obj(
                surface.swapchain.layer_ptr(),
                ffi::sel(c"setDevice:"),
                std::ptr::null_mut(),
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
        // A hang detector: the deviceless nil is immediate and a parked thread
        // never returns, so a minute loses nothing 8 s caught.
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
        // Recovery: hand the device back and the surface vends again.
        // SAFETY: as the strip above, with the real device.
        unsafe {
            let set_obj: unsafe extern "C" fn(ffi::Id, ffi::Sel, ffi::Id) = ffi::msg();
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

        // Latch lost: inject the loss; acquire refuses before FFI.
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

    /// The frame cycle with the crown in the blit pass and the reconcile driven
    /// mid-cycle:
    ///
    /// * frames 0-1: SDR, letterboxed blit only (band pixel is the clear,
    ///   content pixel the offscreen's);
    /// * frame 2: SDR with the `sdr_glow` crown, crown-scissored (inside
    ///   brightens, outside unchanged);
    /// * frame 3: reconcile flips `opaque`;
    /// * frames 4-5: reconcile flips to `Rgba16Float`; the `hdr_glow` crown adds
    ///   f16 light inside the scissor only.
    ///
    /// Every present uses the one session queue, is tap-read before present
    /// and polled to terminal; the latch stays healthy.
    #[test]
    fn the_present_arm_cycles_reconciles_and_crowns_on_the_drawable() {
        let Some(dev) = device() else { return };
        let _pool = ffi::AutoreleasePool::new();
        const W: usize = 32;
        const H: usize = 24;
        // The letterboxed content: 24x18 at offset (4, 3).
        const CW: usize = 24;
        const CH: usize = 18;
        const OFF: (usize, usize) = (4, 3);

        let latch = Arc::new(LossLatch::new());
        let session = EncodeSession::new(&dev, Arc::clone(&latch)).expect("session");
        let mint = super::super::resources::MetalResourceDevice::new(&dev, Arc::clone(&latch));
        let usage = ffi::TEXTURE_USAGE_RENDER_TARGET | ffi::TEXTURE_USAGE_SHADER_READ;

        let mut config = SwapchainConfig {
            format: PixelFormat::Bgra8Unorm,
            width: W,
            height: H,
            framebuffer_only: false,
            display_sync: false,
            maximum_drawables: 3,
            opaque: true,
        };
        let mut surface =
            MetalWindowSurface::standalone(&dev, config, Arc::clone(&latch)).expect("surface");

        // A mid-grey offscreen (CW x CH) so the SCREEN-blend crown has
        // headroom to brighten into.
        let offscreen = mint
            .texture_2d(PixelFormat::Rgba8Unorm, CW, CH, usage)
            .expect("offscreen");
        let grey = vec![0x60u8; CW * CH * 4];
        // SAFETY: fresh managed texture, tight stride.
        unsafe {
            ffi::texture_upload(
                offscreen.obj(),
                ffi::MtlRegion::full_2d(CW, CH),
                &grey,
                CW * 4,
            );
        }
        let nearest = mint
            .sampler(ffi::SamplerDesc::NEAREST_CLAMP)
            .expect("sampler");

        #[expect(clippy::cast_precision_loss, reason = "test extents")]
        let blit_u = BlitU {
            flag: 0,
            overlay: 0,
            border_px: 0.0,
            encode_srgb: 0.0,
            accent: [0.0; 3],
            chrome_y1: 0.0,
            dims: [CW as f32, CH as f32],
            wash_a: 0.0,
            border_a: 0.0,
            band: [1.0, 0.0, 0.0, 1.0], // red letterbox band
            content_off: [OFF.0 as f32, OFF.1 as f32],
            hdr: 0.0,
            translucent: 0.0,
            sdr_white_scale: 1.0,
            visible_y: 0.0,
            visible_h: CH as f32,
            premult: 0.0,
            chrome_y0: 0.0,
            chrome_pad: [0.0; 3],
        };
        let blit_ubuf = dev.new_buffer(size_of::<BlitU>()).expect("blit uniform");
        // SAFETY: repr(C) into an exactly-sized fresh shared buffer.
        unsafe { ffi::buffer_write(&blit_ubuf, as_bytes(&blit_u)) };

        #[expect(clippy::cast_precision_loss, reason = "test extents")]
        let crown_u = CrownU {
            screen: [W as f32, H as f32],
            content_off: [OFF.0 as f32, OFF.1 as f32],
            boost: 1.0,
            headroom: 0.35,
            _pad: [0.0; 2],
        };
        let crown_ubuf = dev.new_buffer(size_of::<CrownU>()).expect("crown uniform");
        // SAFETY: as above.
        unsafe { ffi::buffer_write(&crown_ubuf, as_bytes(&crown_u)) };
        // One glow quad: offscreen-px rect (6,4,8,6), warm colour.
        let mut stream_bytes: Vec<u8> = Vec::new();
        for v in [6u16, 4, 8, 6] {
            stream_bytes.extend_from_slice(&v.to_le_bytes());
        }
        stream_bytes.extend_from_slice(&[0xff, 0xa0, 0x40, 0xff]);
        let crown_stream = dev.new_buffer(stream_bytes.len()).expect("crown stream");
        // SAFETY: as above.
        unsafe { ffi::buffer_write(&crown_stream, &stream_bytes) };
        // The crown scissor clips the quad (dest x 10..18, scissor ends at 14),
        // so the scissor is load-bearing for the outside asserts.
        let crown_scissor = (OFF.0 as u32, OFF.1 as u32, 10u32, CH as u32);

        let blit_spec = Pipeline::Blit.spec();
        let blit_lib = super::super::pipelines::compile(&dev, blit_spec).expect("blit.metal");
        let blit_sdr =
            super::super::pipelines::build(&dev, &blit_lib, blit_spec, PixelFormat::Bgra8Unorm)
                .expect("blit row, SDR");
        let blit_edr =
            super::super::pipelines::build(&dev, &blit_lib, blit_spec, PixelFormat::Rgba16Float)
                .expect("blit row, EDR");
        let sdr_spec = Pipeline::SdrGlow.spec();
        let crown_lib = super::super::pipelines::compile(&dev, sdr_spec).expect("hdr_glow.metal");
        let sdr_crown =
            super::super::pipelines::build(&dev, &crown_lib, sdr_spec, PixelFormat::Bgra8Unorm)
                .expect("row 15 on the SDR drawable");
        let hdr_spec = Pipeline::HdrGlow.spec();
        let hdr_crown = super::super::pipelines::build(
            &dev,
            &crown_lib,
            hdr_spec,
            PixelFormat::Bgra8Unorm, // Edr role pins f16 regardless
        )
        .expect("row 14 on the EDR drawable");

        let crown_binds = |spec: &crate::pipeline_table::PipelineSpec| {
            (
                spec.binds.vertex_uniform.expect("crown vertex uniform") as usize,
                spec.binds.fragment_buffers[0] as usize,
            )
        };
        let (sdr_vu, sdr_fu) = crown_binds(sdr_spec);
        let (hdr_vu, hdr_fu) = crown_binds(hdr_spec);

        // Probe points: BAND (top-left corner), CONTENT outside the crown
        // rect, CROWNED inside it. Crown rect in dest px = OFF + (6..14, 4..10).
        let band_px = (0usize, 0usize);
        let content_px = (OFF.0 + 20, OFF.1 + 15);
        let crowned_px = (OFF.0 + 7, OFF.1 + 6);
        // Inside the quad's extent, OUTSIDE the crown scissor — the pixel the
        // clip must protect. If the crown paints here, the scissor was lost.
        let clipped_px = (OFF.0 + 12, OFF.1 + 6);

        let mut sdr_plain: Option<Vec<u8>> = None;
        let mut sdr_crowned: Option<Vec<u8>> = None;
        for frame_no in 0..6usize {
            // The per-present reconcile flips the translucency axis at 3 and
            // the EDR axis at 4, each a real reconfigure through the verb.
            if frame_no == 3 {
                config.opaque = false;
            }
            if frame_no == 4 {
                config.format = PixelFormat::Rgba16Float;
            }
            let reconfigured = surface.reconcile(&dev, &config).expect("reconcile");
            assert_eq!(
                reconfigured,
                frame_no == 3 || frame_no == 4,
                "frame {frame_no}: reconcile fires exactly on drift"
            );

            let is_hdr = surface.is_hdr();
            // The layer colorspace follows the format across the mid-cycle HDR
            // flip.
            let space = surface.swapchain.colorspace_name();
            if is_hdr {
                assert_eq!(
                    space.as_deref(),
                    Some("kCGColorSpaceExtendedLinearSRGB"),
                    "frame {frame_no}: the EDR drawable must carry extended-linear scRGB"
                );
                assert!(
                    surface.swapchain.wants_extended_dynamic_range(),
                    "frame {frame_no}: the EDR flip must opt the layer in"
                );
            } else {
                assert_eq!(
                    space.as_deref(),
                    Some("kCGColorSpaceSRGB"),
                    "frame {frame_no}: the SDR drawable must carry sRGB"
                );
                assert!(
                    !surface.swapchain.wants_extended_dynamic_range(),
                    "frame {frame_no}: SDR must not opt into EDR"
                );
            }
            let with_crown = frame_no == 2 || frame_no >= 4;
            let seq = PresentSequence {
                offscreen: &offscreen,
                present_off: None,
                copy_rect: [0, 0, 0, 0],
                bloom: None,
                shimmer: None,
                band_shift: None,
                apron: None,
                tray: None,
                blit: PostPass {
                    pso: if is_hdr { &blit_edr } else { &blit_sdr },
                    tex: offscreen.obj(),
                    tex_slot: blit_spec.binds.fragment_textures[0] as usize,
                    sampler: &nearest,
                    sampler_slot: blit_spec.binds.fragment_samplers[0] as usize,
                    uniform: &blit_ubuf,
                    uniform_slot: blit_spec.binds.fragment_buffers[0] as usize,
                },
                clear: ffi::ClearColor {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                },
                crown: with_crown.then_some(CrownPass {
                    pso: if is_hdr { &hdr_crown } else { &sdr_crown },
                    uniform: &crown_ubuf,
                    vertex_uniform_slot: if is_hdr { hdr_vu } else { sdr_vu },
                    fragment_uniform_slot: if is_hdr { hdr_fu } else { sdr_fu },
                    stream: &crown_stream,
                    count: 1,
                    scissor: crown_scissor,
                }),
            };

            let frame = surface
                .acquire()
                .unwrap_or_else(|r| panic!("frame {frame_no}: acquire refused: {r:?}"));
            let target = frame.render_target();
            let bpt = if is_hdr { 8 } else { 4 };
            let row = W * bpt;
            let readback = dev.new_buffer(row * H).expect("tap readback");
            let mut cb = session.begin().expect("cb");
            encode_present_sequence(&mut cb, &target, (W as u32, H as u32), &seq)
                .expect("the sequence encodes");
            cb.copy_texture_to_buffer(&target, W, H, &readback, row)
                .expect("tap copy");
            let submitted = cb.commit();
            let ticket = frame.present(&session).expect("present");
            assert_eq!(submitted.wait_outcome(), CbOutcome::Completed);
            assert_eq!(ticket.wait_outcome(), CbOutcome::Completed);
            assert!(!latch.is_lost(), "frame {frame_no}: healthy cycle");

            // SAFETY: shared storage; both command buffers terminal above.
            let bytes = unsafe { ffi::buffer_bytes(&readback, row * H) };
            let px4 = |b: &[u8], (x, y): (usize, usize)| -> [u8; 4] {
                let o = y * row + x * 4;
                [b[o], b[o + 1], b[o + 2], b[o + 3]]
            };
            if is_hdr {
                // f16 lanes: band red = (1,0,0), crowned pixel gains >0 light
                // in at least one lane vs the flat grey, outside stays grey.
                let half = |b: &[u8], (x, y): (usize, usize), lane: usize| -> u16 {
                    let o = y * row + x * 8 + lane * 2;
                    u16::from_le_bytes([b[o], b[o + 1]])
                };
                assert_eq!(
                    half(&bytes, band_px, 0),
                    0x3c00, // f16 1.0
                    "frame {frame_no}: the EDR band is the clear red"
                );
                let crowned = half(&bytes, crowned_px, 1);
                let content = half(&bytes, content_px, 1);
                assert!(
                    crowned > content,
                    "frame {frame_no}: the EDR crown must ADD light inside its \
                     scissor (crowned g={crowned:04x} vs content g={content:04x})"
                );
                let clipped = half(&bytes, clipped_px, 1);
                assert_eq!(
                    clipped, content,
                    "frame {frame_no}: the EDR crown must NOT paint outside its \
                     scissor — the quad crosses the clip edge and the clipped \
                     half must stay flat grey"
                );
            } else {
                assert_eq!(
                    px4(&bytes, band_px),
                    [0, 0, 0xff, 0xff], // BGRA red
                    "frame {frame_no}: the letterbox band is the clear colour"
                );
                assert_eq!(
                    px4(&bytes, content_px),
                    [0x60, 0x60, 0x60, 0xff],
                    "frame {frame_no}: the content region blits the offscreen"
                );
                if with_crown {
                    sdr_crowned = Some(bytes.clone());
                } else {
                    sdr_plain = Some(bytes.clone());
                }
            }
        }
        let (plain, crowned) = (
            sdr_plain.expect("an SDR plain frame ran"),
            sdr_crowned.expect("an SDR crowned frame ran"),
        );
        let at = |b: &[u8], (x, y): (usize, usize)| -> [u8; 4] {
            let o = y * W * 4 + x * 4;
            [b[o], b[o + 1], b[o + 2], b[o + 3]]
        };
        let (p, c) = (at(&plain, crowned_px), at(&crowned, crowned_px));
        assert!(
            c.iter()
                .take(3)
                .zip(p.iter().take(3))
                .any(|(cc, pp)| cc > pp),
            "the SDR crown must brighten inside its scissor: {p:02x?} -> {c:02x?}"
        );
        assert_eq!(
            at(&plain, content_px),
            at(&crowned, content_px),
            "a pixel outside the crown quad must not move"
        );
        assert_eq!(
            at(&plain, clipped_px),
            at(&crowned, clipped_px),
            "the crown must NOT paint outside its scissor — this pixel is \
             inside the quad's extent and outside the clip, and it is the \
             assert a full-destination scissor plant must turn RED"
        );
        crate::stderr_line!(
            "present arm on {}: 6 frames, reconcile fired on the opaque + EDR \
             flips, crown drew IN the blit pass on both formats, letterbox + \
             content verified per frame",
            dev.name()
        );
    }
}
