// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// aterm GPU renderer (wgpu → Metal on macOS).
//
// The terminal grid is drawn on the GPU: a glyph atlas texture + one instanced
// quad per cell (background fill + glyph blit). The SAME renderer can target an
// on-screen window surface OR an offscreen texture; the offscreen path reads the
// pixels back into an `aterm_render::Frame` so GPU output is verifiable headless
// (PNG round-trip) — exactly like the CPU `read_image` oracle, but on the GPU.
//
// This file currently lands the device + offscreen readback foundation; glyph
// rendering builds on top.

use aterm_render::Frame;

mod format_plan;

// M3 phase B: the EDR present gate's pure decision functions, re-exported so the
// Tier-1 exhaustive enumeration (tests/hdr_gate.rs) drives the SHIPPING policy.
pub use format_plan::{
    HdrPlan, HdrReconfigurePlan, hdr_live_upgrade_wants_f16, hdr_present_plan,
    hdr_reconfigure_plan, hdr_swapchain_wants_f16,
};
mod renderer;
pub use renderer::{
    DropOverlay, GpuRenderer, GpuSurface, PresentCrop, SurfacePresentFailure, TrayQuad, WindowGpu,
};
/// VIDEO introspection: submitted-destination-frame capture (see `video_tap`).
pub mod video_tap;
/// COLD-BUILD sub-phase probe: the ns split of the one startup phase the
/// frontend ledger could only see as a `join()` (see `startup_probe`).
pub mod startup_probe;
// TRUST_NATIVE_TLA Phase 2: the GPU-free slice-precondition decision (the real
// `GpuEncode.tla` `NeverSliceEmpty` gate `InstanceBuf::upload` uses), re-exported so
// the Tier-1 conformance test can drive the genuine decision headlessly.
pub use renderer::should_slice;

