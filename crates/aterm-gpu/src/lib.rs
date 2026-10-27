// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

// One first-party arm per target: Metal (macOS), GL (Linux, and WebGL2 on
// wasm32) and Direct3D 11 (Windows). Any other target has none, since GW.6
// removed wgpu (docs/AUDIT-third-party-2026-10-15.md).
#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    windows,
    target_arch = "wasm32"
)))]
compile_error!("aterm-gpu draws on Metal, GL or Direct3D 11: this target has none of them");

/// `eprintln!` that cannot panic, for every stderr line this crate writes at runtime.
macro_rules! stderr_line {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr().lock(), $($arg)*);
    }};
}
pub(crate) use stderr_line;

/// The window's `--verbose` flag, handed down by the host at launch.
static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Records the host's `--verbose` launch flag.
#[inline]
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the host passed `--verbose`.
#[inline]
#[cfg(any(d3d11_arm, all(gl_arm, not(target_arch = "wasm32"))))]
pub(crate) fn verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
}

use aterm_render::Frame;

mod device_layer;
mod format_plan;
mod pipeline_table;

pub use format_plan::{
    HdrPlan, HdrReconfigurePlan, hdr_live_upgrade_wants_f16, hdr_present_plan,
    hdr_reconfigure_plan, hdr_screen_upgrade_decision, hdr_screen_upgrade_wants_f16,
    hdr_swapchain_wants_f16, hdr_swapchain_wants_f16_on_screen,
};
/// Finite integer compute jobs on Metal shared buffers (`compute`).
#[cfg(all(target_os = "macos", any(test, feature = "compute")))]
pub mod compute;
#[cfg(d3d11_arm)]
mod d3d11;
#[cfg(gl_arm)]
mod gl;
#[cfg(all(test, target_os = "macos", not(gl_arm)))]
#[path = "gl/shaders.rs"]
mod gl_shaders;
#[cfg(target_os = "macos")]
mod metal;
/// Launch Metal warm types (`metal/warm.rs`).
#[cfg(target_os = "macos")]
pub use metal::warm::{FirstFrameKnobs, FirstFrameWarm};
/// First-frame compile benchmark entry points (`benches/first_frame_compile.rs`).
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use metal::warm::{first_frame_bench_main, first_frame_bench_warm};
mod renderer;
/// A test's route for a frame's glow ramps.
#[doc(hidden)]
#[cfg(any(test, debug_assertions, feature = "dev-seams"))]
pub use renderer::RampsFx;
/// Native window target for swapchain attachment.
#[cfg(not(target_arch = "wasm32"))]
pub use renderer::WindowTarget;
pub use renderer::{
    DropOverlay, FetchProbe, FetchStage, FrameShape, GpuRenderer, GpuSurface, PresentCrop,
    SurfacePresentFailure, TrayQuad, TrayScrim, UnormAblation, UnormRoute, WindowGpu,
};
/// Demand-built effect pipeline identifiers and names (`renderer::EffectPipelines`).
pub use renderer::{
    EFFECT_PIPELINE_COUNT, EFFECT_PIPELINE_NAMES, EFFECT_PIPELINES, EffectPipeline,
};
/// A window's own presenter.
#[cfg(target_os = "linux")]
pub use renderer::{FrameDraw, FrameSink};
/// Per-frame GPU execution timing (`gpu_time`).
pub mod gpu_time;
/// Present-to-glass latency measurement (`present_glass`).
pub mod present_glass;
/// Cold-build sub-phase timing for the GPU backend.
pub mod startup_probe;
/// Submitted destination-frame video capture (`video_tap`).
pub mod video_tap;
/// Browser WebGL2 canvas entry point.
#[cfg(target_arch = "wasm32")]
pub mod web;
pub use renderer::should_slice;
/// Armed tray card ring slot bookkeeping (`tray_card_ring`).
pub mod tray_card_ring;

