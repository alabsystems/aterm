// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! W5 — THE METAL PRESENT ARM (map §5 W5, pass graph §2 Submit B).
//!
//! Three things live here, each the Metal twin of a seam `renderer.rs`'s
//! wgpu present path already has:
//!
//! 1. [`MetalWindowSurface`] — the shape `GpuSurface` grows on the flip: one
//!    [`Swapchain`] plus the retained config it was last configured to, with
//!    the PER-PRESENT RECONCILE verb ([`MetalWindowSurface::reconcile`])
//!    that maps the wgpu path's alpha-mode/usage/EDR/size drift reconfigure
//!    onto the W1 `Swapchain::reconfigure`, and a drift-checked acquire.
//!
//! 2. [`AcquireRefusal`] + [`surface_present_failure`] — the
//!    `SurfacePresentFailure` mapping, each arm pinned to a MEASURED signal
//!    (the table on `surface_present_failure`).
//!
//! 3. [`encode_present_sequence`] — §2 Submit B as ONE command buffer, in
//!    the pass graph's exact order: compose copy → bloom extract → bloom
//!    composite → shimmer stage+refract → tray → the "aterm-gpu blit pass"
//!    onto the drawable with `setViewport`+scissor letterbox and the EDR/SDR
//!    crown IN THE SAME PASS → tap copy appended before commit. Every
//!    binding index arrives from THE PIPELINE TABLE's `BindSpec` column via
//!    the caller — this module never hardcodes a slot.
//!
//! Like the W4 seam, wgpu stays the live present arm: production constructs
//! none of this until W6's flip. The consumers are the W5 differentials
//! (`renderer.rs::metal_present_replay_for_test`, this module's tests) — the
//! same test-reached discipline as `MetalFrameRig`.

use std::sync::Arc;

use super::encoder::{CommandBuffer, RenderPassDesc, StoreAction};
use super::ffi::{
    self, ClearColor, LoadAction, MtlScissorRect, MtlViewport, Obj, PixelFormat, PrimitiveType,
};
use super::loss::LossLatch;
use super::resources::SealedTexture;
use super::swapchain::{Frame, Swapchain, SwapchainConfig};
use crate::renderer::SurfacePresentFailure;