// H1 (Windows Mica/Acrylic): the DirectComposition VISUAL swapchain opt-in.
//
// The default DX12 surface is an HWND swapchain (`CreateSwapChainForHwnd`) whose
// ONLY composite mode is Opaque — DWM's Mica/Acrylic backdrop
// (`DWMWA_SYSTEMBACKDROP_TYPE`) is painted strictly BEHIND the window's visual
// tree, so an opaque client area covers it completely and `background_material`
// could never reach the client pixels. wgpu 29 ships the alternative wholesale:
// `Dx12SwapchainKind::DxgiFromVisual` builds an `IDCompositionVisual` +
// `CreateSwapChainForComposition` swapchain itself, whose caps DO advertise the
// non-opaque composite modes — a frame presented with per-pixel alpha then blends
// over the DWM backdrop.
//
// The presentation system is an INSTANCE-descriptor property (it decides how
// `create_surface` interprets a Win32 handle), and the instance is created before
// any window/config-reload can be consulted — so the opt-in is a process-global
// latch the frontend sets from config BEFORE building the [`GpuContext`].
// Rejected alternatives:
//   * `WGPU_DX12_PRESENTATION_SYSTEM=visual` (wgpu's own env knob): works, but
//     mutating our own environment as an IPC channel to a library is exactly the
//     kind of spooky action the explicit `InstanceDescriptor` field exists to
//     replace — and the env would leak to child shells the terminal spawns.
//   * Always-visual: the HWND swapchain is the RenderDoc-debuggable, most-mature
//     path and the shipped default is `background_material = "none"`; the visual
//     path engages ONLY on explicit config, keeping the default byte-identical.
//
// FAIL-SOFT ("DirectComposition unavailable"). On a visual instance
// `create_surface` only records the HWND: the DComp device / target / visual and
// the composition swapchain are all built LAZILY by the FIRST `Surface::configure`
// (wgpu-hal dx12 `DCompositionCreateDevice2` → `CreateTargetForHwnd` →
// `CreateVisual` → `CreateSwapChainForComposition` → `SetContent` → `Commit`),
// and wgpu's `Surface::configure` returns no `Result` — a hal failure becomes
// `ConfigureSurfaceError::InvalidSurface`, delivered to the DEVICE's error sink,
// whose default handler PANICS (aterm installs no uncaptured-error handler, by
// design — see `GpuRenderer::clamp_fb_dim`). Left alone, "DComp unavailable"
// would therefore be a process abort at the first window attach, not a soft
// error, and the `create_window_surface` Err ⇒ CPU arm would never see it. So
// `GpuRenderer::create_window_surface` runs the visual path's first configure
// inside wgpu error scopes and returns `Err`; the frontend's attach arm then
// consults [`surface_attach_fallback`], withdraws this latch, rebuilds the GPU
// stack on the plain HWND swapchain (`GpuRenderer::rebuild_on_fresh_context` —
// the presentation system is instance-level, so a new instance it is), and
// retries the attach once. The material then styles the caption only, with one
// diagnostic; the default (non-visual) configure is never scoped.
#[cfg(windows)]
static DX12_VISUAL_SWAPCHAIN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Whether a made request was later WITHDRAWN this run (GPU init failed, the
/// first composition-swapchain configure failed, or the device was lost), so a
/// later "caption only" diagnostic can say so instead of suggesting a restart
/// that would only repeat the failure.
#[cfg(windows)]
static DX12_VISUAL_SWAPCHAIN_WITHDRAWN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// What [`GpuContext::new`] actually BUILT (requested AND the DX12 backend won
/// adapter selection), distinct from the request so introspection never reports
/// an engaged visual path on a `ATERM_GPU_BACKEND=vulkan` escape-hatch run.
#[cfg(windows)]
static DX12_VISUAL_SWAPCHAIN_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Request that the NEXT [`GpuContext::new`] build its DX12 instance on the
/// DirectComposition VISUAL swapchain path (see the latch comment above). Must be
/// called before the context exists; a GPU-loss recovery rebuild re-reads the
/// latch, so the choice survives device loss. Windows-only by construction —
/// every other backend ignores the DX12 options.
#[cfg(windows)]
pub fn request_dx12_visual_swapchain() {
    DX12_VISUAL_SWAPCHAIN_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Withdraw the visual-swapchain request. Three frontend arms call this:
///   * the GPU-init-failure arm — windows created after a failed GPU init go
///     back to plain HWND attributes (a softbuffer present into a
///     `WS_EX_NOREDIRECTIONBITMAP` window has no redirection surface to blit
///     into and would show nothing);
///   * the GPU-loss downgrade (same softbuffer reasoning for later windows);
///   * the H1 fail-soft arm — the first composition-swapchain configure failed
///     (see the latch comment above), so the GPU stack is rebuilt on the HWND
///     path: [`GpuContext::new`] re-reads the latch, which must be `false` by
///     then.
///
/// Records the withdrawal ([`dx12_visual_swapchain_withdrawn`]) when a request
/// was actually standing; a withdraw with no request outstanding is a no-op.
#[cfg(windows)]
pub fn withdraw_dx12_visual_swapchain() {
    if DX12_VISUAL_SWAPCHAIN_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed) {
        DX12_VISUAL_SWAPCHAIN_WITHDRAWN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Whether a visual-swapchain request was made and then withdrawn this run
/// (see [`withdraw_dx12_visual_swapchain`]). Diagnostics only: the "material
/// styles the caption only" once-warning uses it to name the real cause rather
/// than advise a restart.
#[cfg(windows)]
pub fn dx12_visual_swapchain_withdrawn() -> bool {
    DX12_VISUAL_SWAPCHAIN_WITHDRAWN.load(std::sync::atomic::Ordering::Relaxed)
}

/// H1 fail-soft: what the frontend does when a window's GPU swapchain attach
/// ([`GpuRenderer::create_window_surface`]) returns `Err`. The decision is pure
/// and platform-independent so the latch-withdrawal policy is unit-tested on
/// every CI runner, even though only Windows can build a visual instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceAttachFallback {
    /// Another window already presents on this GPU instance, so the instance
    /// itself works (on a visual instance, its composition swapchain has
    /// already succeeded once): decline THIS window only. Downgrading or
    /// rebuilding the shared backend would orphan the live surfaces.
    DeclineWindow,
    /// First/only window on a DirectComposition visual instance: the failure is
    /// the lazily built composition stack (or something the opaque path does
    /// not need) — withdraw the visual latch, rebuild the GPU stack on the
    /// opaque HWND swapchain, and retry the attach ONCE. The window's
    /// `WS_EX_NOREDIRECTIONBITMAP` is harmless to a flip-model HWND swapchain.
    RebuildOpaque,
    /// First/only window on a plain instance (or the opaque retry failed too):
    /// downgrade the whole app to the CPU softbuffer renderer — the pre-H1 arm.
    CpuRenderer,
}

/// The [`SurfaceAttachFallback`] for a failed attach, given whether the live
/// context was built on the visual path (`visual_swapchain_active`) and whether
/// any other window already presents on it (`other_gpu_windows`). Total over
/// both inputs; `RebuildOpaque` is chosen iff the instance is visual AND no
/// other GPU window exists — the one situation where "DComp unavailable" is
/// the plausible cause and a rebuild orphans nothing.
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

/// Whether the visual swapchain has been REQUESTED (config asked for a client-area
/// backdrop and the frontend latched it). Window creation keys
/// `with_no_redirection_bitmap` off this — the request is made before any window
/// exists, while [`dx12_visual_swapchain_active`] is only known after the (possibly
/// concurrent) GPU init finishes.
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

/// wgpu device + queue, plus what we learned about the adapter.
///
/// The `instance` and `adapter` are KEPT (not dropped after device creation) so a
/// window surface can be created on the SAME instance/adapter later — the GPU
/// application-present path (`GpuRenderer::create_window_surface`) blits the
/// offscreen frame straight into a swapchain instead of reading it back to CPU.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter_name: String,
    pub backend: String,
    /// Whether the device supports `DownlevelFlags::VIEW_FORMATS` — creating a
    /// texture view with a different (sRGB) format than its base. The linear-light
    /// compositing path aliases the `Rgba8Unorm` offscreen with an `Rgba8UnormSrgb`
    /// VIEW, which REQUIRES this. wgpu's GLES/WebGL2 backend (orc's renderer) does
    /// NOT support it — GL textures have an immutable format — so on those devices
    /// we fall back to compositing on the plain Unorm view (no crash; the prior,
    /// non-linear blend). Native (Metal/Vulkan/DX) keeps the linear-light path.
    pub(crate) srgb_offscreen: bool,
    /// Kept alive so window surfaces can be created from this instance, and so
    /// `surface.get_capabilities(&adapter)` can be queried at surface setup.
    pub(crate) instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    /// Set once by wgpu's device-lost callback (registered at device creation) when
    /// the underlying GPU device is removed — a Windows NVIDIA/AMD driver
    /// install/update, a TDR (2s GPU hang) reset, an eGPU unplug, or an explicit
    /// `device.destroy()`. wgpu otherwise routes the lost-device condition to this
    /// callback and DROPS it if unregistered, so every subsequent frame silently
    /// `get_current_texture() -> Lost`s and the window freezes at its last frame
    /// forever. The frontend polls [`Self::device_lost`] after a present and, when
    /// set, rebuilds the whole GPU stack (new instance/adapter/device + per-window
    /// surfaces) or downgrades to the CPU softbuffer backend, instead of freezing.
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// H1 (Windows Mica/Acrylic): whether THIS instance was built on the DX12
    /// DirectComposition VISUAL swapchain path (`Dx12SwapchainKind::DxgiFromVisual`)
    /// — i.e. the request latch was set at build time AND the DX12 backend won
    /// adapter selection. Window surfaces created from a visual instance offer the
    /// non-opaque composite modes; the renderer keys its backdrop-margin alpha and
    /// the PreMultiplied present off this, never off the request alone. Always
    /// `false` off Windows / on wasm / on the Vulkan escape hatch.
    pub(crate) visual_swapchain: bool,
}

/// Row alignment required by `copy_texture_to_buffer`.
const ALIGN: usize = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;

fn padded_bytes_per_row(width: usize) -> usize {
    let unpadded = width * 4;
    unpadded.div_ceil(ALIGN) * ALIGN
}

/// Map an `ATERM_GPU_BACKEND` value to a wgpu backend mask.
/// `None` = unrecognized (caller warns and keeps the default).
#[cfg(not(target_arch = "wasm32"))]
fn parse_gpu_backend(v: &str) -> Option<wgpu::Backends> {
    match v.to_ascii_lowercase().as_str() {
        "dx12" => Some(wgpu::Backends::DX12),
        "vulkan" => Some(wgpu::Backends::VULKAN),
        "gl" => Some(wgpu::Backends::GL),
        "metal" => Some(wgpu::Backends::METAL),
        _ => None,
    }
}

/// Map an `ATERM_GPU_POWER` value to a power preference.
/// `None` = unrecognized (caller warns and keeps the `LowPower` default).
fn parse_gpu_power(v: &str) -> Option<wgpu::PowerPreference> {
    match v.to_ascii_lowercase().as_str() {
        "low" => Some(wgpu::PowerPreference::LowPower),
        "high" => Some(wgpu::PowerPreference::HighPerformance),
        _ => None,
    }
}

/// GPU selection environment overrides (each parsed by the helper right below):
///
/// * `ATERM_GPU_BACKEND=dx12|vulkan|gl|metal` — restrict the wgpu instance to one
///   backend (default: DX12 on Windows — the native API + HDR/scRGB present path;
///   `Backends::PRIMARY` elsewhere). Native only; the wasm build's instance is
///   created by its own init path. Escape hatch for a broken driver path (e.g.
///   `ATERM_GPU_BACKEND=vulkan` if a DX12 driver misbehaves) without a rebuild.
/// * `ATERM_GPU_ADAPTER=<substring>` — pick the adapter whose reported name
///   contains this string, case-insensitive (e.g. `intel`, `nvidia`). Falls back
///   to default selection (with a stderr warning) when nothing matches.
/// * `ATERM_GPU_POWER=low|high` — adapter power preference; defaults to `low`.
///   A terminal's per-frame GPU cost is trivial, and LowPower keeps hybrid
///   laptops on the iGPU (a dGPU-only machine still picks its dGPU); `high`
///   restores the always-discrete behavior.
#[cfg(not(target_arch = "wasm32"))]
fn backends_from_env() -> wgpu::Backends {
    // Default backend: DX12 on Windows, `PRIMARY` elsewhere. DX12 is the native
    // Windows GPU API + the ONLY backend wired for the HDR/scRGB EDR present
    // (`WindowGpu::tag_swapchain_scrgb` reaches the DX12 `IDXGISwapChain3`; on the
    // Vulkan backend the cursor aurora silently falls back to an SDR swapchain).
    // DX12 is a Windows 10+ baseline so restricting is safe; `ATERM_GPU_BACKEND=vulkan`
    // reverts for a broken-driver escape hatch.
    #[cfg(windows)]
    let default = wgpu::Backends::DX12;
    #[cfg(not(windows))]
    let default = wgpu::Backends::PRIMARY;
    match std::env::var("ATERM_GPU_BACKEND") {
        Ok(v) if !v.is_empty() => parse_gpu_backend(&v).unwrap_or_else(|| {
            eprintln!(
                "aterm-gpu: unknown ATERM_GPU_BACKEND {v:?} (want dx12|vulkan|gl|metal); using default"
            );
            default
        }),
        _ => default,
    }
}

/// `ATERM_GPU_POWER` (see [`backends_from_env`] for the env-var table).
fn power_preference_from_env() -> wgpu::PowerPreference {
    match std::env::var("ATERM_GPU_POWER") {
        Ok(v) if !v.is_empty() => parse_gpu_power(&v).unwrap_or_else(|| {
            eprintln!("aterm-gpu: unknown ATERM_GPU_POWER {v:?} (want low|high); using low");
            wgpu::PowerPreference::LowPower
        }),
        _ => wgpu::PowerPreference::LowPower,
    }
}

/// `ATERM_GPU_ADAPTER` (see [`backends_from_env`] for the env-var table).
/// Adapter enumeration is a native-only wgpu API; the wasm build has exactly one
/// adapter anyway, so the override is a no-op there.
#[cfg(not(target_arch = "wasm32"))]
async fn adapter_from_env(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    let want = std::env::var("ATERM_GPU_ADAPTER")
        .ok()
        .filter(|v| !v.is_empty())?;
    let want_lc = want.to_ascii_lowercase();
    let found = instance
        .enumerate_adapters(wgpu::Backends::all())
        .await
        .into_iter()
        .find(|a| a.get_info().name.to_ascii_lowercase().contains(&want_lc));
    if found.is_none() {
        eprintln!(
            "aterm-gpu: ATERM_GPU_ADAPTER={want:?} matched no adapter; using default selection"
        );
    }
    found
}

// Adapter enumeration is native-only; the wasm build has exactly one adapter.
#[cfg(target_arch = "wasm32")]
async fn adapter_from_env(_instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    None
}

impl GpuContext {
    /// Acquire a GPU. Works headless (no window/surface needed) — picks the
    /// default low-power adapter (Metal on macOS), subject to the
    /// `ATERM_GPU_*` env overrides (see [`backends_from_env`]).
    ///
    /// NATIVE ONLY: this uses `pollster::block_on`, which has no browser
    /// equivalent (blocking the wasm main thread is forbidden). The wasm WebGPU
    /// path awaits the adapter/device futures instead — see [`GpuContext::request`]
    /// — so this synchronous constructor is excluded from the wasm32 build.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new() -> Result<Self, String> {
        // This instance must OUTLIVE device creation: it is kept on `GpuContext`
        // so a window surface can be created from it for the application-present
        // path. `new_without_display_handle()` (no `OwnedDisplayHandle`) is still
        // surface-capable on Metal — the platform doesn't use the display handle
        // (it's only required for GLES/Wayland presentation), so the headless
        // adapter request below can keep `compatible_surface: None`.
        #[cfg_attr(not(windows), allow(unused_mut))] // only the DX12 latch below mutates
        let mut desc = wgpu::InstanceDescriptor {
            backends: backends_from_env(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        };
        // H1 (Windows Mica/Acrylic): honour the visual-swapchain latch via the
        // EXPLICIT descriptor field (not wgpu's `WGPU_DX12_PRESENTATION_SYSTEM`
        // env knob — see the latch comment at the top of this file). Ignored by
        // every non-DX12 backend, so the `ATERM_GPU_BACKEND=vulkan` escape hatch
        // still works; `visual_swapchain` below records the ground truth.
        #[cfg(windows)]
        if dx12_visual_swapchain_requested() {
            desc.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
        }
        // The first timed leg of the cold build: backend enumeration + driver
        // load. Everything from here to `from_parts` returning is inside the
        // frontend's single `backend_finalize` number.
        let instance = startup_probe::timed(startup_probe::Leg::GpuInstance, || {
            wgpu::Instance::new(desc)
        });
        #[allow(unused_mut, reason = "mutated only on the Windows visual-swapchain arm")]
        let mut ctx = pollster::block_on(Self::from_instance(instance))?;
        // The visual path is ACTIVE only if the DX12 backend actually won adapter
        // selection (the descriptor option is inert elsewhere). Recorded on the
        // context (renderer decisions) AND the process-global (introspection).
        #[cfg(windows)]
        {
            ctx.visual_swapchain = dx12_visual_swapchain_requested() && ctx.backend == "Dx12";
            DX12_VISUAL_SWAPCHAIN_ACTIVE
                .store(ctx.visual_swapchain, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(ctx)
    }

    /// ASYNC adapter+device acquisition on an existing `wgpu::Instance`. Shared by
    /// the native (`pollster::block_on`) and wasm (`.await`) init paths so both
    /// hit the SAME adapter/device descriptors. The instance is moved in and kept
    /// alive on the returned `GpuContext` (surfaces are created from it later).
    ///
    /// No compatible surface — the headless/WebGPU path, where the adapter is
    /// surface-independent. The WebGL backend instead needs the canvas surface as
    /// the adapter's compatibility target; see
    /// [`from_instance_with_surface`](Self::from_instance_with_surface).
    pub async fn from_instance(instance: wgpu::Instance) -> Result<Self, String> {
        Self::from_instance_with_surface(instance, None).await
    }

    /// ASYNC adapter+device acquisition, with an OPTIONAL `compatible_surface`.
    ///
    /// The WebGL backend (wgpu's `webgl` feature on wasm32) can only enumerate an
    /// adapter that is compatible with a presentation surface — the GL context
    /// lives ON the `<canvas>`. So the wasm WebGL init creates the canvas surface
    /// first and passes it here; native (Metal/Vulkan) passes `None`, leaving the
    /// adapter/device descriptors byte-identical to the prior behavior.
    pub async fn from_instance_with_surface(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, String> {
        let adapter_started = web_time::Instant::now();
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
        let device_started = web_time::Instant::now();
        // WebGL2 has no compute + lower texture/buffer ceilings, so `Limits::
        // default()` is unsatisfiable there (e.g. max_compute_workgroups_per_
        // dimension default 65535 vs WebGL2's 0) and device request fails. On
        // wasm32 (the WebGL backend) request the ADAPTER's OWN supported limits:
        // that's always satisfiable AND keeps the real texture-dimension ceiling
        // the WebGL2 context reports (the canonical downlevel_webgl2_defaults caps
        // max_texture_dimension_2d at 2048, which is too small for a Hi-DPI grid
        // canvas — surface configure then fails). Native keeps the full defaults.
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
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| e.to_string())?;
        startup_probe::record(startup_probe::Leg::GpuDevice, device_started.elapsed());
        let context_tail_started = web_time::Instant::now();
        // sRGB texture-VIEW aliasing requires VIEW_FORMATS; absent on GLES/WebGL2.
        let srgb_offscreen = adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::VIEW_FORMATS);
        // Capture device loss: without a registered callback wgpu SILENTLY DROPS the
        // lost-device notification, so a driver update / TDR reset freezes every
        // window at its last frame with no error. Flip an atomic the frontend polls
        // (see the `device_lost` field) so it can rebuild the GPU stack or fall back
        // to the CPU renderer. The callback runs at most once per device.
        let device_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flag = std::sync::Arc::clone(&device_lost);
            device.set_device_lost_callback(move |reason, msg| {
                // `Destroyed` fires on an intentional teardown (our own drop); only a
                // genuine loss (`Unknown` / driver removal) needs recovery, but flag
                // both — the frontend re-checks liveness before it rebuilds, and a
                // stale flag on a context that is being dropped is harmless.
                eprintln!("aterm-gpu: GPU device lost ({reason:?}): {msg}");
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

    /// Whether the GPU device has been reported lost since this context was created
    /// (driver update, TDR reset, eGPU unplug, explicit destroy). Once `true` it
    /// stays `true` — a lost device is never revived in place; the frontend must
    /// rebuild the GPU stack or downgrade to the CPU backend. Cheap (one relaxed
    /// atomic load), so it is safe to poll every presented frame.
    #[inline]
    pub fn device_lost(&self) -> bool {
        self.device_lost.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Clamp a texture pixel dimension to `1..=max_texture_dimension_2d`.
    ///
    /// The `GpuContext`-side equivalent of [`GpuRenderer::clamp_fb_dim`] (this type
    /// holds the device directly). See that method for why the clamp exists (wgpu
    /// validates every texture against this limit; with no uncaptured-error handler a
    /// violation PANICS/aborts). Floors to 1 like `clamp_fb_dim`: a 0-dimension
    /// texture is ALWAYS invalid, so a public caller passing 0 (e.g.
    /// `clear_to_frame(0, h, ..)`) would otherwise reach `create_texture` with a zero
    /// extent and abort. Upper bound mirrors `Renderer::create_atlas_texture`'s
    /// own-site `.min(max_tex_dim)`.
    #[inline]
    fn clamp_tex_dim(&self, d: u32) -> u32 {
        d.max(1).min(self.device.limits().max_texture_dimension_2d)
    }

    /// Create an offscreen colour target (Rgba8Unorm; render + copy-src +
    /// texture-binding). `TEXTURE_BINDING` is additive — it lets the application
    /// present blit SAMPLE this exact texture into the swapchain destination, so
    /// submitted destination bytes match the introspection readback at the
    /// app-owned boundary. The platform compositor and scanout are unobserved.
    /// The parity tests (which build the atlas on the CPU, not this texture) are
    /// unaffected.
    pub fn offscreen_texture(&self, width: u32, height: u32) -> wgpu::Texture {
        // Defensive floor at this `create_texture` choke point: clamp to the device's
        // max 2D texture dimension (mirrors `Renderer::create_atlas_texture`). Without
        // it an oversized grid would ask wgpu for a texture past the limit, and with NO
        // `on_uncaptured_error` handler installed the validation error hits wgpu's
        // default handler, which PANICS/aborts. IMPORTANT: this clamp bounds ONLY the
        // texture — a caller that ALSO feeds the SAME dims to `read_back` /
        // copy_texture_to_buffer (e.g. `clear_to_frame`) must clamp its OWN copy up
        // front, else the copy extent would exceed this clamped texture and abort
        // anyway (which is exactly why `clear_to_frame` pre-clamps). `encode_frame`
        // pre-clamps too, so on both production paths this is idempotent
        // (`clamp_tex_dim(clamped) == clamped`); it still bounds any future caller.
        let width = self.clamp_tex_dim(width);
        let height = self.clamp_tex_dim(height);
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aterm-gpu offscreen"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Linear-light compositing needs the base passes to fixed-function-blend in
            // LINEAR light. With VIEW_FORMATS (native) we keep an Rgba8Unorm texture +
            // attach an sRGB-typed VIEW for that. WebGL2/GLES can't alias formats, so
            // there we make the texture ITSELF Rgba8UnormSrgb — the base passes attach
            // its default (sRGB) view and STILL blend in linear light (the format
            // auto-decodes/encodes). Either way the STORED bytes are sRGB, so readback
            // is identical. The blit re-encodes on the downlevel path (it samples the
            // sRGB view, which auto-decodes) — see BLIT_SHADER's encode_srgb.
            format: self.offscreen_format(),
            // COPY_DST is needed by the M1b sub-row scroll band shift, which copies
            // the shifted grid band back into the offscreen (via a scratch texture);
            // it also leaves every non-shift path byte-identical (a pure capability
            // add). RENDER_ATTACHMENT (draw target) + COPY_SRC (readback / bloom /
            // the shift's source copy) + TEXTURE_BINDING (blit source) as before.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: crate::format_plan::offscreen_view_formats(self.srgb_offscreen),
        })
    }

    /// The offscreen colour-target format (== `off.view` format). Delegates to the
    /// single source of truth in [`crate::format_plan`] so the texture, its views,
    /// and every pipeline target cannot drift apart (the C1/C2 bug class). Every
    /// render pipeline whose pass attaches `off.view` (additive glow/deco-add, bloom
    /// composite + extract, tray, test/readback blit) MUST build with this.
    pub(crate) fn offscreen_format(&self) -> wgpu::TextureFormat {
        crate::format_plan::offscreen_format(self.srgb_offscreen)
    }

    /// The sRGB-typed VIEW format attached by the base OVER/REPLACE + cursor +
    /// deco-over passes (linear-light blend). Pipelines whose pass attaches
    /// `off.view_srgb` build with this. See [`crate::format_plan`].
    pub(crate) fn offscreen_srgb_view_format(&self) -> wgpu::TextureFormat {
        crate::format_plan::offscreen_srgb_view_format(self.srgb_offscreen)
    }

    /// Read an Rgba8Unorm texture back into an `aterm_render::Frame`
    /// (`0xTTRRGGBB` where the top byte is the CPU renderer's TRANSMITTANCE
    /// encoding, `255 - alpha`; every opaque texel — the historical case —
    /// yields `0x00RRGGBB` exactly as before), stripping GPU row padding.
    /// Folding the texture alpha into the CPU encoding lets the parity tests
    /// compare background-opacity alpha CPU==GPU like any other channel.
    ///
    /// Infallible wrapper over [`Self::try_read_back`]: a readback failure
    /// (device lost mid-frame — TDR, driver update) degrades to a black frame
    /// instead of panicking for historical renderer-oracle/probe callers. Durable
    /// artifact APIs must call the fallible path and report capture failure rather
    /// than publishing this best-effort sentinel as real pixels.
    pub fn read_back(&self, texture: &wgpu::Texture, width: u32, height: u32) -> Frame {
        self.try_read_back(texture, width, height)
            .unwrap_or_else(|e| {
                eprintln!("aterm-gpu: readback failed ({e}); returning blank frame");
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
        // Deref the BufferView ONCE. `wgpu::BufferView: Deref` runs
        // `self.inner.read_slice()` through `DispatchBufferMappedRange`, so the
        // old `data[p]`/`data[p+1]`/... form paid that chain (plus a bounds
        // check) FOUR TIMES PER PIXEL — ~24M redundant deref/bounds pairs for a
        // 3024x1964 readback, on the UI event loop behind the snapshot/image
        // verbs. Binding `&[u8]` once and walking row slices as `[u8; 4]` texels
        // gives the compiler a fixed-size, bounds-check-free window; the packing
        // expression below is byte-for-byte the old one.
        let bytes: &[u8] = &data;

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
        drop(data);
        buffer.unmap();
        Ok(Frame {
            width: w,
            height: h,
            pixels,
        })
    }

    /// Phase-1 proof of life: clear an offscreen target to a colour and read it
    /// back. Confirms the GPU pipeline + readback work on this machine.
    pub fn clear_to_frame(&self, width: u32, height: u32, rgb: u32) -> Frame {
        // Clamp ONCE up front so BOTH the offscreen texture AND the `read_back` copy
        // extent below use the same bounded dims. `offscreen_texture` clamps the
        // texture internally, but `read_back` (copy_texture_to_buffer) would otherwise
        // request a copy LARGER than the clamped source texture — a wgpu validation
        // error that, with no uncaptured-error handler installed, PANICS/aborts.
        let width = self.clamp_tex_dim(width);
        let height = self.clamp_tex_dim(height);
        let tex = self.offscreen_texture(width, height);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Decode to linear on the downlevel sRGB offscreen so the readback
                        // byte == `rgb` (the proof-of-life probe must not lie); raw on
                        // native (plain Unorm) — see format_plan::offscreen_clear_color.
                        load: wgpu::LoadOp::Clear(crate::format_plan::offscreen_clear_color(
                            rgb,
                            self.srgb_offscreen,
                        )),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        self.queue.submit([enc.finish()]);
        self.read_back(&tex, width, height)
    }
}

#[cfg(test)]
mod env_override_tests {
    use super::*;

    #[test]
    fn backend_parses_known_names_case_insensitive() {
        assert_eq!(parse_gpu_backend("DX12"), Some(wgpu::Backends::DX12));
        assert_eq!(parse_gpu_backend("vulkan"), Some(wgpu::Backends::VULKAN));
        assert_eq!(parse_gpu_backend("gl"), Some(wgpu::Backends::GL));
        assert_eq!(parse_gpu_backend("Metal"), Some(wgpu::Backends::METAL));
        assert_eq!(parse_gpu_backend("opengl"), None);
        assert_eq!(parse_gpu_backend(""), None);
    }

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
}

/// H1 fail-soft: the latch-withdrawal decision a failed window-surface attach
/// drives (`surface_attach_fallback`). Pure, so it runs on every platform.
#[cfg(test)]
mod surface_attach_fallback_tests {
    use super::{SurfaceAttachFallback as F, surface_attach_fallback};

    /// The review's scenario: `background_material` set, DX12 won, DComp is
    /// unavailable, first window — the ONLY case that withdraws the latch and
    /// rebuilds on the opaque HWND swapchain (instead of aborting the process).
    #[test]
    fn visual_first_window_rebuilds_on_the_opaque_swapchain() {
        assert_eq!(surface_attach_fallback(true, false), F::RebuildOpaque);
    }

    /// A later window failing on a WORKING visual instance is not a DComp
    /// outage (the first window's composition swapchain succeeded): keep the
    /// pre-H1 hard rollback for that one window, never rebuild under the
    /// survivors.
    #[test]
    fn visual_instance_with_live_gpu_windows_declines_only_this_window() {
        assert_eq!(surface_attach_fallback(true, true), F::DeclineWindow);
    }

    /// The shipped default (`background_material = "none"`, HWND swapchain)
    /// keeps its byte-identical arms: decline when others present, else the
    /// CPU softbuffer downgrade. No latch to withdraw, no rebuild.
    #[test]
    fn plain_instance_keeps_the_pre_h1_arms() {
        assert_eq!(surface_attach_fallback(false, true), F::DeclineWindow);
        assert_eq!(surface_attach_fallback(false, false), F::CpuRenderer);
    }

    /// Totality + the one-liner invariant: `RebuildOpaque` iff visual AND no
    /// other GPU window; `other_gpu_windows` dominates regardless of the path.
    #[test]
    fn rebuild_iff_visual_and_alone() {
        for visual in [false, true] {
            for others in [false, true] {
                let plan = surface_attach_fallback(visual, others);
                assert_eq!(
                    plan == F::RebuildOpaque,
                    visual && !others,
                    "visual={visual} others={others} -> {plan:?}"
                );
                assert_eq!(plan == F::DeclineWindow, others, "others must dominate");
            }
        }
    }
}
