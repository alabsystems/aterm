// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shipped Windows build's window presents (docs/AUDIT-third-party-2026-10-15.md
//! GW.5's flip): the D3D11 arm owns each window's flip-model swap chain
//! (`d3d11::surface`), on the window itself or, on a DirectComposition visual
//! instance, on a visual whose premultiplied pixels blend over Mica or
//! Acrylic. With `hdr_glow` on it is f16, tagged scRGB, where the window's
//! output is in HDR mode, and a probe every 250 ms follows Windows HDR being
//! switched on or off under it, through the `HdrReconfigureRetag` plan the
//! wgpu arm ran. A build that also carries wgpu (`--features wgpu-oracle`)
//! attaches windows on wgpu.
//!
//! The pacing is wgpu-dx12's: one frame of latency on the chain's waitable,
//! waited on as the acquire, and a present that waits for no refresh
//! (`Present(0, 0)`, which a flip-model chain in a window shows at the next
//! composition without tearing), so no frame queues behind another. Every
//! frame the acquire takes ends in one present: a frame that fails after its
//! wait stays held by the chain for the next one
//! (`D3dWindowSurface::wait_for_frame`), and a wait that times out draws and
//! presents all the same, as wgpu-dx12's acquire does (wgpu-hal 29.0.3
//! `acquire_texture` drops the timeout), so a chain whose waitable lost a
//! frame gets it back from that present instead of timing out every frame
//! after.

use super::*;
use crate::d3d11::surface::{D3dPresented, D3dSwapKind, D3dWindowSurface};
use crate::device_layer::TexelFormat;
use aterm_win32::Win32::Foundation::HWND;
use d3d11_present::D3dDest;

/// The frames a window's chain queues before the waitable holds the next
/// one back (wgpu-dx12's `desired_maximum_frame_latency` there).
const FRAME_LATENCY: u32 = 1;

/// How long the acquire waits on the chain's waitable before the frame draws
/// and presents without it (wgpu-dx12's `acquire_texture` timeout).
const ACQUIRE_TIMEOUT_MS: u32 = 1000;

/// One window's D3D11 swap chain and the window it is made on, kept alive as
/// long as the chain.
pub(crate) struct D3dWindow {
    /// `None` only between dropping a chain and making its replacement (an
    /// HDR switch), since a window takes one flip-model chain at a time; a
    /// replacement that failed leaves it `None` and the loss latched.
    chain: Option<D3dWindowSurface>,
    hwnd: HWND,
    kind: D3dSwapKind,
    /// The acquires whose wait timed out, each of which presented anyway.
    timed_out_waits: u64,
    /// The window target, held for the chain's life.
    _target: Box<dyn std::any::Any + Send + Sync>,
}

#[cfg(test)]
impl D3dWindow {
    /// What the chain is made on.
    pub(crate) const fn kind(&self) -> D3dSwapKind {
        self.kind
    }

    /// Whether the window's output is in HDR mode.
    pub(crate) fn output_hdr(&self) -> bool {
        self.chain
            .as_ref()
            .is_some_and(D3dWindowSurface::output_hdr)
    }

    /// The acquires whose wait timed out.
    pub(crate) const fn timed_out_waits(&self) -> u64 {
        self.timed_out_waits
    }

    /// The chain, for a test that drives its waitable.
    pub(crate) fn chain_mut(&mut self) -> &mut D3dWindowSurface {
        self.chain.as_mut().expect("a chain")
    }
}

impl std::fmt::Debug for D3dWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dWindow")
            .field("chain", &self.chain)
            .field("kind", &self.kind)
            .field("timed_out_waits", &self.timed_out_waits)
            .finish_non_exhaustive()
    }
}