/// The `GpuSurface` Metal variant's shape: one W1 swapchain + the config it
/// currently holds, so the per-present reconcile can detect drift without
/// re-querying the layer axis by axis.
pub(crate) struct MetalWindowSurface {
    swapchain: Swapchain,
    config: SwapchainConfig,
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
            swapchain: Swapchain::attached(device, parent, &config, latch)?,
            config,
        })
    }

    /// Headless twin — the shape every test drives (a `CAMetalLayer` vends
    /// drawables without a window).
    pub(crate) fn standalone(
        device: &ffi::Device,
        config: SwapchainConfig,
        latch: Arc<LossLatch>,
    ) -> Result<Self, String> {
        Ok(Self {
            swapchain: Swapchain::standalone(device, &config, latch)?,
            config,
        })
    }

    /// The config the swapchain currently holds.
    pub(crate) const fn config(&self) -> &SwapchainConfig {
        &self.config
    }

    /// The live `contentsScale` of the presented layer, read back off the
    /// `CAMetalLayer` itself. The points-per-pixel term CoreAnimation uses to
    /// lay the drawable down: it MUST equal the parent view's backing scale,
    /// or the device-pixel drawable composites as points (v0.69.0's 2x
    /// oversize + resample). Never echoed from a cached field — the gate asks
    /// the layer.
    pub(crate) fn contents_scale(&self) -> f64 {
        self.swapchain.contents_scale()
    }

    /// W6 flip-drill diagnostic — the LIVE layer's pacing-relevant state,
    /// read back off the actual `CAMetalLayer` (never echoed from config):
    /// `(display_sync_enabled, framebuffer_only, maximum_drawable_count,
    /// wants_edr)`. Exists because the on-glass A/B measured vsync-shaped
    /// acquire waits on the armed arm, and the first question is always
    /// whether the layer really holds the state the config mapped.
    pub(crate) fn layer_state(&self) -> (bool, bool, usize, bool) {
        (
            self.swapchain.display_sync_enabled(),
            self.swapchain.framebuffer_only(),
            self.swapchain.maximum_drawable_count(),
            self.swapchain.wants_extended_dynamic_range(),
        )
    }

    /// M3: whether this is the EDR (`Rgba16Float` extended-linear) target —
    /// `GpuSurface::is_hdr`'s twin.
    pub(crate) fn is_hdr(&self) -> bool {
        self.config.format == PixelFormat::Rgba16Float
    }

    /// THE PER-PRESENT RECONCILE — the Metal twin of the wgpu path's
    /// alpha-mode/usage drift check (`present_input_with_crop`): compare the
    /// wanted config against the retained one and reconfigure ONLY on a real
    /// change, so a steady present never touches the layer. The axes map:
    ///
    /// * wgpu `alpha_mode` Opaque↔PostMultiplied → `opaque`
    ///   (`CALayer.opaque`);
    /// * wgpu `usage` COPY_SRC armed/disarmed → `framebuffer_only`
    ///   (the tap's copy-out needs a non-framebufferOnly drawable);
    /// * wgpu format Bgra8↔Rgba16Float (EDR flip) → `format`;
    /// * `resize_surface` → `width`/`height`.
    ///
    /// Returns whether a reconfigure happened. The W1 reconfigure-storm test
    /// already leak-audits the verb itself; the sequence test below drives
    /// this reconcile mid-cycle.
    pub(crate) fn reconcile(
        &mut self,
        device: &ffi::Device,
        want: &SwapchainConfig,
    ) -> Result<bool, String> {
        // The layer's `contentsScale` is NOT one of the drift axes above and
        // cannot be: it is not in `SwapchainConfig` at all, it is owned by the
        // parent layer, and it changes with NO resize and no reconfigure when
        // a window is dragged between a Retina and a non-Retina display. So it
        // is polled here, ahead of the early-out — this is the poll that
        // replaces `raw-window-metal`'s KVO observer, and skipping it on a
        // steady present is what would let a display change go unnoticed.
        // Change-gated inside, so the steady cost is one property read.
        self.swapchain.sync_layer_geometry();
        let drift = self.config.format != want.format
            || self.config.width != want.width
            || self.config.height != want.height
            || self.config.framebuffer_only != want.framebuffer_only
            || self.config.display_sync != want.display_sync
            || self.config.maximum_drawables != want.maximum_drawables
            || self.config.opaque != want.opaque;
        if !drift {
            return Ok(false);
        }
        self.swapchain.reconfigure(device, want)?;
        self.config = *want;
        Ok(true)
    }

    /// Acquire the next drawable, refusing TYPED instead of stringly: the
    /// loss latch and the bounded nil-acquire each get their own arm, and a
    /// drawable whose texture no longer matches the retained config (the
    /// layer was mutated behind the surface's back) is returned as
    /// [`AcquireRefusal::Drift`] with both geometries named — the caller
    /// reconfigures and skips the frame, exactly like wgpu's Outdated arm.
    pub(crate) fn acquire(&mut self) -> Result<Frame<'_>, AcquireRefusal> {
        if let Some(reason) = self.swapchain.latch().reason() {
            return Err(AcquireRefusal::LatchLost(reason.to_owned()));
        }
        // ONE acquire, geometry-checked on the vended frame itself. This was
        // a probe-then-reacquire two-phase (a scoped throwaway acquire for
        // the geometry check, then a second `nextDrawable` for the real
        // frame) — and the W6 flip drill MEASURED that shape starving the
        // pool on glass: a just-dropped unpresented drawable is not instantly
        // re-vendable, so every present's second `nextDrawable` sat in
        // CoreAnimation's internal usleep wait (`sample`d:
        // `CAMetalLayerPrivateNextDrawableLocked` → `usleep`), pinning
        // `last_acquire_wait_ns` at ~a display period per present —
        // p50 ~10-16 ms vs the wgpu arm's ~0.02 ms, at EVERY cadence and
        // window size. The wanted geometry is copied out first so the error
        // arm borrows nothing from the live frame.
        let (want_w, want_h) = (self.config.width, self.config.height);
        let frame = match self.swapchain.acquire() {
            Ok(f) => f,
            Err(detail) => return Err(AcquireRefusal::AcquireNil { detail }),
        };
        let (tw, th) = (
            ffi::texture_width(frame.texture()),
            ffi::texture_height(frame.texture()),
        );
        if (tw, th) != (want_w, want_h) {
            // Dropping the frame returns the drawable to the pool unpresented.
            return Err(AcquireRefusal::Drift {
                got: (tw, th),
                want: (want_w, want_h),
            });
        }
        Ok(frame)
    }

    /// The loss latch this surface answers to — the `device_lost()` hook's
    /// wiring point (see `GpuContext::wire_metal_loss_latch`).
    pub(crate) fn latch(&self) -> Arc<LossLatch> {
        Arc::clone(self.swapchain.latch())
    }
}

