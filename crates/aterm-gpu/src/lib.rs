// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

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
#[cfg(any(wgpu_arm, all(gl_arm, not(target_arch = "wasm32"))))]
pub(crate) fn verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
}

use aterm_render::Frame;

#[cfg(wgpu_arm)]
mod block_on;
mod device_layer;
mod format_plan;
mod pipeline_table;

pub use format_plan::{
    HdrPlan, HdrReconfigurePlan, hdr_live_upgrade_wants_f16, hdr_present_plan,
    hdr_reconfigure_plan, hdr_screen_upgrade_decision, hdr_screen_upgrade_wants_f16,
    hdr_swapchain_wants_f16, hdr_swapchain_wants_f16_on_screen,
};
#[cfg(gl_arm)]
mod gl;
#[cfg(all(test, target_os = "macos", not(gl_arm)))]
#[path = "gl/shaders.rs"]
mod gl_shaders;
#[cfg(target_os = "macos")]
mod metal;
/// Launch Metal warm types (`metal/warm.rs`).
#[cfg(target_os = "macos")]
pub use metal::warm::{FirstFrameKnobs, FirstFrameWarm, PresentKnobs, ShaderCacheWarm};
/// First-frame compile benchmark entry points (`benches/first_frame_compile.rs`).
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use metal::warm::{first_frame_bench_main, first_frame_bench_warm};
mod renderer;
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
/// The wgpu arm's clock for `gpu_time`: per-pass timestamp queries.
#[cfg(wgpu_arm)]
mod wgpu_timer;
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
    /// Wgpu logical device.
    #[cfg(wgpu_arm)]
    pub(crate) device: wgpu::Device,
    #[cfg(wgpu_arm)]
    pub(crate) queue: wgpu::Queue,
    pub adapter_name: String,
    pub backend: String,
    /// Whether the device supports sRGB views of `Rgba8Unorm` offscreen textures.
    pub(crate) srgb_offscreen: bool,
    /// Wgpu instance used to create window surfaces.
    #[cfg(wgpu_arm)]
    pub(crate) instance: wgpu::Instance,
    #[cfg(wgpu_arm)]
    pub(crate) adapter: wgpu::Adapter,
    /// Latched by wgpu's device-lost callback.
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Driver message from the device-lost callback, read by [`Self::device_loss_reason`].
    device_loss_msg: std::sync::Arc<std::sync::OnceLock<String>>,
    /// Metal loss latch wired at construction and read by [`Self::device_lost`].
    #[cfg(target_os = "macos")]
    metal_loss: std::sync::OnceLock<std::sync::Arc<metal::loss::LossLatch>>,
    /// Whether this instance uses the Windows DirectComposition visual swapchain.
    pub(crate) visual_swapchain: bool,
    /// Maximum 2D texture dimension reported by the GL driver.
    #[cfg(all(gl_arm, not(wgpu_arm)))]
    pub(crate) gl_max_texture_dim: u32,
    /// WebGL2 device bound to the browser canvas.
    #[cfg(target_arch = "wasm32")]
    pub(crate) gl_canvas: gl::device::GlDevice,
}

/// Row byte alignment required by `copy_texture_to_buffer` and Metal readback.
const ALIGN: usize = 256;
#[cfg(wgpu_arm)]
const _: () = assert!(ALIGN == wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);

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

/// Map an `ATERM_GPU_POWER` value to a power preference.
#[cfg(wgpu_arm)]
fn parse_gpu_power(v: &str) -> Option<wgpu::PowerPreference> {
    match v.to_ascii_lowercase().as_str() {
        "low" => Some(wgpu::PowerPreference::LowPower),
        "high" => Some(wgpu::PowerPreference::HighPerformance),
        _ => None,
    }
}