#[cfg(not(wgpu_arm))]
impl GpuRenderer {
    /// Attach `target`'s window: the arm (minted here if a headless frame did
    /// not come first) and a flip-model swap chain at `width` x `height`
    /// (clamped like every framebuffer) on the window, or on a
    /// DirectComposition visual when the context is the visual instance.
    /// `Err` sends the frontend to its fallback (`surface_attach_fallback`):
    /// the opaque HWND chain after a failed visual one, else the CPU renderer.
    pub(crate) fn d3d11_create_window_surface<W: WindowTarget>(
        &mut self,
        target: W,
        width: u32,
        height: u32,
    ) -> Result<GpuSurface, String> {
        use raw_window_handle::RawWindowHandle;
        if !self.d3d11_armed {
            return Err(self.d3d11_start_error());
        }
        let handle = target
            .window_handle()
            .map_err(|e| format!("the window has no handle: {e}"))?;
        let hwnd: HWND = match handle.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get() as HWND,
            other => return Err(format!("not a Win32 window: {other:?}")),
        };
        if !self.d3d11_arm_ready() {
            return Err(self.d3d11_start_error());
        }
        let width = self.clamp_fb_dim(width.max(1));
        let height = self.clamp_fb_dim(height.max(1));
        let kind = if self.ctx.visual_swapchain {
            D3dSwapKind::Composition
        } else {
            D3dSwapKind::Hwnd
        };
        // SAFETY: `hwnd` is `target`'s window, which the surface keeps alive
        // as long as the chain.
        let chain = unsafe { self.d3d11_make_chain(hwnd, kind, (width, height)) }.map_err(|e| {
            if kind == D3dSwapKind::Composition {
                format!("DirectComposition visual swapchain configure failed: {e}")
            } else {
                format!("window surface configure failed: {e}")
            }
        })?;
        if crate::verbose() {
            crate::stderr_line!(
                "aterm-gpu: D3D11 {kind:?} swap chain, {:?}, latency {FRAME_LATENCY}",
                chain.format()
            );
        }
        Ok(GpuSurface {
            acquire_id: next_surface_acquire_id(),
            neutral: NeutralSurfaceConfig {
                width,
                height,
                format: chain.format(),
            },
            // Every D3D11 device makes f16 chains; the scRGB tag decides.
            supports_f16: true,
            last_hdr_probe: Some(aterm_time::Instant::now()),
            // A flip-model chain in a window is opaque or premultiplied, never
            // post-multiplied: translucency is the visual instance's.
            post_mult: false,
            pre_mult: kind == D3dSwapKind::Composition,
            // The arm reads a buffer back for the taps.
            copyable: true,
            d3d11: Some(D3dWindow {
                chain: Some(chain),
                hwnd,
                kind,
                timed_out_waits: 0,
                _target: Box::new(target),
            }),
        })
    }

    /// A chain of `kind` on `hwnd`: f16 tagged scRGB when the gate asks for
    /// it and the window's output is in HDR mode, else 8-bit.
    ///
    /// # Safety
    ///
    /// `hwnd` is a live window of this thread, which outlives the chain.
    unsafe fn d3d11_make_chain(
        &self,
        hwnd: HWND,
        kind: D3dSwapKind,
        size: (u32, u32),
    ) -> Result<D3dWindowSurface, String> {
        let live = self.d3d11_arm.as_ref().ok_or("the D3D11 arm is not live")?;
        if crate::format_plan::hdr_swapchain_wants_f16(self.hdr_glow, true) {
            // SAFETY: the caller's contract.
            let f16 = unsafe {
                D3dWindowSurface::create(
                    &live.dev,
                    hwnd,
                    kind,
                    size,
                    TexelFormat::Rgba16Float,
                    FRAME_LATENCY,
                )
            };
            match f16.and_then(|c| c.tag_scrgb_on_hdr_output().map(|()| c)) {
                Ok(chain) => return Ok(chain),
                Err(e) => {
                    if crate::verbose() {
                        crate::stderr_line!("aterm-gpu: f16 requested ({e}); 8-bit swap chain");
                    }
                }
            }
        }
        // SAFETY: the caller's contract; the f16 attempt's chain is dropped.
        unsafe {
            D3dWindowSurface::create(
                &live.dev,
                hwnd,
                kind,
                size,
                TexelFormat::Rgba8Unorm,
                FRAME_LATENCY,
            )
        }
    }

    /// Replace `surf`'s chain with a fresh one of the gate's format (the old
    /// one dropped first: a window takes one flip-model chain at a time).
    /// `Err` leaves the window without a chain and latches the loss, so the
    /// frontend's loss poll downgrades it.
    fn d3d11_remake_chain(&self, surf: &mut GpuSurface, why: &str) -> Result<(), ()> {
        let (w, h) = (surf.neutral.width, surf.neutral.height);
        let Some(dw) = surf.d3d11.as_mut() else {
            return Err(());
        };
        dw.chain = None;
        // SAFETY: `hwnd` is the window `_target` keeps alive.
        match unsafe { self.d3d11_make_chain(dw.hwnd, dw.kind, (w, h)) } {
            Ok(chain) => {
                surf.neutral.format = chain.format();
                dw.chain = Some(chain);
                Ok(())
            }
            Err(e) => {
                self.ctx
                    .latch_surface_loss(format!("the swap chain remade after {why} failed: {e}"));
                Err(())
            }
        }
    }

    /// Revalidate or restore the opted-in f16 chain on the probe's cadence,
    /// since Windows does not resize a window when HDR is switched: a live
    /// f16 chain is re-tagged, and falls back to 8-bit when its output left
    /// HDR mode; an opted-in 8-bit chain moves to f16 when its output entered
    /// it. The wgpu arm's plan (`format_plan::hdr_reconfigure_plan`,
    /// `hdr_live_upgrade_wants_f16`) and window metadata.
    fn d3d11_reconcile_hdr_if_due(&mut self, win: &mut WindowGpu, surf: &mut GpuSurface) {
        if !surf.is_hdr() && !(self.hdr_glow && surf.supports_f16) {
            return;
        }
        let now = aterm_time::Instant::now();
        if !hdr_color_space_probe_due(surf.last_hdr_probe, now) {
            return;
        }
        surf.last_hdr_probe = Some(now);
        let Some(chain) = surf.d3d11.as_ref().and_then(|dw| dw.chain.as_ref()) else {
            return;
        };
        if surf.is_hdr() {
            let retagged = chain.tag_scrgb_on_hdr_output().is_ok();
            let plan = crate::format_plan::hdr_reconfigure_plan(true, retagged);
            if plan == crate::format_plan::HdrReconfigurePlan::FallbackToSdr {
                // The gate still asks for f16, so the remake takes 8-bit only
                // because the tag is refused, as it was just now.
                if self
                    .d3d11_remake_chain(surf, "Windows HDR switched off")
                    .is_err()
                {
                    return;
                }
                if crate::verbose() {
                    crate::stderr_line!(
                        "aterm-gpu: scRGB re-tag failed; fell back to {:?}",
                        surf.neutral.format
                    );
                }
            }
            win.apply_hdr_reconfigure_plan(plan);
            return;
        }
        if !crate::format_plan::hdr_live_upgrade_wants_f16(
            self.hdr_glow,
            surf.supports_f16,
            chain.output_hdr(),
        ) {
            return;
        }
        if self
            .d3d11_remake_chain(surf, "Windows HDR switched on")
            .is_ok()
            && surf.is_hdr()
        {
            win.apply_hdr_surface_upgrade();
        }
    }

    /// The window present: wait on the chain's waitable (timed as the
    /// acquire; a timeout draws and presents all the same), encode on D3D11,
    /// compose into buffer 0 (8-bit, or the EDR present on an f16 chain),
    /// present it. A frame that fails after the wait keeps the chain's frame
    /// for the next present.
    #[allow(
        clippy::too_many_arguments,
        reason = "the shared swapchain seam's established tuple"
    )]
    pub(crate) fn d3d11_present_input_with_crop(
        &mut self,
        win: &mut WindowGpu,
        surf: &mut GpuSurface,
        input: &RenderInput,
        invert: bool,
        overlay: Option<DropOverlay>,
        tray: Option<TrayQuad<'_>>,
        source_crop: Option<PresentCrop>,
        effects_transport_shift_y: i32,
    ) -> Result<(), SurfacePresentFailure> {
        // A minimized window cannot be seen, and DXGI presents to it all the
        // same: refuse before any compose, as the wgpu arm did, unless a
        // capture or a recording waits on the present.
        if win.parks_while_minimized() {
            return Err(SurfacePresentFailure::Occluded);
        }
        let source_crop = self
            .resident_present_crop(input, source_crop)
            .map_err(|()| SurfacePresentFailure::Validation)?;
        if surf.d3d11.is_none() {
            d3d11_arm::d3d11_arm_note(
                "armed present refused: the window has no D3D11 swap chain (re-attach it)",
            );
            return Err(SurfacePresentFailure::Validation);
        }
        if !self.d3d11_arm_ready() {
            d3d11_arm::d3d11_arm_note("armed present refused: the D3D11 arm is not live");
            return Err(SurfacePresentFailure::Validation);
        }
        self.d3d11_reconcile_hdr_if_due(win, surf);
        // A remake above that failed latched the loss: the frontend's loss
        // poll downgrades.
        if self.device_lost() {
            return Err(SurfacePresentFailure::Validation);
        }
        let (w, h) = (surf.neutral.width, surf.neutral.height);
        let Some(dw) = surf.d3d11.as_mut() else {
            return Err(SurfacePresentFailure::Validation);
        };
        let Some(chain) = dw.chain.as_mut() else {
            return Err(SurfacePresentFailure::Validation);
        };
        // The retained desire (`resize_surface`) is the chain's size.
        if let Err(e) = chain.resize(w, h) {
            d3d11_arm::d3d11_arm_note(&format!("armed present: the resize failed ({e})"));
            return Err(SurfacePresentFailure::Validation);
        }
        if !win.measure_surface_acquire(|| chain.wait_for_frame(ACQUIRE_TIMEOUT_MS)) {
            // wgpu-dx12's acquire: the frame draws and presents, and that
            // present's retirement gives the waitable a frame back.
            dw.timed_out_waits += 1;
            d3d11_arm::d3d11_arm_note(
                "armed present: the frame-latency wait timed out; presenting anyway",
            );
        }
        let work_started = aterm_time::Instant::now();
        let (fw, fh) = self.encode_present_frame(win, input);
        if !self.last_frame_arm_d3d11 {
            d3d11_arm::d3d11_arm_note("armed present dropped: the frame did not draw on D3D11");
            return Err(SurfacePresentFailure::Validation);
        }
        win.resident_input_effects_transport_shift_y = effects_transport_shift_y;
        if tray.is_none() {
            win.d3d11_tray_card = None;
        }
        // Translucent on the visual instance with backdrop margins live, and
        // then premultiplied; an HWND chain offers no post-multiplied
        // composite (`post_mult` is false at every D3D11 attach), so it is
        // opaque.
        let premult = surf.pre_mult && self.ctx.visual_swapchain && self.backdrop_margins_active();
        let translucent = premult || (self.cpu.background_opacity() < 1.0 && surf.post_mult);
        let chain = surf
            .d3d11
            .as_mut()
            .and_then(|dw| dw.chain.as_mut())
            .expect("checked above");
        let composed = match chain.back_buffer() {
            Ok(back) => self
                .d3d11_compose_present(
                    win,
                    input,
                    invert,
                    overlay,
                    tray,
                    source_crop,
                    (fw, fh),
                    D3dDest::Texture(back),
                    translucent,
                    premult,
                )
                .map(|_| ()),
            Err(e) => Err(e),
        };
        if let Err(e) = composed {
            d3d11_arm::d3d11_arm_note(&format!("armed present dropped: the compose failed ({e})"));
            return Err(SurfacePresentFailure::Validation);
        }
        win.last_present_work_ns =
            u64::try_from(work_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        win.last_present_encode_ns = win.last_present_work_ns;
        match chain.present(0) {
            Ok(D3dPresented::Shown) => {}
            Ok(D3dPresented::Occluded) => return Err(SurfacePresentFailure::Occluded),
            Err(e) => {
                d3d11_arm::d3d11_arm_note(&format!("armed present: the present failed ({e})"));
                return Err(SurfacePresentFailure::Validation);
            }
        }
        self.d3d11_maybe_inject_loss();
        Ok(())
    }
}