/// W6a — the winit view's backing `CALayer`, from a raw-window-handle target:
/// the PARENT [`MetalWindowSurface::attached`] hangs the first-party
/// swapchain under. `None` for a non-AppKit handle or a layerless view (a
/// winit macOS window is always layer-backed). Must run on the MAIN THREAD
/// (an AppKit property read), which is where the frontend attaches windows.
pub(crate) fn parent_layer_of<W: raw_window_handle::HasWindowHandle>(target: &W) -> Option<Obj> {
    let raw = target.window_handle().ok()?.as_raw();
    let raw_window_handle::RawWindowHandle::AppKit(h) = raw else {
        return None;
    };
    let view: ffi::Id = h.ns_view.as_ptr().cast();
    // SAFETY: `ns_view` is a live NSView pointer for the duration of the
    // attach call (raw-window-handle's contract); `layer` is a +0 property
    // read retained to +1 by `Obj::retain`.
    unsafe {
        let f: unsafe extern "C" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::msg();
        Obj::retain(f(view, ffi::sel(c"layer")))
    }
}

/// Why a Metal acquire vended nothing — the typed pre-image of
/// [`SurfacePresentFailure`].
#[derive(Debug)]
pub(crate) enum AcquireRefusal {
    /// The process device-loss latch is set (acquire refuses before FFI).
    LatchLost(String),
    /// The drawable's texture geometry no longer matches the retained config
    /// — the layer was resized/mutated externally.
    Drift {
        got: (usize, usize),
        want: (usize, usize),
    },
    /// `nextDrawable` returned nil after the BOUNDED wait
    /// (`allowsNextDrawableTimeout=YES`): pool exhausted or the window
    /// server is throttling an invisible window.
    AcquireNil { detail: String },
}

/// The `SurfacePresentFailure` mapping, one arm per measured signal:
///
/// | failure | Metal signal (this arm) | wgpu-hal 29.0.3 Metal signal (measured, `metal/surface.rs`) |
/// |---|---|---|
/// | `Validation` | the process [`LossLatch`] is set — acquire/present refuse before FFI | wgpu-core validation (hal's Metal acquire never produces it) |
/// | `Reconfigured` | drawable texture extent ≠ retained config (drift check at acquire) | never produced by hal's Metal acquire — Outdated/Lost come from wgpu-core config drift |
/// | `Occluded` | **no CAMetalLayer signal exists** — the hint is `NSWindow.occlusionState`'s visible bit, supplied by the frontend | identical: hal walks layer→delegate→window and checks `occlusionState & (1<<1)` (`surface.rs:129-155`, the wgpu#8309 workaround) — a WINDOW signal, not a Metal one |
/// | `Timeout` | `nextDrawable` nil after the bounded wait (`allowsNextDrawableTimeout=YES`, pinned by the S1 readback; the ~1 s bound is `CAMetalLayer.h`'s documented timeout — compositor-only, so headless the MEASURED nils are the deviceless layer's 0.000 s (S4) and never-starve ms vends (S5), and the ~1 s wait itself stays unmeasured first-party until W6's on-window drill) | `nextDrawable` nil (`surface.rs:166`; hal sets `allowsNextDrawableTimeout=false` at `surface.rs:103`, so its nil blocks unboundedly first) |
///
/// The occlusion hint is a parameter precisely because the layer cannot
/// answer it: the frontend owns the `NSWindow`, so the frontend supplies the
/// bit (winit surfaces it as `WindowEvent::Occluded`). A nil acquire WITH the
/// hint set maps to `Occluded` (park until stimulus); without it, `Timeout`
/// (bounded retry) — the same retry policy split the gui applies today.
pub(crate) fn surface_present_failure(
    refusal: &AcquireRefusal,
    occluded_hint: bool,
) -> SurfacePresentFailure {
    match refusal {
        AcquireRefusal::LatchLost(_) => SurfacePresentFailure::Validation,
        AcquireRefusal::Drift { .. } => SurfacePresentFailure::Reconfigured,
        AcquireRefusal::AcquireNil { .. } if occluded_hint => SurfacePresentFailure::Occluded,
        AcquireRefusal::AcquireNil { .. } => SurfacePresentFailure::Timeout,
    }
}