/// One context made at a time in a process ([`GpuContext::one_at_a_time`]).
#[cfg(wgpu_arm)]
static GPU_INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The GPU turn of a test or debug build on Windows: the thread that makes a device keeps it until
/// the thread exits ([`take_gpu_turn`]), so one thread at a time draws.
///
/// D3D12 devices are singletons per adapter, so the devices parallel tests make are one
/// `ID3D12Device` driven from a command queue each. WARP (`d3d10warp.dll` 10.0.26100.9278 in the
/// lane) crashes when two queues JIT shaders at once: `STATUS_ACCESS_VIOLATION` in its
/// `Task_CompileShader` (`FeCleanup::Pass2`, `PixelJitProgram::ClassifyVars`) under concurrent
/// `ExecuteCommandLists` (docs/measured/windows-warp-parallel-2026-10-23.md). Queues are
/// free-threaded, so the fault is WARP's, and a person's aterm never reaches it: it makes one device
/// at a time, and wgpu submits to one device one submission at a time. Creation alone is serialized
/// already (`GPU_INIT`); the crash is in drawing, so the turn lasts as long as the thread. Every
/// device takes it ([`GpuContext::one_at_a_time`]), so no test has to remember to. A debug aterm
/// makes its device on a startup worker that exits, or on its main thread, so its turn is never
/// contended; a thread that makes a device while another thread holds the turn waits for that thread
/// to exit.
#[cfg(all(windows, any(test, debug_assertions)))]
static GPU_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(windows, any(test, debug_assertions)))]
std::thread_local! {
    /// This thread's hold on [`GPU_TURN`], released when the thread (a test) exits.
    static HELD_GPU_TURN: std::cell::OnceCell<std::sync::MutexGuard<'static, ()>> =
        const { std::cell::OnceCell::new() };
}