#[cfg(windows)]
static VISUAL_SWAPCHAIN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether a request was withdrawn this run due to GPU init, configure, or device loss.
#[cfg(windows)]
static VISUAL_SWAPCHAIN_WITHDRAWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether [`GpuContext::new`] actually built the visual path.
#[cfg(windows)]
static VISUAL_SWAPCHAIN_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Requests that the next [`GpuContext::new`] use the Windows DirectComposition visual swapchain.
#[inline]
#[cfg(windows)]
pub fn request_visual_swapchain() {
    VISUAL_SWAPCHAIN_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Withdraws the Windows visual-swapchain request.
#[inline]
#[cfg(windows)]
pub fn withdraw_visual_swapchain() {
    if VISUAL_SWAPCHAIN_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed) {
        VISUAL_SWAPCHAIN_WITHDRAWN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether a visual-swapchain request was made and then withdrawn this run.
#[inline]
#[cfg(windows)]
pub fn visual_swapchain_withdrawn() -> bool {
    VISUAL_SWAPCHAIN_WITHDRAWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Frontend recovery action when `GpuRenderer::create_window_surface` fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceAttachFallback {
    /// Another window already presents on this instance; decline this window only.
    DeclineWindow,
    /// Withdraw the visual latch, rebuild on the opaque HWND swapchain, and retry.
    RebuildOpaque,
    /// Downgrade the application to the CPU renderer.
    CpuRenderer,
}

/// Selects the [`SurfaceAttachFallback`] for a failed window surface attach.
#[must_use]
#[inline]
pub const fn surface_attach_fallback(
    visual_swapchain_active: bool,
    other_gpu_windows: bool,
) -> SurfaceAttachFallback {
    if other_gpu_windows {
        SurfaceAttachFallback::DeclineWindow
    } else if visual_swapchain_active {
        SurfaceAttachFallback::RebuildOpaque
    } else {
        SurfaceAttachFallback::CpuRenderer
    }
}

/// Whether the DirectComposition visual swapchain has been requested.
#[inline]
#[cfg(windows)]
pub fn visual_swapchain_requested() -> bool {
    VISUAL_SWAPCHAIN_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the live [`GpuContext`] was built on the DirectComposition visual path.
#[inline]
#[cfg(windows)]
pub fn visual_swapchain_active() -> bool {
    VISUAL_SWAPCHAIN_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// GPU device, queue, and adapter capabilities.
pub struct GpuContext {
    pub adapter_name: String,
    pub backend: String,
    /// Whether the device supports sRGB views of `Rgba8Unorm` offscreen textures.
    pub(crate) srgb_offscreen: bool,
    /// Latched by a D3D11 window surface's failure (`latch_surface_loss`).
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The latched loss's cause, read by [`Self::device_loss_reason`].
    device_loss_msg: std::sync::Arc<std::sync::OnceLock<String>>,
    /// Metal loss latch wired at construction and read by [`Self::device_lost`].
    #[cfg(target_os = "macos")]
    metal_loss: std::sync::OnceLock<std::sync::Arc<metal::loss::LossLatch>>,
    /// Whether this instance uses the Windows DirectComposition visual swapchain.
    pub(crate) visual_swapchain: bool,
    /// Maximum 2D texture dimension reported by the GL driver.
    #[cfg(gl_arm)]
    pub(crate) gl_max_texture_dim: u32,
    /// WebGL2 device bound to the browser canvas.
    #[cfg(target_arch = "wasm32")]
    pub(crate) gl_canvas: gl::device::GlDevice,
}

/// Row byte alignment required by `copy_texture_to_buffer` and Metal readback.
const ALIGN: usize = 256;

/// Row byte stride padded to the 256-byte `copy_texture_to_buffer` alignment.
#[inline]
pub(crate) const fn padded_bytes_per_row(width: usize) -> usize {
    let unpadded = width * 4;
    unpadded.div_ceil(ALIGN) * ALIGN
}

/// Unpacks a padded RGBA8 readback buffer into an `aterm_render::Frame`.
pub(crate) fn frame_from_padded_rgba(bytes: &[u8], w: usize, h: usize, padded: usize) -> Frame {
    let mut pixels = Vec::with_capacity(w * h);
    for row in 0..h {
        let base = row * padded;
        let src = &bytes[base..base + w * 4];
        pixels.extend(src.as_chunks::<4>().0.iter().map(|c| {
            ((255 - u32::from(c[3])) << 24)
                | (u32::from(c[0]) << 16)
                | (u32::from(c[1]) << 8)
                | u32::from(c[2])
        }));
    }
    Frame {
        width: w,
        height: h,
        pixels,
    }
}

/// Whether the first-party Metal backend is active (always `true` on macOS).
#[cfg(target_os = "macos")]
#[must_use]
#[inline]
pub const fn metal_backend_selected() -> bool {
    true
}

/// Claims the process-once first-frame Metal warm for background execution.
#[cfg(target_os = "macos")]
#[must_use]
#[inline]
pub fn begin_first_frame_warm() -> Option<FirstFrameWarm> {
    metal::warm::FIRST_FRAME.begin()
}

/// Whether [`begin_first_frame_warm`] has been claimed in this process.
#[cfg(target_os = "macos")]
#[must_use]
#[inline]
pub fn first_frame_warm_began() -> bool {
    metal::warm::FIRST_FRAME.began()
}

impl GpuContext {
    /// Creates the production macOS [`GpuContext`] (Metal backend).
    #[cfg(target_os = "macos")]
    pub fn new() -> Result<Self, String> {
        let adapter_name = metal::ffi::Device::preferred()
            .map(|d| d.name())
            .ok_or("no Metal device on this machine")?;
        let ctx = Self {
            adapter_name,
            backend: "Metal".to_owned(),
            srgb_offscreen: true,
            device_lost: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            device_loss_msg: std::sync::Arc::default(),
            metal_loss: std::sync::OnceLock::new(),
            visual_swapchain: false,
        };
        let wired = ctx.wire_metal_loss_latch(std::sync::Arc::new(metal::loss::LossLatch::new()));
        debug_assert!(wired, "a fresh context's loss-latch cell is empty");
        Ok(ctx)
    }

    /// Creates the production Linux [`GpuContext`] after probing EGL and GLESv2.
    #[cfg(all(gl_arm, not(target_arch = "wasm32")))]
    pub fn new() -> Result<Self, String> {
        let probe = gl::device::GlDevice::headless()?;
        let caps = probe.caps().clone();
        drop(probe);
        Ok(Self {
            adapter_name: caps.renderer,
            backend: "Gl".to_owned(),
            srgb_offscreen: caps.srgb_views,
            device_lost: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            device_loss_msg: std::sync::Arc::default(),
            visual_swapchain: false,
            gl_max_texture_dim: caps.max_texture_dim,
        })
    }

    /// Creates the production Windows [`GpuContext`] after probing a headless Direct3D 11
    /// device; `Err` (no Direct3D 11 adapter) sends the frontend to the CPU renderer.
    #[cfg(d3d11_arm)]
    pub fn new() -> Result<Self, String> {
        let probe = d3d11::device::D3dDevice::headless()?;
        let adapter_name = probe.caps().adapter.clone();
        drop(probe);
        let visual_swapchain = visual_swapchain_requested();
        VISUAL_SWAPCHAIN_ACTIVE.store(visual_swapchain, std::sync::atomic::Ordering::Relaxed);
        Ok(Self {
            adapter_name,
            backend: "D3d11".to_owned(),
            // The arm runs the native plan: sRGB views of `R8G8B8A8_TYPELESS` storage.
            srgb_offscreen: true,
            device_lost: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            device_loss_msg: std::sync::Arc::default(),
            visual_swapchain,
        })
    }

    /// Whether the GPU device has been reported lost.
    #[inline]
    pub fn device_lost(&self) -> bool {
        #[cfg(target_os = "macos")]
        if self.metal_loss.get().is_some_and(|l| l.is_lost()) {
            return true;
        }
        self.device_lost.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Latches a window surface configuration failure as a device loss with reason `why`.
    #[inline]
    #[cfg(d3d11_arm)]
    pub(crate) fn latch_surface_loss(&self, why: String) {
        let _ = self.device_loss_msg.set(why);
        self.device_lost
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Returns the recorded cause of the first latched device loss, if any.
    #[must_use]
    #[inline]
    pub fn device_loss_reason(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        if let Some(r) = self.metal_loss.get().and_then(|l| l.reason()) {
            return Some(r.to_owned());
        }
        self.device_loss_msg.get().cloned()
    }

    /// Wires a Metal loss latch into [`Self::device_lost`].
    #[inline]
    #[cfg(target_os = "macos")]
    pub(crate) fn wire_metal_loss_latch(
        &self,
        latch: std::sync::Arc<metal::loss::LossLatch>,
    ) -> bool {
        self.metal_loss.set(latch).is_ok()
    }

    /// Returns the wired Metal loss latch, wiring a fresh one if needed.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_loss_latch_or_wire(&self) -> std::sync::Arc<metal::loss::LossLatch> {
        if let Some(l) = self.metal_loss.get() {
            return std::sync::Arc::clone(l);
        }
        let _ = self.wire_metal_loss_latch(std::sync::Arc::new(metal::loss::LossLatch::new()));
        std::sync::Arc::clone(self.metal_loss.get().expect("wired just above"))
    }
}

/// Fail-soft: the latch-withdrawal decision a failed window-surface attach
/// drives (`surface_attach_fallback`).
#[cfg(test)]
mod surface_attach_fallback_tests {
    use super::{SurfaceAttachFallback as F, surface_attach_fallback};

    #[test]
    fn rebuild_iff_visual_and_alone() {
        let rows = [
            (true, false, F::RebuildOpaque, "visual, first window"),
            (true, true, F::DeclineWindow, "visual, live GPU windows"),
            (false, true, F::DeclineWindow, "plain, live GPU windows"),
            (false, false, F::CpuRenderer, "plain, first window"),
        ];
        for (visual, others, want, why) in rows {
            let plan = surface_attach_fallback(visual, others);
            assert_eq!(plan, want, "{why}: visual={visual} others={others}");
            assert_eq!(
                plan == F::RebuildOpaque,
                visual && !others,
                "visual={visual} others={others} -> {plan:?}"
            );
            assert_eq!(plan == F::DeclineWindow, others, "others must dominate");
        }
    }
}