/// One fullscreen post pass over the throwaway present copy (bloom
/// composite, shimmer refract) or the drawable (blit): the row's PSO plus
/// its `BindSpec::POST_FS` bindings, spelled by the caller off the table.
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

/// The bloom pair (§2: "bloom extract pass" + "bloom composite pass").
pub(crate) struct BloomPasses<'a> {
    /// Half-res target, cleared TRANSPARENT, drawn with row 4 `GlowAdd`.
    pub(crate) half_target: &'a SealedTexture,
    pub(crate) extract_pso: &'a Obj,
    /// The 16-byte cell `Uniforms` block at row 4's `vertex_uniform` slot.
    pub(crate) cell_uniform: &'a Obj,
    pub(crate) cell_uniform_slot: usize,
    /// The UNGATED glow stream — the extract sub-stream the wgpu arm draws
    /// as `extract_first..extract_first+n` (the caller slices; see the
    /// base-instance note in the W5 addendum).
    pub(crate) extract_stream: &'a Obj,
    /// BYTE offset of the extract window's first instance inside
    /// `extract_stream` (`extract_first * stride` — the W1 offset verb; the
    /// crown keeps drawing the exact set from 0).
    pub(crate) extract_offset: usize,
    pub(crate) extract_count: usize,
    /// Row 12 `bloom` composited onto the present copy (SCREEN blend), with
    /// the byte-invisible dilated-bbox scissor.
    pub(crate) composite: PostPass<'a>,
    pub(crate) composite_scissor: Option<[u32; 4]>,
}

/// The shimmer pass (§2: stage copy + scissored refract, row 13).
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

/// The tray pass (§2: row 17, 4-vertex strip, straight-alpha src-over).
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

/// The crown, drawn IN THE BLIT PASS (§2: row 14 `hdr_glow` on f16, else
/// row 15 `sdr_glow`), crown-scissored, instanced over the ungated glow
/// stream.
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

/// §2 Submit B, minus acquire/present (the caller owns the frame boundary).
pub(crate) struct PresentSequence<'a> {
    /// The clean offscreen (base + aurora — the scissor base).
    pub(crate) offscreen: &'a SealedTexture,
    /// The throwaway present copy the effects composite over and the blit
    /// samples. `None` = the effect-free present: the blit samples
    /// `offscreen` directly and the compose copy is skipped (the wgpu
    /// `use_present_off == false` arm).
    pub(crate) present_off: Option<&'a SealedTexture>,
    /// The compose copy's rect (half-open); the first sync copies the full
    /// frame. Ignored when `present_off` is `None`.
    pub(crate) copy_rect: [u32; 4],
    pub(crate) bloom: Option<BloomPasses<'a>>,
    pub(crate) shimmer: Option<ShimmerPass<'a>>,
    pub(crate) tray: Option<TrayPass<'a>>,
    /// The letterbox blit (row 16) onto the drawable: POST_FS binds; the
    /// clear is the live terminal background.
    pub(crate) blit: PostPass<'a>,
    pub(crate) clear: ClearColor,
    pub(crate) crown: Option<CrownPass<'a>>,
}

