// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// aterm GPU renderer: first-party Metal on macOS, first-party OpenGL ES on
// Linux and over WebGL2 in a browser, and wgpu on Windows.
//
// The terminal grid is drawn on the GPU: a glyph atlas texture plus instanced
// quads per cell (background fill and glyph). The renderer targets an on-screen
// window surface or an offscreen texture; the offscreen path reads pixels back
// into an `aterm_render::Frame`, so GPU output is verifiable headless like the
// CPU `read_image` oracle.

// Every build compiles with all lints on, whatever its arm count: a crossing
// refusal between backend arms compiles only under `cfg(multi_arm)` (build.rs),
// so the single-armed builds (macOS production: Metal; Linux and wasm: GL;
// Windows: wgpu) spell no unreachable pattern. See `device_layer`.

/// `eprintln!` that cannot panic, for every stderr line this crate writes at
/// runtime. `eprintln!` panics when stderr fails (a closed pipe or pty), and
/// these notes fire on the present path, where a panic aborts under
/// aterm-objc's trampoline policy. Same shape as
/// `aterm_gui::logging::stderr_line!`.
macro_rules! stderr_line {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr().lock(), $($arg)*);
    }};
}
pub(crate) use stderr_line;

/// The window's `--verbose` flag, handed down by the host at launch: the surface
/// notes (present mode, EDR swapchain, the scRGB fallbacks) print only under it.
static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record the host's `--verbose` flag (a launch flag, never an environment read).
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the host asked for `--verbose` (read by the wgpu and GL arms'
/// window surface notes).
#[cfg(any(wgpu_arm, all(gl_arm, not(target_arch = "wasm32"))))]
pub(crate) fn verbose() -> bool {
    VERBOSE.load(std::sync::atomic::Ordering::Relaxed)
}

use aterm_render::Frame;

mod device_layer;
mod format_plan;
// The pipeline table: the single declaration of every render pipeline (entry
// points, target role, blend, write mask, vertex layout, topology), read by the
// wgpu renderer and the Metal backend.
mod pipeline_table;
// A one-future executor for wgpu's async adapter/device acquisition on native.
// The wasm path has no wgpu and nothing to wait on (`web`).
#[cfg(wgpu_arm)]
mod block_on;

// The EDR present gate's pure decision functions, re-exported so
// tests/hdr_gate.rs enumerates the shipping attach and present policy
// (`hdr_swapchain_wants_f16`, `hdr_present_plan`).
pub use format_plan::{
    HdrPlan, HdrReconfigurePlan, hdr_live_upgrade_wants_f16, hdr_present_plan,
    hdr_reconfigure_plan, hdr_screen_upgrade_decision, hdr_screen_upgrade_wants_f16,
    hdr_swapchain_wants_f16, hdr_swapchain_wants_f16_on_screen,
};
// The first-party Metal backend, the renderer's arm on macOS. See
// `metal/mod.rs`.
#[cfg(target_os = "macos")]
mod metal;
// The first-party OpenGL ES arm, Linux's renderer and, over WebGL2, the
// browser's (`cfg(gl_arm)`, build.rs). See `gl/mod.rs`.
#[cfg(gl_arm)]
mod gl;
/// The launch's first-frame Metal warm (see `metal/warm.rs`).
#[cfg(target_os = "macos")]
pub use metal::warm::FirstFrameWarm;
/// The first-frame compile measurement (`benches/first_frame_compile.rs`).
#[cfg(target_os = "macos")]
#[doc(hidden)]
pub use metal::warm::{first_frame_bench_main, first_frame_bench_warm};
mod renderer;
/// A window the native backends attach a swapchain to (a browser has only
/// its canvas: `web`).
#[cfg(not(target_arch = "wasm32"))]
pub use renderer::WindowTarget;
pub use renderer::{
    DropOverlay, GpuRenderer, GpuSurface, PresentCrop, SurfacePresentFailure, TrayQuad, WindowGpu,
};
/// The demand-built effect pipelines (see `renderer::EffectPipelines`). The
/// enum and name table let a test name a slot and read
/// `GpuRenderer::effect_pipelines_resident`.
pub use renderer::{
    EFFECT_PIPELINE_COUNT, EFFECT_PIPELINE_NAMES, EFFECT_PIPELINES, EffectPipeline,
};
/// Present-to-glass timing: `presentDrawable:` registration to the drawable's
/// `presentedTime`.
pub mod present_glass;
/// Cold-build sub-phase timing for the GPU backend (see `startup_probe`).
pub mod startup_probe;
/// VIDEO introspection: submitted-destination-frame capture (see `video_tap`).
pub mod video_tap;
/// The browser entry: a canvas becomes a context and a pending surface.
#[cfg(target_arch = "wasm32")]
pub mod web;
// The GPU-free slice-precondition decision `VertexBuffer::upload_via` (and the
// Metal arm's `upload_stream`) uses,
// re-exported for the Tier-1 conformance test.
pub use renderer::should_slice;

// The DirectComposition visual swapchain opt-in (Windows).
//
// The names are backend-neutral: the latch says "present through a composition
// visual", and whichever Windows backend is compiled honours it. Today that is
// wgpu's DX12: its default surface is an HWND swapchain whose only composite
// mode is Opaque, which fully covers DWM's Mica/Acrylic backdrop, and its
// `Dx12SwapchainKind::DxgiFromVisual` builds a composition swapchain that offers
// non-opaque modes, so per-pixel alpha blends over the backdrop. The
// first-party D3D11 backend (docs/AUDIT-third-party-2026-10-15.md, GW.5) keeps
// the same latch over `CreateSwapChainForComposition`.
//
// The presentation system is an instance-descriptor property decided before any
// window exists, so the opt-in is a process-global latch the frontend sets from
// config before building the [`GpuContext`]. Not wgpu's
// `WGPU_DX12_PRESENTATION_SYSTEM` env knob, which would leak to child shells.
// The HWND path remains the default (`background_material = "none"`).
//
// Fail-soft: on a visual instance the composition stack is built lazily by the
// first `Surface::configure`, and a failure there reaches wgpu's default error
// handler, which panics. So `GpuRenderer::create_window_surface` runs that
// configure inside error scopes and returns `Err`; the frontend consults
// [`surface_attach_fallback`], withdraws this latch, rebuilds on the HWND
// swapchain (`GpuRenderer::rebuild_on_fresh_context`) and retries once. The
// material then styles only the caption, with one diagnostic.
#[cfg(windows)]
static VISUAL_SWAPCHAIN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether a request was withdrawn this run (GPU init failed, the first
/// composition configure failed, or the device was lost), so the "caption
/// only" diagnostic can say so rather than suggest a restart.
#[cfg(windows)]
static VISUAL_SWAPCHAIN_WITHDRAWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether [`GpuContext::new`] actually built the visual path (requested and
/// the compiled Windows backend, today wgpu's DX12, won adapter selection).
#[cfg(windows)]
static VISUAL_SWAPCHAIN_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Request that the next [`GpuContext::new`] build its Windows instance on the
/// visual swapchain path. Call before the context exists; loss recovery
/// re-reads the latch. Ignored by a backend without a composition swapchain.
#[cfg(windows)]
pub fn request_visual_swapchain() {
    VISUAL_SWAPCHAIN_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Withdraw the visual-swapchain request. Called by:
///   * the GPU-init-failure arm (later windows use plain HWND attributes, since
///     a softbuffer present into a `WS_EX_NOREDIRECTIONBITMAP` window shows
///     nothing);
///   * the GPU-loss downgrade (same reason);
///   * the fail-soft arm after the first composition configure failed, before
///     [`GpuContext::new`] re-reads the latch.
///
/// Records the withdrawal ([`visual_swapchain_withdrawn`]) when a request
/// was standing; otherwise a no-op.
#[cfg(windows)]
pub fn withdraw_visual_swapchain() {
    if VISUAL_SWAPCHAIN_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed) {
        VISUAL_SWAPCHAIN_WITHDRAWN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether a visual-swapchain request was made and then withdrawn this run.
/// Diagnostics only.
#[cfg(windows)]
pub fn visual_swapchain_withdrawn() -> bool {
    VISUAL_SWAPCHAIN_WITHDRAWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// What the frontend does when [`GpuRenderer::create_window_surface`] returns
/// `Err`. Pure and platform-independent, so the policy is tested everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceAttachFallback {
    /// Another window already presents on this instance, so it works: decline
    /// this window only. Rebuilding would orphan the live surfaces.
    DeclineWindow,
    /// First window on a visual instance: withdraw the latch, rebuild on the
    /// opaque HWND swapchain, and retry the attach once.
    RebuildOpaque,
    /// First/only window on a plain instance (or the opaque retry failed too):
    /// downgrade the whole app to the CPU softbuffer renderer.
    CpuRenderer,
}

/// The [`SurfaceAttachFallback`] for a failed attach: `RebuildOpaque` iff the
/// context is visual (`visual_swapchain_active`) and no other window presents
/// on it (`other_gpu_windows`).
#[must_use]
pub fn surface_attach_fallback(
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

/// Whether the visual swapchain has been requested. Window creation keys
/// `with_no_redirection_bitmap` off this, since
/// [`visual_swapchain_active`] is known only after GPU init.
#[cfg(windows)]
pub fn visual_swapchain_requested() -> bool {
    VISUAL_SWAPCHAIN_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the live [`GpuContext`] was actually built on the DirectComposition
/// visual path (ground truth for `aterm-ctl chrome`'s `client=` field).
#[cfg(windows)]
pub fn visual_swapchain_active() -> bool {
    VISUAL_SWAPCHAIN_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// wgpu device and queue, plus what we learned about the adapter.
///
/// `instance` and `adapter` are kept so a window surface can be created on the
/// same instance later (`GpuRenderer::create_window_surface`).
pub struct GpuContext {
    /// Present only where the wgpu arm compiles. The production macOS context
    /// is Metal-only; the armed renderer mints its own device (`MetalArmLive`).
    #[cfg(wgpu_arm)]
    pub(crate) device: wgpu::Device,
    #[cfg(wgpu_arm)]
    pub(crate) queue: wgpu::Queue,
    pub adapter_name: String,
    pub backend: String,
    /// Whether the device supports `DownlevelFlags::VIEW_FORMATS` (an sRGB view
    /// of `Rgba8Unorm` storage), which linear-light compositing needs. GLES and
    /// WebGL2 lack it and composite on an sRGB-typed offscreen instead.
    pub(crate) srgb_offscreen: bool,
    /// Kept alive so window surfaces can be created from this instance, and so
    /// `surface.get_capabilities(&adapter)` can be queried at surface setup.
    #[cfg(wgpu_arm)]
    pub(crate) instance: wgpu::Instance,
    #[cfg(wgpu_arm)]
    pub(crate) adapter: wgpu::Adapter,
    /// Set by wgpu's device-lost callback (driver install or update, TDR reset,
    /// eGPU unplug, `device.destroy()`). Without the callback wgpu drops the
    /// notification and the window freezes. The frontend polls
    /// [`Self::device_lost`] and rebuilds the GPU stack or downgrades to CPU.
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The driver's message from that callback (set once, only when non-empty),
    /// read back by [`Self::device_loss_reason`] for the frontend's one line.
    device_loss_msg: std::sync::Arc<std::sync::OnceLock<String>>,
    /// The Metal loss latch, wired once at construct so `device_lost()` (and
    /// `recover_from_gpu_loss`) consumes Metal loss through the same seam. Metal
    /// has no device-lost callback; loss is classified from command-buffer
    /// outcomes (`metal/loss.rs`).
    #[cfg(target_os = "macos")]
    metal_loss: std::sync::OnceLock<std::sync::Arc<metal::loss::LossLatch>>,
    /// Whether this instance was built on the DirectComposition visual path
    /// (requested and DX12 won adapter selection). Surfaces then offer
    /// non-opaque modes; the renderer keys backdrop-margin alpha and the
    /// PreMultiplied present off this. Always `false` off Windows.
    pub(crate) visual_swapchain: bool,
    /// The GL driver's 2-D texture ceiling, read by the context's probe (the
    /// shipped Linux build and wasm32, where no wgpu device answers it).
    #[cfg(all(gl_arm, not(wgpu_arm)))]
    pub(crate) gl_max_texture_dim: u32,
    /// The WebGL2 context on the canvas the context was made for: in a
    /// browser the device is the canvas's, so the renderer's GL arm draws on
    /// this one rather than minting its own (`web::GpuContext::for_canvas`).
    #[cfg(target_arch = "wasm32")]
    pub(crate) gl_canvas: gl::device::GlDevice,
}

/// Row alignment required by `copy_texture_to_buffer`, also the frame byte
/// contract the Metal readback reproduces; checked against wgpu's constant
/// where wgpu compiles.
const ALIGN: usize = 256;
#[cfg(wgpu_arm)]
const _: () = assert!(ALIGN == wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);

/// The padded readback row width: `copy_texture_to_buffer` strides are
/// multiples of 256, so every `Frame` readback stages through padded rows. The
/// Metal readback (`device_layer::metal_try_read_back`) uses the same stride so
/// the staging shape is identical on both backends.
pub(crate) fn padded_bytes_per_row(width: usize) -> usize {
    let unpadded = width * 4;
    unpadded.div_ceil(ALIGN) * ALIGN
}

/// Unpack a padded RGBA8 staging buffer into an `aterm_render::Frame`
/// (`0xTTRRGGBB`, top byte `255 - alpha`, the CPU's transmittance encoding),
/// stripping row padding. Shared by both readback arms.
pub(crate) fn frame_from_padded_rgba(bytes: &[u8], w: usize, h: usize, padded: usize) -> Frame {
    let mut pixels = Vec::with_capacity(w * h);
    for row in 0..h {
        let base = row * padded;
        let src = &bytes[base..base + w * 4];
        // `src` is exactly `w * 4` bytes, so the `as_chunks` remainder is always
        // empty — the discarded tail is the one `chunks_exact` also dropped.
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
/// `None` = unrecognized (caller warns and keeps the `LowPower` default).
#[cfg(wgpu_arm)]
fn parse_gpu_power(v: &str) -> Option<wgpu::PowerPreference> {
    match v.to_ascii_lowercase().as_str() {
        "low" => Some(wgpu::PowerPreference::LowPower),
        "high" => Some(wgpu::PowerPreference::HighPerformance),
        _ => None,
    }
}

/// One [`GpuContext::new`] at a time in a process: its instance, adapter and
/// device. Vulkan's loader and NVIDIA's ICD do not survive two instances made
/// at once (two threads in `vk_icdNegotiateLoaderICDInterfaceVersion`, one
/// jumps to address 0: measured on driver 580.159, libvulkan 1.4.328). The
/// window makes one context per process; parallel tests make many.
#[cfg(wgpu_arm)]
static GPU_INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `wgpu::Instance::new`, under a name of its own: the lock-order census
/// resolves a call held under [`GPU_INIT`] by callee name, and a bare `new`
/// there reads as `GpuContext::new` re-taking the lock.
#[cfg(wgpu_arm)]
fn fresh_instance(desc: wgpu::InstanceDescriptor) -> wgpu::Instance {
    wgpu::Instance::new(desc)
}

/// The Vulkan cell's instance flags: the build's, less `DEBUG`. `DEBUG` labels
/// every texture, pipeline and command buffer through the loader's
/// `vkSetDebugUtilsObjectNameEXT` trampoline, which faults while another
/// instance's `vkEnumeratePhysicalDevices` unloads the ICDs it does not use
/// (measured as above). Only a debug build sets `DEBUG`, and only a process with
/// two instances meets the race: parallel tests, never a window.
#[cfg(all(wgpu_arm, unix, not(target_vendor = "apple")))]
fn vulkan_instance_flags(flags: wgpu::InstanceFlags) -> wgpu::InstanceFlags {
    flags - wgpu::InstanceFlags::DEBUG
}

/// GPU selection development seams ([`aterm_types::dev_seam!`]; shipped
/// binaries read none of them):
///
/// * `ATERM_GPU_ADAPTER=<substring>`: pick the adapter whose name contains
///   this, case-insensitive; falls back to default selection with a warning.
/// * `ATERM_GPU_POWER=low|high`: power preference, default `low` (keeps hybrid
///   laptops on the iGPU).
///
/// The backend is not selectable: each platform compiles one backend (the
/// first-party Metal and OpenGL ES arms on macOS and Linux, where wgpu is only
/// the test oracle; wgpu's DX12 on Windows, its Vulkan elsewhere). A driver
/// that cannot render falls to the CPU present path; `gpu = false` in
/// aterm.toml chooses it outright.
#[cfg(wgpu_arm)]
fn default_backends() -> wgpu::Backends {
    #[cfg(windows)]
    let backends = wgpu::Backends::DX12;
    #[cfg(not(windows))]
    let backends = wgpu::Backends::PRIMARY;
    backends
}

/// The first-party Metal renderer is the macOS backend, unconditionally, with
/// no wgpu escape hatch: wgpu is not in the macOS production dependency graph.
/// A Mac where Metal fails to initialize falls to the softbuffer CPU present
/// path, the same floor as device loss. A `const fn` so every armed-path site
/// reads as this decision.
#[cfg(target_os = "macos")]
#[must_use]
pub const fn metal_backend_selected() -> bool {
    true
}

/// Claim the process's first-frame Metal warm, to [`FirstFrameWarm::run`] on a
/// worker: the first window's shaders compiled and discarded so its own compile
/// is a cache hit. Claim it on the thread that spawns the worker, so a mint
/// that starts before the worker does still waits for it. `None` once claimed.
#[cfg(target_os = "macos")]
#[must_use]
pub fn begin_first_frame_warm() -> Option<FirstFrameWarm> {
    metal::warm::FIRST_FRAME.begin()
}

/// Whether this process claimed its first-frame warm
/// ([`begin_first_frame_warm`]); a headless launch never does.
#[cfg(target_os = "macos")]
#[must_use]
pub fn first_frame_warm_began() -> bool {
    metal::warm::FIRST_FRAME.began()
}

/// The `ATERM_GPU_POWER` seam, parsed (see [`default_backends`] for the table).
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

/// The suballocator block floor in MiB: the smallest driver heap wgpu-hal's
/// allocator creates for the first resource of a memory type. It is an empty
/// reservation paid once per memory type.
///
/// A resource larger than the block size gets a dedicated heap of its exact
/// size, so a small floor is safe: the full-surface offscreen is dedicated
/// either way, and the pool holds only small resources (atlas, uniforms,
/// instance streams).
#[cfg(wgpu_arm)]
const MEMBLOCK_FLOOR_MIB: u64 = 16;

/// The block-size ceiling in MiB. The block size doubles per additional live
/// block of a memory type up to this, so real pooled demand still reaches big
/// blocks.
#[cfg(wgpu_arm)]
const MEMBLOCK_CEILING_MIB: u64 = 128;

/// Build a `MemoryHints::Manual` from a `(min, max)` device block size in MiB.
/// Host block sizes are half the device ones.
#[cfg(wgpu_arm)]
fn memblock_hints(min_mib: u64, max_mib: u64) -> wgpu::MemoryHints {
    const MIB: u64 = 1024 * 1024;
    wgpu::MemoryHints::Manual {
        suballocated_device_memory_block_size: (min_mib * MIB)..(max_mib * MIB),
    }
}

/// The suballocator sizing aterm asks its GPU device for.
///
/// [`wgpu::MemoryHints::Performance`] floors the suballocator at 128 MiB device
/// plus 64 MiB host of empty heap, created at the first buffer and texture. On
/// an integrated adapter that is all process working set, for a terminal that
/// uses a few MB (the one large resource, the offscreen, gets a dedicated heap
/// anyway). A 16 MiB device floor keeps the reservation at 24 MiB; smaller
/// floors cost more heap creations for little gain.
#[must_use]
#[cfg(wgpu_arm)]
pub(crate) fn terminal_memory_hints() -> wgpu::MemoryHints {
    memblock_hints(MEMBLOCK_FLOOR_MIB, MEMBLOCK_CEILING_MIB)
}

/// The view-descriptor heap aterm asks a DX12 device for, in descriptors
/// (`wgpu::Limits::max_non_sampler_bindings`).
///
/// wgpu-hal's DX12 backend creates one shader-visible CBV/SRV/UAV descriptor
/// heap of exactly this many descriptors when the device is created, and
/// carves every bind group's buffers and texture views out of it for the bind
/// group's life. wgpu's default is 1,000,000, a bindless engine's number. No
/// other backend sizes anything by the limit, so only a DX12 device is asked
/// for a smaller one ([`terminal_device_limits`]).
///
/// The working tree this was ported from (last touched 2026-09-29, parked as
/// 6ad1a274d8) recorded, on an AMD Radeon 780M under DX12 (an integrated
/// adapter, so the heap is system RAM): the default heap as one 30.5 MiB
/// region (32 bytes a descriptor), every page resident and zero, and a bare
/// device at 125 MB working set / 98 MB private with it against 95 MB / 68 MB
/// with a 4,096 heap. Those are that snapshot's numbers, not re-measured.
///
/// What a terminal holds: sixteen `create_bind_group` sites in `renderer.rs`,
/// none with more than two non-sampler entries (samplers live in their own
/// heap). A dropped bind group returns its range once the GPU is done with it
/// (`a_dropped_bind_group_returns_its_view_descriptors`). Running out would
/// fail `create_bind_group` with an out-of-memory device error, so 16,384 is
/// sized for headroom (hundreds of windows at that rate), not for the
/// measurement.
#[cfg(wgpu_arm)]
const TERMINAL_VIEW_DESCRIPTORS: u32 = 16_384;

/// The device limits aterm asks of an adapter on `backend`: wgpu's defaults,
/// with a DX12 view-descriptor heap sized for a terminal
/// ([`TERMINAL_VIEW_DESCRIPTORS`]). DX12 compiles only into the Windows build,
/// so every other build asks for wgpu's defaults, as before.
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

/// The instance flags aterm asks for over `backends`: `flags`, less
/// indirect-call validation on a DX12 instance, which is the Windows build's
/// ([`default_backends`]) and no other's.
///
/// `VALIDATION_INDIRECT_CALL` (in wgpu's defaults for every build) makes
/// wgpu-core build two compute pipelines, their layouts and their shader at
/// device creation, to validate the arguments of indirect draws and
/// dispatches. aterm issues none (clippy.toml bans wgpu's indirect draws and
/// dispatches: no indirect call), so they are built for nothing. The same
/// snapshot (6ad1a274d8) recorded, on that Radeon 780M under DX12 (a bare
/// device, three paired runs), 78.5 to 74.0 MB working set in every pair and
/// device creation 167/242/147 to 125/178/121 ms; Metal and Vulkan were not
/// measured and keep wgpu's flags.
#[cfg(wgpu_arm)]
fn terminal_instance_flags(
    flags: wgpu::InstanceFlags,
    backends: wgpu::Backends,
) -> wgpu::InstanceFlags {
    if backends == wgpu::Backends::DX12 {
        flags - wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    } else {
        flags
    }
}

/// The `ATERM_GPU_ADAPTER` seam (see [`default_backends`]).
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
    /// The production macOS constructor (no wgpu): no adapter negotiation. The
    /// context carries neutral facts (device name via the Metal FFI, the loss
    /// domain wired at construct); the armed renderer mints its own device on
    /// the render thread (`MetalArmLive::new`). `srgb_offscreen` is true: Metal
    /// always supports sRGB views of Unorm storage.
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

    /// The production Linux constructor (no wgpu): a headless probe of the
    /// GL arm's driver (`libEGL.so.1`, an EGL 1.5 display, a GLES 3.0
    /// context), whose facts the context keeps: the renderer's name, the
    /// texture ceiling, and whether the native format plan's two views exist
    /// (`srgb_offscreen`). The probe's device is dropped; the renderer mints
    /// its own on the render thread, on the window system's display once a
    /// window attaches (`gl_arm::GpuRenderer::gl_arm_ready`), and takes these
    /// facts from that device, which can be another GPU than the probe's.
    /// `Err` (no EGL, no GLES 3 driver) sends the frontend to the CPU renderer.
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

    /// Acquire a GPU headless, picking the default low-power adapter, subject to
    /// the development `ATERM_GPU_*` seams (see [`default_backends`]).
    ///
    /// Blocks via the crate's private `block_on`. The browser's path is
    /// `web`'s `GpuContext::for_canvas`.
    #[cfg(wgpu_arm)]
    pub fn new() -> Result<Self, String> {
        // The instance outlives device creation so window surfaces can be made
        // from it later. It is surface-capable without a display handle except
        // on GLES/Wayland, so the adapter request can use
        // `compatible_surface: None`.
        let desc = wgpu::InstanceDescriptor {
            backends: default_backends(),
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
        // Honour the visual-swapchain latch via the explicit descriptor field.
        // Ignored by non-DX12 backends; `visual_swapchain` below records the
        // outcome.
        #[cfg(windows)]
        let desc = {
            let mut desc = desc;
            if visual_swapchain_requested() {
                desc.backend_options.dx12.presentation_system =
                    wgpu::Dx12SwapchainKind::DxgiFromVisual;
            }
            desc
        };
        let ctx = {
            let _one_at_a_time = GPU_INIT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // The first timed cold-build leg: backend enumeration and driver load.
            let instance =
                startup_probe::timed(startup_probe::Leg::GpuInstance, || fresh_instance(desc));
            crate::block_on::block_on(Self::from_instance(instance))?
        };
        // The visual path is active only if DX12 won adapter selection.
        // Recorded on the context and the process global.
        #[cfg(windows)]
        let ctx = {
            let mut ctx = ctx;
            ctx.visual_swapchain = visual_swapchain_requested() && ctx.backend == "Dx12";
            VISUAL_SWAPCHAIN_ACTIVE
                .store(ctx.visual_swapchain, std::sync::atomic::Ordering::Relaxed);
            ctx
        };
        // Mint the process Metal loss domain now and wire it into
        // `device_lost()`, so the frontend's poll sees Metal loss from the first
        // armed frame.
        #[cfg(target_os = "macos")]
        if metal_backend_selected() {
            let wired =
                ctx.wire_metal_loss_latch(std::sync::Arc::new(metal::loss::LossLatch::new()));
            debug_assert!(wired, "a fresh context's loss-latch cell is empty");
        }
        Ok(ctx)
    }

    /// Async adapter and device acquisition on an existing `wgpu::Instance`,
    /// driven by this crate's `block_on`; no compatible surface (every
    /// instance here is surface-capable without one).
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
                required_features: wgpu::Features::empty(),
                required_limits,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                // Not `MemoryHints::Performance`; see `terminal_memory_hints`.
                memory_hints: terminal_memory_hints(),
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| e.to_string())?;
        startup_probe::record(startup_probe::Leg::GpuDevice, device_started.elapsed());
        let context_tail_started = aterm_time::Instant::now();
        // sRGB texture-VIEW aliasing requires VIEW_FORMATS; absent on GLES/WebGL2.
        let srgb_offscreen = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS);
        // Register a device-lost callback (wgpu drops the notification
        // otherwise) that keeps the driver's message and flips an atomic the
        // frontend polls; the frontend prints the one line. Runs at most once.
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
            // Seeded false; ONLY the native `new()` (which owns the descriptor
            // decision) upgrades it — the wasm/web path never has a DComp visual.
            visual_swapchain: false,
        };
        startup_probe::record(
            startup_probe::Leg::GpuContextTail,
            context_tail_started.elapsed(),
        );
        Ok(ctx)
    }

    /// Whether the device has been reported lost. Sticky: a lost device is never
    /// revived; the frontend rebuilds or downgrades to CPU. One relaxed load,
    /// safe to poll every frame.
    #[inline]
    pub fn device_lost(&self) -> bool {
        #[cfg(target_os = "macos")]
        if self.metal_loss.get().is_some_and(|l| l.is_lost()) {
            return true;
        }
        self.device_lost.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Latch a window surface wgpu refused to configure as this context's
    /// loss, with `why` as the cause when it is the first: the frontend's loss
    /// poll ([`Self::device_lost`]) then downgrades to the CPU renderer, as for
    /// a lost device, where a surface it presents to was refused (its window
    /// system gone, a lost swapchain). Sticky like a device loss.
    #[cfg(wgpu_arm)]
    pub(crate) fn latch_surface_loss(&self, why: String) {
        let _ = self.device_loss_msg.set(why);
        self.device_lost
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The first latched loss's cause, for the downgrade line: the Metal latch's
    /// classification (e.g. `MTLCommandBufferErrorPageFault`), else the driver
    /// message the wgpu device-lost callback stored (its reason when the message
    /// is empty). `None` when neither has one.
    #[must_use]
    pub fn device_loss_reason(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        if let Some(r) = self.metal_loss.get().and_then(|l| l.reason()) {
            return Some(r.to_owned());
        }
        self.device_loss_msg.get().cloned()
    }

    /// Wire a Metal loss latch into [`Self::device_lost`] (see `metal_loss`).
    /// One latch per context; a second wire returns `false` rather than
    /// swapping domains.
    #[cfg(target_os = "macos")]
    pub(crate) fn wire_metal_loss_latch(
        &self,
        latch: std::sync::Arc<metal::loss::LossLatch>,
    ) -> bool {
        self.metal_loss.set(latch).is_ok()
    }

    /// The armed renderer's loss domain: the latch wired at construct, or a
    /// fresh one wired now for the test-armed path. It is the latch
    /// `device_lost()` polls.
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_loss_latch_or_wire(&self) -> std::sync::Arc<metal::loss::LossLatch> {
        if let Some(l) = self.metal_loss.get() {
            return std::sync::Arc::clone(l);
        }
        let _ = self.wire_metal_loss_latch(std::sync::Arc::new(metal::loss::LossLatch::new()));
        std::sync::Arc::clone(self.metal_loss.get().expect("wired just above"))
    }

    /// Clamp a texture dimension to `1..=max_texture_dimension_2d`, the
    /// `GpuContext` equivalent of [`GpuRenderer::clamp_fb_dim`]. A 0 or oversized
    /// extent would abort in wgpu's default error handler.
    #[inline]
    #[cfg(wgpu_arm)]
    fn clamp_tex_dim(&self, d: u32) -> u32 {
        d.max(1).min(self.device.limits().max_texture_dimension_2d)
    }

    /// This context as the device layer's wgpu arm. The Metal arm wraps the
    /// armed renderer's own mint (`renderer.rs::MetalArmLive::layer`), which
    /// lives on the render thread rather than on this context.
    #[cfg(wgpu_arm)]
    pub(crate) fn device_layer(&self) -> crate::device_layer::DeviceHandle<'_> {
        crate::device_layer::DeviceHandle::Wgpu {
            device: &self.device,
            queue: &self.queue,
        }
    }

    /// Create an offscreen colour target (Rgba8Unorm; render, copy-src,
    /// texture-binding). `TEXTURE_BINDING` lets the present blit sample this
    /// texture, so presented bytes match the introspection readback.
    #[cfg(wgpu_arm)]
    pub(crate) fn offscreen_texture(&self, width: u32, height: u32) -> wgpu::Texture {
        // Clamp to the device's max 2D dimension at this choke point. This
        // bounds only the texture: a caller that also reads back must clamp its
        // own copy extent (`encode_frame` pre-clamps).
        let width = self.clamp_tex_dim(width);
        let height = self.clamp_tex_dim(height);
        // Base passes blend in linear light: with VIEW_FORMATS the texture is
        // Rgba8Unorm plus an sRGB alias view; on WebGL2/GLES the texture itself
        // is Rgba8UnormSrgb. Stored bytes are sRGB either way. `COPY_DST` serves
        // the scroll band shift; `TexUsage::OFFSCREEN` is the full usage set.
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

    /// The offscreen colour-target format (the `off.view` format), from
    /// [`crate::format_plan`]. Every pipeline whose pass attaches `off.view`
    /// builds with it.
    #[cfg(wgpu_arm)]
    pub(crate) fn offscreen_format(&self) -> wgpu::TextureFormat {
        crate::format_plan::offscreen_format(self.srgb_offscreen)
    }

    /// The sRGB view format attached by the base, cursor and deco-over passes.
    /// See [`crate::format_plan`].
    #[cfg(wgpu_arm)]
    pub(crate) fn offscreen_srgb_view_format(&self) -> wgpu::TextureFormat {
        crate::format_plan::offscreen_srgb_view_format(self.srgb_offscreen)
    }

    /// The offscreen's two view formats for
    /// [`crate::pipeline_table::TargetFormats::resolve`]; each pipeline's
    /// [`crate::pipeline_table::TargetRole`] chooses. Add the swapchain with
    /// `with_present`.
    #[cfg(wgpu_arm)]
    pub(crate) fn pipeline_targets(&self) -> crate::pipeline_table::TargetFormats {
        crate::pipeline_table::TargetFormats {
            offscreen_srgb: self.offscreen_srgb_view_format(),
            offscreen_unorm: self.offscreen_format(),
            present: None,
        }
    }

    /// Read an Rgba8Unorm texture back into an `aterm_render::Frame`
    /// (`0xTTRRGGBB`, top byte `255 - alpha`, so opaque texels are `0x00RRGGBB`),
    /// stripping row padding.
    ///
    /// Infallible wrapper over [`Self::try_read_back`]: a failure (device lost)
    /// yields a black frame. Artifact producers must use the fallible path.
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

    /// Fallible core of [`Self::read_back`]: propagates buffer-map / device-poll
    /// failure (device loss) as `Err` instead of panicking.
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
        self.queue.submit([enc.finish()]);

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("GPU poll failed: {e}"))?;
        // The wait-poll above guarantees the map callback ran on success; a missing
        // result means the poll returned without completing the map (device loss).
        match rx.try_recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("readback buffer map failed: {e}")),
            Err(_) => return Err("readback buffer map never completed".to_string()),
        }
        let data = slice.get_mapped_range();
        // Deref the `BufferView` once and walk row slices as `[u8; 4]` texels,
        // avoiding a mapped-range deref and bounds check per channel.
        let bytes: &[u8] = &data;
        // The unpack is the shared `frame_from_padded_rgba`, one spelling of
        // the Frame byte contract for both backends.
        let frame = frame_from_padded_rgba(bytes, w, h, padded);
        drop(data);
        buffer.unmap();
        Ok(frame)
    }
}

#[cfg(all(test, wgpu_arm))]
mod device_request_tests {
    use super::*;

    /// The DX12 view-descriptor heap is the one device limit aterm moves off
    /// wgpu's defaults, and only for a DX12 adapter: every other limit, and
    /// every other backend's whole request, is wgpu's default.
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

    /// Indirect-call validation is dropped from a DX12 instance and nothing
    /// else moves; every other instance keeps the flags it was given. The
    /// DX12-only instance is the Windows build's and no other build's, so off
    /// Windows the shipped flags are wgpu's, untouched.
    #[test]
    fn indirect_call_validation_is_dropped_on_a_dx12_instance_only() {
        let indirect = wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL;
        let defaults = wgpu::InstanceFlags::default();
        assert!(
            defaults.contains(indirect),
            "wgpu's defaults carry it, or there is nothing to drop"
        );
        let dx12 = terminal_instance_flags(defaults, wgpu::Backends::DX12);
        assert!(!dx12.contains(indirect), "a DX12 instance drops it");
        assert_eq!(dx12 | indirect, defaults, "and nothing else");
        for backends in [
            wgpu::Backends::METAL,
            wgpu::Backends::VULKAN,
            wgpu::Backends::GL,
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
/// floors to 1 and caps at `max_texture_dimension_2d`. Two different oversized
/// widths landing on one width shows the cap is the device limit.
#[cfg(all(test, wgpu_arm))]
mod offscreen_extent_tests {
    #[test]
    fn offscreen_texture_clamps_zero_and_oversized_dims() {
        let ctx = super::GpuContext::new()
            .unwrap_or_else(|e| aterm_spec::host::could_not_run(format!("no GPU available: {e}")));
        // Reaching each assert == `create_texture` did not abort.
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

    /// The request reaches the device: a DX12 device holds the terminal-sized
    /// heap, every other device wgpu's default limits (macOS's oracle lane
    /// runs on Metal, so this is where it shows its request is unchanged).
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
    /// assumed: a DROPPED bind group gives its descriptors back. Three heaps'
    /// worth of bind groups are created and dropped in batches of a quarter
    /// heap; if ranges leaked, creation would run the heap out of memory before
    /// the first third was through. Only DX12 has the heap, and only the
    /// Windows build compiles DX12.
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
        // wgpu-core answers an error scope on its first poll.
        let error = crate::block_on::block_on(scope.pop());
        assert!(
            error.is_none(),
            "3x the heap's descriptors, created and dropped in batches, must never \
             run it out: {error:?}"
        );
    }
}

/// Fail-soft: the latch-withdrawal decision a failed window-surface attach
/// drives (`surface_attach_fallback`). Pure, so it runs on every platform.
#[cfg(test)]
mod surface_attach_fallback_tests {
    use super::{SurfaceAttachFallback as F, surface_attach_fallback};

    /// `RebuildOpaque` iff visual and no other GPU window; other windows
    /// dominate. One labelled row per input.
    #[test]
    fn rebuild_iff_visual_and_alone() {
        // (visual, other_gpu_windows, verdict, why)
        let rows = [
            // Material set, DX12 won, DComp unavailable, first window: the only
            // case that withdraws the latch and rebuilds.
            (true, false, F::RebuildOpaque, "visual, first window"),
            // A later window failing on a working visual instance: decline that
            // window only.
            (true, true, F::DeclineWindow, "visual, live GPU windows"),
            // The default HWND swapchain: decline when others present, else the
            // CPU downgrade.
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