/// Gives the calling thread [`GPU_TURN`] until it exits, at once if it holds it.
#[cfg(all(windows, any(test, debug_assertions)))]
fn take_gpu_turn() {
    HELD_GPU_TURN.with(|held| {
        held.get_or_init(|| {
            GPU_TURN
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
    });
}

/// `wgpu::Instance::new`, under a name of its own.
#[cfg(wgpu_arm)]
fn fresh_instance(desc: wgpu::InstanceDescriptor) -> wgpu::Instance {
    wgpu::Instance::new(desc)
}

/// Builds the `wgpu::InstanceDescriptor` for `backends`.
#[cfg(wgpu_arm)]
pub(crate) fn instance_descriptor(backends: wgpu::Backends) -> wgpu::InstanceDescriptor {
    let desc = wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    };
    let desc = wgpu::InstanceDescriptor {
        flags: terminal_instance_flags(desc.flags, desc.backends),
        ..desc
    };
    #[cfg(all(unix, not(target_vendor = "apple")))]
    let desc = wgpu::InstanceDescriptor {
        flags: vulkan_instance_flags(desc.flags),
        ..desc
    };
    desc
}

/// The Vulkan cell's instance flags: the build's, less `DEBUG`.
#[inline]
#[cfg(all(wgpu_arm, unix, not(target_vendor = "apple")))]
fn vulkan_instance_flags(flags: wgpu::InstanceFlags) -> wgpu::InstanceFlags {
    flags - wgpu::InstanceFlags::DEBUG
}

#[inline]
#[cfg(wgpu_arm)]
const fn default_backends() -> wgpu::Backends {
    #[cfg(windows)]
    let backends = wgpu::Backends::DX12;
    #[cfg(not(windows))]
    let backends = wgpu::Backends::PRIMARY;
    backends
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

/// Claims the process-once headless launch warm for background execution: the first frames' compiles, and under [`PresentKnobs`] the rest a frame can bind, made before a deferred renderer's first pixel demand and awaited by nothing ([`ShaderCacheWarm::run`]).
#[cfg(target_os = "macos")]
#[must_use]
#[inline]
pub fn begin_shader_cache_warm() -> Option<ShaderCacheWarm> {
    ShaderCacheWarm::begin()
}

/// Whether [`begin_first_frame_warm`] has been claimed in this process.
#[cfg(target_os = "macos")]
#[must_use]
#[inline]
pub fn first_frame_warm_began() -> bool {
    metal::warm::FIRST_FRAME.began()
}

/// Whether [`begin_shader_cache_warm`] has been claimed in this process.
#[cfg(target_os = "macos")]
#[must_use]
#[inline]
pub fn shader_cache_warm_began() -> bool {
    ShaderCacheWarm::began()
}

/// The `ATERM_GPU_POWER` seam, parsed.
#[cfg(wgpu_arm)]
fn power_preference_from_env() -> wgpu::PowerPreference {
    match aterm_types::dev_seam!("ATERM_GPU_POWER").map(|v| v.to_string_lossy().into_owned()) {
        Some(v) if !v.is_empty() => parse_gpu_power(&v).unwrap_or_else(|| {
            crate::stderr_line!(
                "aterm-gpu: unknown ATERM_GPU_POWER {v:?} (want low|high); using low"
            );
            wgpu::PowerPreference::LowPower
        }),
        _ => wgpu::PowerPreference::LowPower,
    }
}

/// The suballocator block floor in MiB.
#[cfg(wgpu_arm)]
const MEMBLOCK_FLOOR_MIB: u64 = 16;

/// The block-size ceiling in MiB.
#[cfg(wgpu_arm)]
const MEMBLOCK_CEILING_MIB: u64 = 128;

/// Build a `MemoryHints::Manual` from a `(min, max)` device block size in MiB.
#[inline]
#[cfg(wgpu_arm)]
const fn memblock_hints(min_mib: u64, max_mib: u64) -> wgpu::MemoryHints {
    const MIB: u64 = 1024 * 1024;
    wgpu::MemoryHints::Manual {
        suballocated_device_memory_block_size: (min_mib * MIB)..(max_mib * MIB),
    }
}

/// Memory allocator hints passed to wgpu device creation.
#[must_use]
#[inline]
#[cfg(wgpu_arm)]
pub(crate) const fn terminal_memory_hints() -> wgpu::MemoryHints {
    memblock_hints(MEMBLOCK_FLOOR_MIB, MEMBLOCK_CEILING_MIB)
}

/// The view-descriptor heap aterm asks a DX12 device for, in descriptors.
#[cfg(wgpu_arm)]
const TERMINAL_VIEW_DESCRIPTORS: u32 = 16_384;

/// The optional features aterm asks of an adapter offering `available`.
#[inline]
#[cfg(wgpu_arm)]
fn terminal_device_features(available: wgpu::Features) -> wgpu::Features {
    available & wgpu::Features::TIMESTAMP_QUERY
}

/// The device limits aterm asks of an adapter on `backend`.
#[inline]
#[cfg(wgpu_arm)]
fn terminal_device_limits(backend: wgpu::Backend) -> wgpu::Limits {
    match backend {
        wgpu::Backend::Dx12 => wgpu::Limits {
            max_non_sampler_bindings: TERMINAL_VIEW_DESCRIPTORS,
            ..wgpu::Limits::default()
        },
        _ => wgpu::Limits::default(),
    }
}

/// The instance flags aterm asks for over `backends`.
#[inline]
#[cfg(wgpu_arm)]
fn terminal_instance_flags(
    flags: wgpu::InstanceFlags,
    backends: wgpu::Backends,
) -> wgpu::InstanceFlags {
    if backends == wgpu::Backends::DX12 || backends == wgpu::Backends::GL {
        flags - wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    } else {
        flags
    }
}

/// The `ATERM_GPU_ADAPTER` seam.
#[cfg(wgpu_arm)]
async fn adapter_from_env(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    let want = aterm_types::dev_seam!("ATERM_GPU_ADAPTER")
        .map(|v| v.to_string_lossy().into_owned())
        .filter(|v| !v.is_empty())?;
    let want_lc = want.to_ascii_lowercase();
    let found = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .find(|a| a.get_info().name.to_ascii_lowercase().contains(&want_lc));
    if found.is_none() {
        crate::stderr_line!(
            "aterm-gpu: ATERM_GPU_ADAPTER={want:?} matched no adapter; using default selection"
        );
    }
    found
}

impl GpuContext {
    /// Creates the production macOS [`GpuContext`] (Metal backend).
    #[cfg(all(not(wgpu_arm), target_os = "macos"))]
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
    #[cfg(all(not(wgpu_arm), gl_arm, not(target_arch = "wasm32")))]
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

    /// Acquires a headless wgpu [`GpuContext`].
    #[cfg(wgpu_arm)]
    pub fn new() -> Result<Self, String> {
        let desc = instance_descriptor(default_backends());
        #[cfg(windows)]
        let desc = {
            let mut desc = desc;
            if visual_swapchain_requested() {
                desc.backend_options.dx12.presentation_system =
                    wgpu::Dx12SwapchainKind::DxgiFromVisual;
            }
            desc
        };
        let ctx = Self::one_at_a_time(desc)?;
        #[cfg(windows)]
        let ctx = {
            let mut ctx = ctx;
            ctx.visual_swapchain = visual_swapchain_requested() && ctx.backend == "Dx12";
            VISUAL_SWAPCHAIN_ACTIVE
                .store(ctx.visual_swapchain, std::sync::atomic::Ordering::Relaxed);
            ctx
        };
        #[cfg(target_os = "macos")]
        if metal_backend_selected() {
            let wired =
                ctx.wire_metal_loss_latch(std::sync::Arc::new(metal::loss::LossLatch::new()));
            debug_assert!(wired, "a fresh context's loss-latch cell is empty");
        }
        Ok(ctx)
    }

    /// Acquires a wgpu adapter and device from `desc`, serialized across threads.
    #[cfg(wgpu_arm)]
    pub(crate) fn one_at_a_time(desc: wgpu::InstanceDescriptor) -> Result<Self, String> {
        // The turn before `GPU_INIT`: a thread that holds the turn never waits for `GPU_INIT` on
        // one that holds it and waits for the turn.
        #[cfg(all(windows, any(test, debug_assertions)))]
        take_gpu_turn();
        let _one_at_a_time = GPU_INIT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let instance =
            startup_probe::timed(startup_probe::Leg::GpuInstance, || fresh_instance(desc));
        crate::block_on::block_on(Self::from_instance(instance))
    }

    /// Asynchronously acquires an adapter and device from `instance`.
    #[cfg(wgpu_arm)]
    pub(crate) async fn from_instance(instance: wgpu::Instance) -> Result<Self, String> {
        let adapter_started = aterm_time::Instant::now();
        let adapter = match adapter_from_env(&instance).await {
            Some(adapter) => adapter,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: power_preference_from_env(),
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
                .map_err(|e| format!("no GPU adapter available: {e}"))?,
        };
        let info = adapter.get_info();
        startup_probe::record(startup_probe::Leg::GpuAdapter, adapter_started.elapsed());
        let device_started = aterm_time::Instant::now();
        let required_limits = terminal_device_limits(info.backend);
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("aterm-gpu device"),
                required_features: terminal_device_features(adapter.features()),
                required_limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: terminal_memory_hints(),
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| e.to_string())?;
        startup_probe::record(startup_probe::Leg::GpuDevice, device_started.elapsed());
        let context_tail_started = aterm_time::Instant::now();
        let srgb_offscreen = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS);
        let device_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let device_loss_msg = std::sync::Arc::new(std::sync::OnceLock::new());
        {
            let flag = std::sync::Arc::clone(&device_lost);
            let msg_cell = std::sync::Arc::clone(&device_loss_msg);
            device.set_device_lost_callback(move |reason, msg| {
                let _ = msg_cell.set(if msg.is_empty() {
                    format!("{reason:?}")
                } else {
                    msg
                });
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            });
        }
        let ctx = Self {
            device,
            queue,
            adapter_name: info.name,
            backend: format!("{:?}", info.backend),
            srgb_offscreen,
            instance,
            adapter,
            device_lost,
            device_loss_msg,
            #[cfg(target_os = "macos")]
            metal_loss: std::sync::OnceLock::new(),
            visual_swapchain: false,
        };
        startup_probe::record(
            startup_probe::Leg::GpuContextTail,
            context_tail_started.elapsed(),
        );
        Ok(ctx)
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
    #[cfg(wgpu_arm)]
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

    /// Clamps a texture dimension to `1..=max_texture_dimension_2d`.
    #[inline]
    #[cfg(wgpu_arm)]
    fn clamp_tex_dim(&self, d: u32) -> u32 {
        d.max(1).min(self.device.limits().max_texture_dimension_2d)
    }

    /// Borrows this context as a wgpu [`crate::device_layer::DeviceHandle`].
    #[inline]
    #[cfg(wgpu_arm)]
    pub(crate) const fn device_layer(&self) -> crate::device_layer::DeviceHandle<'_> {
        crate::device_layer::DeviceHandle::Wgpu {
            device: &self.device,
            queue: &self.queue,
        }
    }

    /// Creates an `Rgba8Unorm` offscreen colour target texture.
    #[cfg(wgpu_arm)]
    pub(crate) fn offscreen_texture(&self, width: u32, height: u32) -> wgpu::Texture {
        let width = self.clamp_tex_dim(width);
        let height = self.clamp_tex_dim(height);
        let format = crate::device_layer::TexelFormat::from_wgpu(self.offscreen_format())
            .expect("the offscreen format is inside the map's closed set of six");
        let alias = crate::format_plan::offscreen_view_formats(self.srgb_offscreen)
            .first()
            .map(|f| {
                crate::device_layer::TexelFormat::from_wgpu(*f)
                    .expect("the offscreen alias view format is inside the closed set")
            });
        self.device_layer()
            .create_texture_2d(
                "aterm-gpu offscreen",
                format,
                width,
                height,
                crate::device_layer::TexUsage::OFFSCREEN,
                alias,
            )
            .into_wgpu()
    }

    /// Offscreen colour-target texture format ([`crate::format_plan`]).
    #[inline]
    #[cfg(wgpu_arm)]
    pub(crate) const fn offscreen_format(&self) -> wgpu::TextureFormat {
        crate::format_plan::offscreen_format(self.srgb_offscreen)
    }

    /// SRGB view format for the offscreen colour target ([`crate::format_plan`]).
    #[inline]
    #[cfg(wgpu_arm)]
    pub(crate) fn offscreen_srgb_view_format(&self) -> wgpu::TextureFormat {
        crate::format_plan::offscreen_srgb_view_format(self.srgb_offscreen)
    }

    /// Offscreen target format pair for [`crate::pipeline_table::TargetFormats::resolve`].
    #[inline]
    #[cfg(wgpu_arm)]
    pub(crate) fn pipeline_targets(&self) -> crate::pipeline_table::TargetFormats {
        crate::pipeline_table::TargetFormats {
            offscreen_srgb: self.offscreen_srgb_view_format(),
            offscreen_unorm: self.offscreen_format(),
            present: None,
        }
    }

    /// Reads `texture` back into a [`Frame`], returning a black frame on device loss.
    #[cfg(wgpu_arm)]
    pub(crate) fn read_back(&self, texture: &wgpu::Texture, width: u32, height: u32) -> Frame {
        self.try_read_back(texture, width, height)
            .unwrap_or_else(|e| {
                crate::stderr_line!("aterm-gpu: readback failed ({e}); returning blank frame");
                let (w, h) = (width as usize, height as usize);
                Frame {
                    width: w,
                    height: h,
                    pixels: vec![0; w * h],
                }
            })
    }

    /// Fallible core of [`Self::read_back`].
    #[cfg(wgpu_arm)]
    pub(crate) fn try_read_back(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        let (w, h) = (width as usize, height as usize);
        let padded = padded_bytes_per_row(w);
        let buffer = self
            .device_layer()
            .create_readback_buffer("aterm-gpu readback", (padded * h) as u64)
            .into_wgpu();
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded as u32),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        crate::device_layer::submit_host_readable(
            &self.device,
            &self.queue,
            enc.finish(),
            std::slice::from_ref(&buffer),
        );

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        #[expect(
            clippy::disallowed_methods,
            reason = "the copy was submitted by device_layer::submit_host_readable"
        )]
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("GPU poll failed: {e}"))?;
        match rx.try_recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("readback buffer map failed: {e}")),
            Err(_) => return Err("readback buffer map never completed".to_string()),
        }
        let data = slice.get_mapped_range();
        let bytes: &[u8] = &data;
        let frame = frame_from_padded_rgba(bytes, w, h, padded);
        drop(data);
        buffer.unmap();
        Ok(frame)
    }
}

