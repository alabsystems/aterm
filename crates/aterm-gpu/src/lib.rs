// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// aterm GPU renderer: wgpu, and first-party Metal on macOS.
//
// The terminal grid is drawn on the GPU: a glyph atlas texture plus instanced
// quads per cell (background fill and glyph). The renderer targets an on-screen
// window surface or an offscreen texture; the offscreen path reads pixels back
// into an `aterm_render::Frame`, so GPU output is verifiable headless like the
// CPU `read_image` oracle.

// The wgpu-free macOS production build leaves some oracle-shared code
// single-armed (device-layer seams collapse to their Metal arm) or without a
// production caller. The oracle build (every test target enables
// `wgpu-oracle`) compiles both arms with all lints on; this suppression
// applies only to the production build.
#![cfg_attr(not(wgpu_arm), allow(unreachable_patterns, irrefutable_let_patterns))]

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

/// Whether the host asked for `--verbose` (read by the wgpu arm's surface notes).
#[cfg(wgpu_arm)]
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
// The browser must not block its main thread, so wasm awaits instead.
#[cfg(all(wgpu_arm, not(target_arch = "wasm32")))]
mod block_on;

// The EDR present gate's pure decision functions, re-exported so
// tests/hdr_gate.rs enumerates the shipping policy.
pub use format_plan::{
    HdrPlan, HdrReconfigurePlan, hdr_live_upgrade_wants_f16, hdr_present_plan,
    hdr_reconfigure_plan, hdr_screen_upgrade_wants_f16, hdr_swapchain_wants_f16,
    hdr_swapchain_wants_f16_on_screen,
};
// The first-party Metal backend, the renderer's arm on macOS. See
// `metal/mod.rs`.
#[cfg(target_os = "macos")]
mod metal;
mod renderer;
pub use renderer::{
    DropOverlay, GpuRenderer, GpuSurface, PresentCrop, SurfacePresentFailure, TrayQuad, WindowGpu,
    WindowTarget,
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
// The GPU-free slice-precondition decision `VertexBuffer::upload_via` (and the
// Metal arm's `upload_stream`) uses,
// re-exported for the Tier-1 conformance test.
pub use renderer::should_slice;

// The DirectComposition visual swapchain opt-in (Windows).
//
// The default DX12 surface is an HWND swapchain whose only composite mode is
// Opaque, which fully covers DWM's Mica/Acrylic backdrop. wgpu's
// `Dx12SwapchainKind::DxgiFromVisual` builds a composition swapchain that offers
// non-opaque modes, so per-pixel alpha blends over the backdrop.
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
static DX12_VISUAL_SWAPCHAIN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether a request was withdrawn this run (GPU init failed, the first
/// composition configure failed, or the device was lost), so the "caption
/// only" diagnostic can say so rather than suggest a restart.
#[cfg(windows)]
static DX12_VISUAL_SWAPCHAIN_WITHDRAWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether [`GpuContext::new`] actually built the visual path (requested and
/// the DX12 backend won adapter selection).
#[cfg(windows)]
static DX12_VISUAL_SWAPCHAIN_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Request that the next [`GpuContext::new`] build its DX12 instance on the
/// visual swapchain path. Call before the context exists; loss recovery
/// re-reads the latch. Ignored by non-DX12 backends.
#[cfg(windows)]
pub fn request_dx12_visual_swapchain() {
    DX12_VISUAL_SWAPCHAIN_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Withdraw the visual-swapchain request. Called by:
///   * the GPU-init-failure arm (later windows use plain HWND attributes, since
///     a softbuffer present into a `WS_EX_NOREDIRECTIONBITMAP` window shows
///     nothing);
///   * the GPU-loss downgrade (same reason);
///   * the fail-soft arm after the first composition configure failed, before
///     [`GpuContext::new`] re-reads the latch.
///
/// Records the withdrawal ([`dx12_visual_swapchain_withdrawn`]) when a request
/// was standing; otherwise a no-op.
#[cfg(windows)]
pub fn withdraw_dx12_visual_swapchain() {
    if DX12_VISUAL_SWAPCHAIN_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed) {
        DX12_VISUAL_SWAPCHAIN_WITHDRAWN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether a visual-swapchain request was made and then withdrawn this run.
/// Diagnostics only.
#[cfg(windows)]
pub fn dx12_visual_swapchain_withdrawn() -> bool {
    DX12_VISUAL_SWAPCHAIN_WITHDRAWN.load(std::sync::atomic::Ordering::Relaxed)
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
/// [`dx12_visual_swapchain_active`] is known only after GPU init.
#[cfg(windows)]
pub fn dx12_visual_swapchain_requested() -> bool {
    DX12_VISUAL_SWAPCHAIN_REQUESTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Whether the live [`GpuContext`] was actually built on the DirectComposition
/// visual path (ground truth for `aterm-ctl chrome`'s `client=` field).
#[cfg(windows)]
pub fn dx12_visual_swapchain_active() -> bool {
    DX12_VISUAL_SWAPCHAIN_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// wgpu device and queue, plus what we learned about the adapter.
///
/// `instance` and `adapter` are kept so a window surface can be created on the
/// same instance later (`GpuRenderer::create_window_surface`).
pub struct GpuContext {
    /// Present only where the wgpu arm compiles. The production macOS context
    /// is Metal-only; the armed renderer mints its own device (`MetalArmLive`).
    #[cfg(wgpu_arm)]
    pub device: wgpu::Device,
    #[cfg(wgpu_arm)]
    pub queue: wgpu::Queue,
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

/// GPU selection development seams ([`aterm_types::dev_seam!`]; shipped
/// binaries read none of them):
///
/// * `ATERM_GPU_ADAPTER=<substring>`: pick the adapter whose name contains
///   this, case-insensitive; falls back to default selection with a warning.
/// * `ATERM_GPU_POWER=low|high`: power preference, default `low` (keeps hybrid
///   laptops on the iGPU).
/// * `ATERM_GPU_MEMBLOCK=performance|small|default|<4..=256>`: the
///   suballocator's block-size floor; `<N>` is the device floor in MiB (host is
///   half). Default 16 MiB; see [`terminal_memory_hints`].
///
/// The backend is not selectable: each platform compiles one wgpu backend
/// (DX12 on Windows, Vulkan elsewhere). A driver that cannot render falls to
/// the CPU present path; `gpu = false` in aterm.toml chooses it outright.
#[cfg(not(target_arch = "wasm32"))]
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

/// Parse `ATERM_GPU_MEMBLOCK` (see [`terminal_memory_hints`]).
/// `None` = unrecognized (caller warns and keeps the default).
#[cfg(wgpu_arm)]
fn parse_memblock(v: &str) -> Option<wgpu::MemoryHints> {
    match v.to_ascii_lowercase().as_str() {
        "performance" => Some(wgpu::MemoryHints::Performance),
        "small" => Some(wgpu::MemoryHints::MemoryUsage),
        "default" => Some(terminal_memory_hints_default()),
        other => {
            let mib: u64 = other.parse().ok()?;
            // gpu-allocator clamps to 4..=256 MiB itself; refuse a value outside
            // that rather than silently honouring a different number.
            if !(4..=256).contains(&mib) {
                return None;
            }
            Some(memblock_hints(mib, mib.max(MEMBLOCK_CEILING_MIB)))
        }
    }
}

/// Build a `MemoryHints::Manual` from a `(min, max)` device block size in MiB.
/// Host block sizes are half the device ones.
#[cfg(wgpu_arm)]
fn memblock_hints(min_mib: u64, max_mib: u64) -> wgpu::MemoryHints {
    const MIB: u64 = 1024 * 1024;
    wgpu::MemoryHints::Manual {
        suballocated_device_memory_block_size: (min_mib * MIB)..(max_mib * MIB),
    }
}

/// The shipped block sizing, before the env override.
#[cfg(wgpu_arm)]
fn terminal_memory_hints_default() -> wgpu::MemoryHints {
    memblock_hints(MEMBLOCK_FLOOR_MIB, MEMBLOCK_CEILING_MIB)
}

/// The suballocator sizing aterm asks its GPU device for.
///
/// [`wgpu::MemoryHints::Performance`] floors the suballocator at 128 MiB device
/// plus 64 MiB host of empty heap, created at the first buffer and texture. On
/// an integrated adapter that is all process working set, for a terminal that
/// uses a few MB (the one large resource, the offscreen, gets a dedicated heap
/// anyway). A 16 MiB device floor keeps the reservation at 24 MiB; smaller
/// floors cost more heap creations for little gain.
///
/// A development build can override it with `ATERM_GPU_MEMBLOCK` (the `<MiB>`
/// arm sets the device floor, clamped to 4..=256); see [`default_backends`].
#[must_use]
#[cfg(wgpu_arm)]
pub fn terminal_memory_hints() -> wgpu::MemoryHints {
    match aterm_types::dev_seam!("ATERM_GPU_MEMBLOCK").map(|v| v.to_string_lossy().into_owned()) {
        Some(v) if !v.is_empty() => parse_memblock(&v).unwrap_or_else(|| {
            crate::stderr_line!(
                "aterm-gpu: unknown ATERM_GPU_MEMBLOCK {v:?} \
                 (want performance|small|default|<4..=256 MiB>); using default"
            );
            terminal_memory_hints_default()
        }),
        _ => terminal_memory_hints_default(),
    }
}

/// The `ATERM_GPU_ADAPTER` seam (see [`default_backends`]). Native only; wasm
/// has one adapter.
#[cfg(all(wgpu_arm, not(target_arch = "wasm32")))]
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

// Adapter enumeration is native-only; the wasm build has exactly one adapter.
#[cfg(all(wgpu_arm, target_arch = "wasm32"))]
async fn adapter_from_env(_instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    None
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

    /// Acquire a GPU headless, picking the default low-power adapter, subject to
    /// the development `ATERM_GPU_*` seams (see [`default_backends`]).
    ///
    /// Native only: it blocks via the crate's private `block_on`. The wasm path
    /// awaits instead (see [`GpuContext::request`]).
    #[cfg(all(wgpu_arm, not(target_arch = "wasm32")))]
    pub fn new() -> Result<Self, String> {
        // The instance outlives device creation so window surfaces can be made
        // from it later. It is surface-capable without a display handle except
        // on GLES/Wayland, so the adapter request can use
        // `compatible_surface: None`.
        let desc = wgpu::InstanceDescriptor {
            backends: default_backends(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        };
        // Honour the visual-swapchain latch via the explicit descriptor field.
        // Ignored by non-DX12 backends; `visual_swapchain` below records the
        // outcome.
        #[cfg(windows)]
        let desc = {
            let mut desc = desc;
            if dx12_visual_swapchain_requested() {
                desc.backend_options.dx12.presentation_system =
                    wgpu::Dx12SwapchainKind::DxgiFromVisual;
            }
            desc
        };
        // The first timed cold-build leg: backend enumeration and driver load.
        let instance = startup_probe::timed(startup_probe::Leg::GpuInstance, || {
            wgpu::Instance::new(desc)
        });
        let ctx = crate::block_on::block_on(Self::from_instance(instance))?;
        // The visual path is active only if DX12 won adapter selection.
        // Recorded on the context and the process global.
        #[cfg(windows)]
        let ctx = {
            let mut ctx = ctx;
            ctx.visual_swapchain = dx12_visual_swapchain_requested() && ctx.backend == "Dx12";
            DX12_VISUAL_SWAPCHAIN_ACTIVE
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
    /// shared by the native (`block_on`) and wasm (`.await`) paths. No
    /// compatible surface; the WebGL backend uses
    /// [`from_instance_with_surface`](Self::from_instance_with_surface).
    #[cfg(wgpu_arm)]
    pub async fn from_instance(instance: wgpu::Instance) -> Result<Self, String> {
        Self::from_instance_with_surface(instance, None).await
    }

    /// Async adapter and device acquisition with an optional
    /// `compatible_surface`: WebGL can only enumerate an adapter compatible
    /// with the canvas surface. Native passes `None`.
    #[cfg(wgpu_arm)]
    pub async fn from_instance_with_surface(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, String> {
        let adapter_started = aterm_time::Instant::now();
        let adapter = match adapter_from_env(&instance).await {
            Some(adapter) => adapter,
            None => instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: power_preference_from_env(),
                    compatible_surface,
                    force_fallback_adapter: false,
                })
                .await
                .map_err(|e| format!("no GPU adapter available: {e}"))?,
        };
        let info = adapter.get_info();
        startup_probe::record(startup_probe::Leg::GpuAdapter, adapter_started.elapsed());
        let device_started = aterm_time::Instant::now();
        // WebGL2 cannot satisfy `Limits::default()` (no compute, lower ceilings),
        // so on wasm32 request the adapter's own limits, which also keeps its
        // real texture-dimension ceiling for Hi-DPI canvases.
        #[cfg(target_arch = "wasm32")]
        let required_limits = adapter.limits();
        #[cfg(not(target_arch = "wasm32"))]
        let required_limits = wgpu::Limits::default();
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

    /// This context as the device layer's wgpu arm. The Metal arm is minted only
    /// by the differential tests (`metal::resources::MetalResourceDevice`).
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
    pub fn offscreen_texture(&self, width: u32, height: u32) -> wgpu::Texture {
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
    pub fn read_back(&self, texture: &wgpu::Texture, width: u32, height: u32) -> Frame {
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
    pub fn try_read_back(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<Frame, String> {
        let (w, h) = (width as usize, height as usize);
        let padded = padded_bytes_per_row(w);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aterm-gpu readback"),
            size: (padded * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
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

#[cfg(test)]
mod env_override_tests {
    use super::*;

    #[test]
    fn power_parses_low_and_high_only() {
        assert_eq!(
            parse_gpu_power("high"),
            Some(wgpu::PowerPreference::HighPerformance)
        );
        assert_eq!(
            parse_gpu_power("LOW"),
            Some(wgpu::PowerPreference::LowPower)
        );
        assert_eq!(parse_gpu_power("medium"), None);
    }

    /// The block-size range a `MemoryHints` carries, in MiB, or `None` for a
    /// variant that carries no explicit range.
    fn range_mib(h: &wgpu::MemoryHints) -> Option<(u64, u64)> {
        const MIB: u64 = 1024 * 1024;
        match h {
            wgpu::MemoryHints::Manual {
                suballocated_device_memory_block_size: r,
            } => Some((r.start / MIB, r.end / MIB)),
            _ => None,
        }
    }

    /// The floor: 16 MiB device (8 MiB host), growing to 128 MiB under real
    /// pressure. See `terminal_memory_hints`.
    #[test]
    fn the_shipped_floor_is_terminal_sized_not_streaming_sized() {
        let d = terminal_memory_hints_default();
        assert_eq!(
            range_mib(&d),
            Some((16, 128)),
            "the shipped hint must be an explicit Manual range, not a wgpu preset"
        );
        // The wgpu presets this replaces, for the record: `Performance` floors at
        // 128 MiB device / 64 MiB host, which is what regressing to it would cost.
        assert!(
            matches!(d, wgpu::MemoryHints::Manual { .. }),
            "a preset would put the floor back in wgpu's hands"
        );
    }

    #[test]
    fn memblock_env_parses_the_documented_table() {
        assert!(matches!(
            parse_memblock("performance"),
            Some(wgpu::MemoryHints::Performance)
        ));
        assert!(matches!(
            parse_memblock("SMALL"),
            Some(wgpu::MemoryHints::MemoryUsage)
        ));
        assert_eq!(
            range_mib(&parse_memblock("default").expect("default parses")),
            Some((16, 128))
        );
        // A numeric floor sets the DEVICE minimum; the ceiling never drops below
        // the shipped one, so growth under pressure is never taken away.
        assert_eq!(
            range_mib(&parse_memblock("8").expect("8 parses")),
            Some((8, 128))
        );
        assert_eq!(
            range_mib(&parse_memblock("256").expect("256 parses")),
            Some((256, 256))
        );
        // gpu-allocator itself clamps to 4..=256 MiB; a value outside that would
        // silently become a different number, so refuse it instead.
        assert!(parse_memblock("3").is_none(), "below gpu-allocator's floor");
        assert!(parse_memblock("257").is_none(), "above its ceiling");
        assert!(parse_memblock("0").is_none());
        assert!(parse_memblock("lots").is_none());
        assert!(parse_memblock("").is_none());
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