/// Encode §2 Submit B onto `cb`, targeting `dest` (the drawable's sealed
/// render target) at `dest_w`×`dest_h`. Record order IS the wgpu path's
/// submission order — compose copy, bloom extract, bloom composite, shimmer
/// stage+refract, tray, then ONE "aterm-gpu blit pass" carrying the
/// viewport+scissor letterbox blit AND the crown — so nothing is reordered
/// across the seam. The tap copy (when armed) is appended by the CALLER
/// after this returns, before commit — the same "appended to the encoder
/// before submit" ordering the wgpu taps use.
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

    // 5. THE "aterm-gpu blit pass": Clear(live bg), full-destination
    //    viewport + scissor (the letterbox — fs_blit places the frame at
    //    content_off and paints the bands), the row 16 fullscreen triangle,
    //    then — IN THE SAME PASS — the crown, crown-scissored, instanced.
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
    use crate::pipeline_table::Pipeline;

    fn device() -> Option<ffi::Device> {
        let d = ffi::Device::system_default();
        if d.is_none() {
            eprintln!("SKIP: no Metal device on this machine");
        }
        d
    }

    /// A bare `CALayer` at a caller-chosen `contentsScale` — the headless
    /// stand-in for the winit view's backing layer on a Retina display.
    fn parent_at_scale(scale: f64) -> Obj {
        let _pool = ffi::AutoreleasePool::new();
        // SAFETY: alloc/init (+1, exactly as `Swapchain::new_layer`) then a
        // scalar `CGFloat` setter on the live layer.
        unsafe {
            let alloc: unsafe extern "C" fn(ffi::ClassPtr, ffi::Sel) -> ffi::Id = ffi::msg();
            let raw = alloc(ffi::class(c"CALayer"), ffi::sel(c"alloc"));
            let init: unsafe extern "C" fn(ffi::Id, ffi::Sel) -> ffi::Id = ffi::msg();
            let layer = Obj::from_owned(init(raw, ffi::sel(c"init"))).expect("CALayer init");
            let set: unsafe extern "C" fn(ffi::Id, ffi::Sel, f64) = ffi::msg();
            set(layer.id(), ffi::sel(c"setContentsScale:"), scale);
            layer
        }
    }

    /// W7 — THE RETINA GATE AT THE PRESENT SEAM. `contentsScale` is owned by
    /// the parent layer and is NOT a `SwapchainConfig` axis, so it can never
    /// appear in `reconcile`'s drift set; the sync therefore has to run AHEAD
    /// of the no-drift early-out. This test is the one that fails if someone
    /// "tidies" the poll inside the `if drift` block: the config here never
    /// moves, so `reconcile` returns `false` every time, and the scale must
    /// still track the parent.
    ///
    /// It is reachable headlessly for the same reason S5 is — a layer tree
    /// needs no window, and `contentsScale` propagation is a property read,
    /// not a composite.
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

    /// # Safety
    /// `T` must be `repr(C)` and padding-insensitive; the bytes go straight
    /// into a GPU-visible shared buffer.
    unsafe fn as_bytes<T: Copy>(v: &T) -> &[u8] {
        // SAFETY: `T: Copy` + `repr(C)`; read-only, lives only for the copy.
        unsafe { std::slice::from_raw_parts(std::ptr::from_ref(v).cast::<u8>(), size_of::<T>()) }
    }

    /// The 96-byte blit uniform, restated `repr(C)` (the swapchain tests'
    /// spelling — the production `BlitUniform` layout is pinned against
    /// `blit.metal` by `MetalBlit::UNIFORM_BYTES` and the differentials).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BlitU {
        flag: u32,
        overlay: u32,
        border_px: f32,
        encode_srgb: f32,
        accent: [f32; 4],
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

    /// W5 — THE FAILURE MAPPING, live where a healthy GPU can produce the
    /// signal and pure where it cannot:
    ///
    /// * latch-lost → `Validation`: LIVE — an injected `Lost` outcome makes
    ///   the next acquire refuse before FFI;
    /// * config drift → `Reconfigured`: LIVE — the drawable's own texture
    ///   extent disagrees with the retained config (the layer mutated behind
    ///   the surface's back, simulated by desyncing the retained config);
    /// * bounded nil → `Timeout`/`Occluded`: LIVE — the layer's device is
    ///   stripped (S4's deterministic stand-in; a deviceless layer returns
    ///   nil in ~0.000 s — the ~1 s compositor-paced wait is out of headless
    ///   reach, `CAMetalLayer.h`'s documented bound riding the S1
    ///   `allowsNextDrawableTimeout == YES` readback) and the surface's own
    ///   acquire produces the real `AcquireNil`, wall-clock bounded; the
    ///   MAPPING is pinned on it — hint set ⇒ `Occluded` (the window-side
    ///   `NSWindow.occlusionState` signal wgpu-hal 29.0.3 also uses; the
    ///   layer has none), hint clear ⇒ `Timeout` — and restoring the device
    ///   restores acquire (the recovery half).
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

        // DRIFT, live: desync the retained config from the layer — the next
        // acquire measures the drawable texture at the LAYER's 16x16 against
        // the retained 24x16 and refuses Reconfigured-shaped.
        surface.config.width = 24;
        let refusal = surface.acquire().expect_err("a drifted acquire refuses");
        assert!(
            matches!(
                refusal,
                AcquireRefusal::Drift {
                    got: (16, 16),
                    want: (24, 16)
                }
            ),
            "drift names both geometries: {refusal:?}"
        );
        assert_eq!(
            surface_present_failure(&refusal, false),
            SurfacePresentFailure::Reconfigured
        );
        surface.config.width = 16;

        // THE NIL ARM, live: strip the layer's device (S4's deterministic
        // stand-in for "the pool cannot serve this acquire") and the
        // surface's own acquire produces a REAL `AcquireNil`, wall-clocked.
        // SAFETY: `setDevice:` accepts nil (nullable, CAMetalLayer.h:66);
        // the layer stays live.
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
            matches!(nil, AcquireRefusal::AcquireNil { .. }),
            "the deviceless refusal is the nil arm, not drift/latch: {nil:?}"
        );
        assert!(
            waited < std::time::Duration::from_secs(8),
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

        // LATCH-LOST, live: inject the loss, and acquire refuses before FFI.
        latch.record(&CbOutcome::Lost {
            code: None,
            name: "injected",
        });
        let refusal = surface.acquire().expect_err("a latched acquire refuses");
        assert!(matches!(refusal, AcquireRefusal::LatchLost(_)));
        assert_eq!(
            surface_present_failure(&refusal, false),
            SurfacePresentFailure::Validation
        );
        eprintln!(
            "failure mapping on {}: drift->Reconfigured (live), deviceless \
             nil->Timeout/Occluded (live, hint-split), latch->Validation (live)",
            dev.name()
        );
    }

    /// W5 — THE FRAME CYCLE WITH THE CROWN IN THE BLIT PASS, plus the
    /// per-present reconcile verb driven mid-cycle:
    ///
    /// * frames 0-1: SDR (Bgra8) — letterboxed blit only; the band pixel is
    ///   the clear colour, the content pixel is the offscreen's;
    /// * frame 2: SDR + the row 15 `sdr_glow` crown IN THE SAME PASS,
    ///   crown-scissored — the crowned pixel brightens vs frame 1, a pixel
    ///   outside the crown scissor does not move;
    /// * frame 3: reconcile flips `opaque` (the M5 translucency axis) — a
    ///   real reconfigure, presenting continues;
    /// * frames 4-5: reconcile flips the format to `Rgba16Float` (the EDR
    ///   axis) — the row 14 `hdr_glow` crown adds >0 f16 light inside the
    ///   scissor and none outside.
    ///
    /// Every present is committed on the ONE session queue, tap-read before
    /// present, and status-polled to terminal; the latch stays healthy.
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
        // SAFETY: fresh shared texture, tight stride.
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
            accent: [0.0; 4],
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
        // The crown scissor CLIPS THE QUAD, judged into the fixture: the
        // original scissor was the whole content region and the only quad sat
        // entirely inside it, so a plant replacing the crown's scissor with
        // the full destination rode every test GREEN — the "outside" asserts
        // were satisfied by quad geometry alone. Width 10 puts the quad's
        // dest x 10..18 half in (10..14), half clipped (14..18), and the
        // clipped_px probe below is what makes the scissor load-bearing.
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
            // The per-present reconcile: flip the M5 translucency axis at 3,
            // the EDR axis at 4 — each a REAL reconfigure through the verb.
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
            // The sticky-colorspace lesson, pinned AT THE PRESENT ARM: the
            // layer's space must track the format across the mid-cycle EDR
            // flip (S10 pins the verb; this pins the arm's use of it — a
            // reconcile that skipped the space would present every EDR frame
            // tagged sRGB, or worse, leave scRGB sticky after a flip back).
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
        eprintln!(
            "present arm on {}: 6 frames, reconcile fired on the opaque + EDR \
             flips, crown drew IN the blit pass on both formats, letterbox + \
             content verified per frame",
            dev.name()
        );
    }
}