#[cfg(all(test, wgpu_arm))]
mod device_request_tests {
    use super::*;

    #[test]
    fn only_timestamp_queries_are_requested_and_only_when_offered() {
        assert_eq!(
            terminal_device_features(wgpu::Features::all()),
            wgpu::Features::TIMESTAMP_QUERY
        );
        assert_eq!(
            terminal_device_features(wgpu::Features::empty()),
            wgpu::Features::empty()
        );
        let without = wgpu::Features::all() - wgpu::Features::TIMESTAMP_QUERY;
        assert_eq!(terminal_device_features(without), wgpu::Features::empty());
    }

    #[test]
    fn only_a_dx12_device_moves_off_the_default_limits() {
        let defaults = wgpu::Limits::default();
        let dx12 = terminal_device_limits(wgpu::Backend::Dx12);
        assert_eq!(dx12.max_non_sampler_bindings, TERMINAL_VIEW_DESCRIPTORS);
        assert!(
            TERMINAL_VIEW_DESCRIPTORS <= defaults.max_non_sampler_bindings / 32,
            "the heap must stay a small fraction of wgpu's bindless default"
        );
        assert_eq!(
            wgpu::Limits {
                max_non_sampler_bindings: defaults.max_non_sampler_bindings,
                ..dx12
            },
            defaults,
            "every other DX12 limit is wgpu's default"
        );
        for backend in wgpu::Backend::ALL {
            if backend != wgpu::Backend::Dx12 {
                assert_eq!(
                    terminal_device_limits(backend),
                    defaults,
                    "{backend:?} must keep wgpu's default limits"
                );
            }
        }
    }

    #[test]
    fn indirect_call_validation_is_dropped_on_dx12_and_gl_instances_only() {
        let indirect = wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL;
        let defaults = wgpu::InstanceFlags::default();
        assert!(
            defaults.contains(indirect),
            "wgpu's defaults carry it, or there is nothing to drop"
        );
        for backends in [wgpu::Backends::DX12, wgpu::Backends::GL] {
            let dropped = terminal_instance_flags(defaults, backends);
            assert!(
                !dropped.contains(indirect),
                "a {backends:?} instance drops it"
            );
            assert_eq!(dropped | indirect, defaults, "and nothing else");
        }
        for backends in [
            wgpu::Backends::METAL,
            wgpu::Backends::VULKAN,
            wgpu::Backends::PRIMARY,
            wgpu::Backends::all(),
        ] {
            assert_eq!(
                terminal_instance_flags(defaults, backends),
                defaults,
                "{backends:?} keeps wgpu's flags"
            );
        }
        assert_eq!(
            default_backends() == wgpu::Backends::DX12,
            cfg!(windows),
            "the DX12-only instance is Windows's alone"
        );
    }
}

/// `GpuContext::offscreen_texture` bounds its own extent: `clamp_tex_dim`
/// floors to 1 and caps at `max_texture_dimension_2d`.
#[cfg(all(test, wgpu_arm))]
mod offscreen_extent_tests {
    #[test]
    fn offscreen_texture_clamps_zero_and_oversized_dims() {
        let ctx = super::GpuContext::new()
            .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU available: {e}")));
        let dims = |w, h| {
            let t = ctx.offscreen_texture(w, h);
            (t.width(), t.height())
        };
        assert_eq!(dims(0, 4), (1, 4), "width 0 clamps up to 1");
        assert_eq!(dims(6, 0), (6, 1), "height 0 clamps up to 1");
        assert_eq!(dims(0, 0), (1, 1), "0x0 clamps to 1x1");
        let (a, h) = dims(40_000, 4);
        let (b, _) = dims(80_000, 4);
        assert_eq!(h, 4, "an in-range height passes through");
        assert!(
            a < 40_000,
            "width {a} must be clamped below the 40000px request"
        );
        assert_eq!(a, b, "oversized widths must clamp to the SAME device limit");
    }
}

/// The device `GpuContext::new` builds carries the limits its backend is asked
/// for (`terminal_device_limits`), and on DX12 its view-descriptor heap
/// recycles.
#[cfg(all(test, wgpu_arm))]
mod device_limits_tests {
    use super::{GpuContext, TERMINAL_VIEW_DESCRIPTORS, terminal_device_limits};

    fn context() -> GpuContext {
        GpuContext::new()
            .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU available: {e}")))
    }

    #[test]
    fn the_device_carries_the_limits_its_backend_is_asked_for() {
        let ctx = context();
        let backend = ctx.adapter.get_info().backend;
        let limits = ctx.device.limits();
        assert_eq!(limits, terminal_device_limits(backend), "{backend:?}");
        assert_eq!(
            limits.max_non_sampler_bindings == TERMINAL_VIEW_DESCRIPTORS,
            backend == wgpu::Backend::Dx12,
            "{backend:?}: only a DX12 device is asked for the terminal-sized heap"
        );
    }

    /// The heap's sizing leans on one property, checked here rather than
    /// assumed: a DROPPED bind group gives its descriptors back.
    #[cfg(windows)]
    #[test]
    fn a_dropped_bind_group_returns_its_view_descriptors() {
        let ctx = context();
        let device = &ctx.device;
        assert_eq!(
            device.limits().max_non_sampler_bindings,
            TERMINAL_VIEW_DESCRIPTORS,
            "the Windows device must be requested with the terminal-sized heap"
        );
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("heap recycle probe bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("heap recycle probe texture"),
            size: wgpu::Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        for _ in 0..12 {
            let groups: Vec<wgpu::BindGroup> = (0..TERMINAL_VIEW_DESCRIPTORS / 4)
                .map(|_| {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&view),
                        }],
                    })
                })
                .collect();
            drop(groups);
            ctx.queue.submit(std::iter::empty());
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("an empty submission completes");
        }
        let error = crate::block_on::block_on(scope.pop());
        assert!(
            error.is_none(),
            "3x the heap's descriptors, created and dropped in batches, must never \
             run it out: {error:?}"
        );
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
