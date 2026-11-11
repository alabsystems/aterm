// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D3D11 arm's swap chains (docs/AUDIT-third-party-2026-10-15.md GW.5): a
//! flip-model swap chain on a window, or on a DirectComposition visual the
//! window shows, with the frame-latency waitable, the scRGB tag of an f16
//! chain, resize and the present.
//!
//! * [`D3dSwapKind::Hwnd`]: `CreateSwapChainForHwnd`, opaque (`ALPHA_MODE_IGNORE`),
//!   and Alt+Enter left to aterm (`MakeWindowAssociation`), as wgpu-dx12 makes
//!   a window's swap chain.
//! * [`D3dSwapKind::Composition`]: `CreateSwapChainForComposition` with
//!   premultiplied alpha, set as the content of one visual that is the root of
//!   a DirectComposition target on the window. Its pixels then blend over
//!   DWM's Mica or Acrylic backdrop, which an HWND swap chain's opaque
//!   composite covers.
//!
//! Both are `FLIP_DISCARD` with `FRAME_LATENCY_WAITABLE_OBJECT`: `latency + 1`
//! buffers, `SetMaximumFrameLatency(latency)`, and a waitable that is signalled
//! when the next frame may start without queueing behind more than `latency`
//! presented frames ([`D3dWindowSurface::wait_for_frame`]). The waitable counts
//! frames: each wait it answers takes one, and only a present gives one back
//! when the compositor retires it, so the surface holds a frame it waited for
//! until a present spends it. Their buffers are
//! `R8G8B8A8_UNORM` (the blit writes the offscreen's sRGB-encoded bytes raw,
//! and the taps read them back as the virtual target's), or
//! `R16G16B16A16_FLOAT` tagged scRGB (`SetColorSpace1(G10_NONE_P709)`) for the
//! EDR crown, which [`D3dWindowSurface::create`] refuses by name where the
//! output cannot present it, so the caller falls back to 8-bit.
//!
//! A present takes the device's context (the runtime flushes it), and a
//! removal code from `Present` or `ResizeBuffers` latches the device lost like
//! any checked call; the injected-loss drill answers the present's `enter`.
//! A drop clears what the context binds and flushes it: a frame that never
//! presented leaves buffer 0 bound, and a released chain lives on until the
//! context lets it go, its window refusing another chain meanwhile.

use std::mem::ManuallyDrop;

use aterm_win32::Win32::Foundation::{
    CloseHandle, DXGI_STATUS_OCCLUDED, HANDLE, HWND, WAIT_OBJECT_0,
};
use aterm_win32::Win32::System::Threading::WaitForSingleObjectEx;
use aterm_win32::com::Com;
use aterm_win32::{d3d11 as d3d, dcomp, dxgi};

use super::device::D3dDevice;
use super::resources::D3dTexture;
use crate::device_layer::TexelFormat;

/// What a swap chain is made on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum D3dSwapKind {
    /// The window itself, opaque.
    Hwnd,
    /// A DirectComposition visual on the window, premultiplied over its
    /// backdrop.
    Composition,
}

/// How a present ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum D3dPresented {
    /// The frame is queued for the screen.
    Shown,
    /// The window cannot be seen (`DXGI_STATUS_OCCLUDED`); nothing was shown.
    Occluded,
}

/// The DirectComposition objects a composition swap chain is shown by, held
/// for the surface's life.
struct DcompTree {
    _device: Com,
    _target: Com,
    _visual: Com,
}

/// One window's swap chain on the arm's device.
pub(crate) struct D3dWindowSurface {
    dev: D3dDevice,
    /// The `IDXGISwapChain3`, released by `drop` before it forces the
    /// chain's destruction.
    swap: ManuallyDrop<Com>,
    /// The frame-latency waitable, closed on drop.
    waitable: HANDLE,
    /// A composition chain's tree, held for the surface's life.
    _tree: Option<DcompTree>,
    kind: D3dSwapKind,
    format: TexelFormat,
    flags: u32,
    width: u32,
    height: u32,
    /// Buffer 0 with its views while a frame draws into it; dropped before a
    /// resize, which needs every reference to the buffers released.
    back: Option<D3dTexture>,
    /// Whether a wait took a frame from the waitable that no present has
    /// spent: a frame whose draw failed after its wait. The next wait reuses
    /// it, since asking the waitable again would hold one frame more than a
    /// present gives back, and at latency 1 time out every frame after.
    frame_held: bool,
}

impl std::fmt::Debug for D3dWindowSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dWindowSurface")
            .field("kind", &self.kind)
            .field("format", &self.format)
            .field("size", &(self.width, self.height))
            .finish_non_exhaustive()
    }
}

/// The DXGI spelling of a swap chain's buffer format.
const fn swap_format(format: TexelFormat) -> Option<dxgi::DXGI_FORMAT> {
    match format {
        TexelFormat::Rgba8Unorm => Some(dxgi::DXGI_FORMAT_R8G8B8A8_UNORM),
        TexelFormat::Rgba16Float => Some(dxgi::DXGI_FORMAT_R16G16B16A16_FLOAT),
        _ => None,
    }
}

impl D3dWindowSurface {
    /// A swap chain of `kind` on `hwnd` at `width` x `height`, its buffers
    /// `format` (`Rgba8Unorm`, or `Rgba16Float`, which the caller tags with
    /// [`Self::tag_scrgb_on_hdr_output`] before it presents), `latency`
    /// frames deep (at least 1).
    ///
    /// # Safety
    ///
    /// `hwnd` is a live window of this thread, which outlives the surface.
    pub(crate) unsafe fn create(
        dev: &D3dDevice,
        hwnd: HWND,
        kind: D3dSwapKind,
        (width, height): (u32, u32),
        format: TexelFormat,
        latency: u32,
    ) -> Result<Self, String> {
        let max = dev.caps().max_texture_dim;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(format!(
                "d3d11: a {width}x{height} swap chain is outside 1..={max} per side"
            ));
        }
        let swap_format =
            swap_format(format).ok_or_else(|| format!("d3d11: no {format:?} swap chain"))?;
        let latency = latency.clamp(1, 15);
        let flags = dxgi::DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT as u32;
        let desc = dxgi::DXGI_SWAP_CHAIN_DESC1 {
            Width: width,
            Height: height,
            Format: swap_format,
            Stereo: 0,
            SampleDesc: dxgi::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: dxgi::DXGI_USAGE_RENDER_TARGET_OUTPUT | dxgi::DXGI_USAGE_SHADER_INPUT,
            BufferCount: latency + 1,
            Scaling: dxgi::DXGI_SCALING_STRETCH,
            SwapEffect: dxgi::DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: match kind {
                D3dSwapKind::Hwnd => dxgi::DXGI_ALPHA_MODE_IGNORE,
                D3dSwapKind::Composition => dxgi::DXGI_ALPHA_MODE_PREMULTIPLIED,
            },
            Flags: flags,
        };
        let shared = dev.shared();
        let factory = shared.factory();
        // SAFETY: `factory` is an IDXGIFactory2.
        let fvt = unsafe { factory.vtable::<dxgi::IDXGIFactory2_Vtbl>() };
        let mut out = std::ptr::null_mut();
        // SAFETY: the device is this factory's adapter's; `hwnd` is live (the
        // caller's contract); a valid flip-model description; no fullscreen
        // description or output restriction; one owned IDXGISwapChain1 is
        // written on success.
        let hr = unsafe {
            match kind {
                D3dSwapKind::Hwnd => (fvt.CreateSwapChainForHwnd)(
                    factory.as_raw(),
                    shared.device().as_raw(),
                    hwnd,
                    &desc,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    &mut out,
                ),
                D3dSwapKind::Composition => (fvt.CreateSwapChainForComposition)(
                    factory.as_raw(),
                    shared.device().as_raw(),
                    &desc,
                    std::ptr::null_mut(),
                    &mut out,
                ),
            }
        };
        let what = match kind {
            D3dSwapKind::Hwnd => "CreateSwapChainForHwnd",
            D3dSwapKind::Composition => "CreateSwapChainForComposition",
        };
        shared.ok(what, hr)?;
        // SAFETY: a successful creation hands over one reference.
        let swap1 =
            unsafe { Com::from_raw(out) }.ok_or_else(|| format!("d3d11: {what} returned null"))?;
        let swap = swap1
            .query(&dxgi::IID_IDXGISwapChain3)
            .map_err(|hr| shared.failure("IDXGISwapChain3", hr))?;
        // SAFETY: `swap` is an IDXGISwapChain3.
        let svt = unsafe { swap.vtable::<dxgi::IDXGISwapChain3_Vtbl>() };
        if kind == D3dSwapKind::Hwnd {
            // SAFETY: as above; `hwnd` is the swap chain's window. aterm
            // handles Alt+Enter itself.
            let hr = unsafe {
                (fvt.MakeWindowAssociation)(factory.as_raw(), hwnd, dxgi::DXGI_MWA_NO_ALT_ENTER)
            };
            shared.ok("MakeWindowAssociation", hr)?;
        }
        // SAFETY: a waitable swap chain takes a latency in 1..=16.
        let hr = unsafe { (svt.SetMaximumFrameLatency)(swap.as_raw(), latency.min(16)) };
        shared.ok("SetMaximumFrameLatency", hr)?;
        // SAFETY: a waitable swap chain hands out its waitable, which the
        // caller closes.
        let waitable = unsafe { (svt.GetFrameLatencyWaitableObject)(swap.as_raw()) };
        if waitable.is_null() {
            return Err("d3d11: the swap chain has no frame-latency waitable".to_owned());
        }
        let mut surface = Self {
            dev: dev.clone(),
            swap: ManuallyDrop::new(swap),
            waitable,
            _tree: None,
            kind,
            format,
            flags,
            width,
            height,
            back: None,
            frame_held: false,
        };
        if kind == D3dSwapKind::Composition {
            // SAFETY: `hwnd` is live (the caller's contract).
            surface._tree = Some(unsafe { surface.show_on(hwnd) }?);
        }
        Ok(surface)
    }

    /// Whether the output holding the window shows the HDR10 desktop colour
    /// space (Windows HDR on): the one test that an scRGB chain will be shown
    /// as such, since `CheckColorSpaceSupport` can report support while the
    /// desktop is SDR. `false` when the chain has no containing output.
    pub(crate) fn output_hdr(&self) -> bool {
        let mut out = std::ptr::null_mut();
        // SAFETY: `swap` is an IDXGISwapChain3; one owned IDXGIOutput is
        // written on success, which `Com` releases.
        let hr = unsafe {
            (self
                .swap
                .vtable::<dxgi::IDXGISwapChain3_Vtbl>()
                .GetContainingOutput)(self.swap.as_raw(), &mut out)
        };
        // SAFETY: as above.
        let Some(output) = (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten() else {
            return false;
        };
        let Ok(output) = output.query(&dxgi::IID_IDXGIOutput6) else {
            return false;
        };
        let mut desc = dxgi::DXGI_OUTPUT_DESC1::default();
        // SAFETY: `output` is an IDXGIOutput6; GetDesc1 fills `desc`.
        let hr = unsafe {
            (output.vtable::<dxgi::IDXGIOutput6_Vtbl>().GetDesc1)(output.as_raw(), &mut desc)
        };
        hr >= 0 && desc.ColorSpace == dxgi::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
    }

    /// Tag this f16 chain scRGB where its output is in HDR mode
    /// ([`Self::output_hdr`]); refused by a name that says scRGB otherwise,
    /// and the caller makes an 8-bit chain instead.
    pub(crate) fn tag_scrgb_on_hdr_output(&self) -> Result<(), String> {
        if !self.output_hdr() {
            return Err(
                "d3d11: the window's output is not in HDR mode, so no scRGB (Windows HDR off?)"
                    .to_owned(),
            );
        }
        self.tag_scrgb()
    }

    /// Tag an f16 chain scRGB (linear, Rec. 709 primaries), refused by name
    /// where the chain cannot present it.
    fn tag_scrgb(&self) -> Result<(), String> {
        let shared = self.dev.shared();
        // SAFETY: `swap` is an IDXGISwapChain3.
        let svt = unsafe { self.swap.vtable::<dxgi::IDXGISwapChain3_Vtbl>() };
        let space = dxgi::DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709;
        let mut support = 0u32;
        // SAFETY: as above; `support` receives the flags.
        let hr = unsafe { (svt.CheckColorSpaceSupport)(self.swap.as_raw(), space, &mut support) };
        shared.ok("CheckColorSpaceSupport", hr)?;
        if support & dxgi::DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT as u32 == 0 {
            return Err("d3d11: the output cannot present scRGB (Windows HDR off?)".to_owned());
        }
        // SAFETY: as above, with a colour space the chain supports.
        let hr = unsafe { (svt.SetColorSpace1)(self.swap.as_raw(), space) };
        shared.ok("SetColorSpace1", hr)
    }

    /// The DirectComposition tree that shows this composition chain on
    /// `hwnd`: a device on the D3D11 device's DXGI device, a topmost target on
    /// the window, one visual with the chain as its content, committed.
    ///
    /// # Safety
    ///
    /// `hwnd` is a live window of this thread.
    unsafe fn show_on(&self, hwnd: HWND) -> Result<DcompTree, String> {
        let shared = self.dev.shared();
        let dxgi_device = shared
            .device()
            .query(&dxgi::IID_IDXGIDevice)
            .map_err(|hr| shared.failure("IDXGIDevice", hr))?;
        let mut out = std::ptr::null_mut();
        // SAFETY: an IDXGIDevice; one owned IDCompositionDevice on success.
        let hr = unsafe {
            dcomp::DCompositionCreateDevice(
                dxgi_device.as_raw(),
                &dcomp::IID_IDCompositionDevice,
                &mut out,
            )
        };
        shared.ok("DCompositionCreateDevice", hr)?;
        // SAFETY: as above.
        let device =
            unsafe { Com::from_raw(out) }.ok_or("d3d11: DCompositionCreateDevice returned null")?;
        // SAFETY: `device` is an IDCompositionDevice.
        let dvt = unsafe { device.vtable::<dcomp::IDCompositionDevice_Vtbl>() };
        let mut out = std::ptr::null_mut();
        // SAFETY: `hwnd` is live (the caller's contract); topmost, over the
        // window's own content; one owned target on success.
        let hr = unsafe { (dvt.CreateTargetForHwnd)(device.as_raw(), hwnd, 1, &mut out) };
        shared.ok("IDCompositionDevice::CreateTargetForHwnd", hr)?;
        // SAFETY: as above.
        let target =
            unsafe { Com::from_raw(out) }.ok_or("d3d11: CreateTargetForHwnd returned null")?;
        let mut out = std::ptr::null_mut();
        // SAFETY: one owned visual on success.
        let hr = unsafe { (dvt.CreateVisual)(device.as_raw(), &mut out) };
        shared.ok("IDCompositionDevice::CreateVisual", hr)?;
        // SAFETY: as above.
        let visual = unsafe { Com::from_raw(out) }.ok_or("d3d11: CreateVisual returned null")?;
        // SAFETY: `visual` is an IDCompositionVisual and the swap chain a
        // composition chain of the same adapter.
        let hr = unsafe {
            (visual
                .vtable::<dcomp::IDCompositionVisual_Vtbl>()
                .SetContent)(visual.as_raw(), self.swap.as_raw())
        };
        shared.ok("IDCompositionVisual::SetContent", hr)?;
        // SAFETY: `target` is an IDCompositionTarget; the visual is its root.
        let hr = unsafe {
            (target.vtable::<dcomp::IDCompositionTarget_Vtbl>().SetRoot)(
                target.as_raw(),
                visual.as_raw(),
            )
        };
        shared.ok("IDCompositionTarget::SetRoot", hr)?;
        // SAFETY: as above.
        let hr = unsafe { (dvt.Commit)(device.as_raw()) };
        shared.ok("IDCompositionDevice::Commit", hr)?;
        Ok(DcompTree {
            _device: device,
            _target: target,
            _visual: visual,
        })
    }

    /// What the chain is made on.
    #[cfg(test)]
    #[inline]
    pub(crate) const fn kind(&self) -> D3dSwapKind {
        self.kind
    }

    /// The buffers' format.
    #[inline]
    pub(crate) const fn format(&self) -> TexelFormat {
        self.format
    }

    /// The buffers' size.
    #[cfg(test)]
    #[inline]
    pub(crate) const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Wait at most `timeout_ms` for the chain to take another frame without
    /// queueing past its latency; `false` on the timeout. A frame an earlier
    /// wait took and no present spent is still held, and answers at once.
    pub(crate) fn wait_for_frame(&mut self, timeout_ms: u32) -> bool {
        if !self.frame_held {
            self.frame_held = self.wait_on_waitable(timeout_ms);
        }
        self.frame_held
    }

    /// One wait on the waitable itself, past the held frame.
    fn wait_on_waitable(&self, timeout_ms: u32) -> bool {
        // SAFETY: the waitable is the chain's, open until drop.
        unsafe { WaitForSingleObjectEx(self.waitable, timeout_ms, 0) == WAIT_OBJECT_0 }
    }

    /// Test seam: take a frame from the waitable past the held one, as a
    /// frame the surface lost track of would; `false` on the timeout.
    #[cfg(test)]
    pub(crate) fn take_frame_unheld_for_test(&self, timeout_ms: u32) -> bool {
        self.wait_on_waitable(timeout_ms)
    }

    /// Buffer 0, the one a flip-model frame draws into, with its views.
    pub(crate) fn back_buffer(&mut self) -> Result<&D3dTexture, String> {
        if self.back.is_none() {
            let shared = self.dev.shared();
            let mut out = std::ptr::null_mut();
            // SAFETY: `swap` is an IDXGISwapChain3; buffer 0 as an
            // ID3D11Texture2D, one owned reference on success.
            let hr = unsafe {
                (self.swap.vtable::<dxgi::IDXGISwapChain3_Vtbl>().GetBuffer)(
                    self.swap.as_raw(),
                    0,
                    &d3d::IID_ID3D11Texture2D,
                    &mut out,
                )
            };
            let tex = shared.created("IDXGISwapChain::GetBuffer", hr, out)?;
            self.back = Some(
                self.dev
                    .adopt_texture(tex, self.format, self.width, self.height)?,
            );
        }
        Ok(self.back.as_ref().expect("set above"))
    }

    /// Resize the buffers to `width` x `height` (clamped to 1); a no-op at the
    /// current size.
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        let max = self.dev.caps().max_texture_dim;
        let (width, height) = (width.max(1), height.max(1));
        if width > max || height > max {
            return Err(format!(
                "d3d11: a {width}x{height} swap chain is outside 1..={max} per side"
            ));
        }
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        // Every reference to the buffers, the views included, must be gone.
        // The context's binding of buffer 0, which a frame that never
        // presented leaves, does not stop it (SPECS.md §5.19).
        self.back = None;
        let cur = self.dev.enter()?;
        // SAFETY: `swap` is an IDXGISwapChain3 whose buffers the surface no
        // longer holds; the count and format are kept (0, UNKNOWN), and so are
        // the flags.
        let hr = unsafe {
            (self
                .swap
                .vtable::<dxgi::IDXGISwapChain3_Vtbl>()
                .ResizeBuffers)(
                self.swap.as_raw(),
                0,
                width,
                height,
                dxgi::DXGI_FORMAT_UNKNOWN,
                self.flags,
            )
        };
        drop(cur);
        self.dev.shared().ok("IDXGISwapChain::ResizeBuffers", hr)?;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Present buffer 0, waiting for `sync_interval` refreshes (0: at the next
    /// one the compositor takes, never tearing in a window).
    pub(crate) fn present(&mut self, sync_interval: u32) -> Result<D3dPresented, String> {
        let cur = self.dev.enter()?;
        // SAFETY: `swap` is an IDXGISwapChain3; the context the runtime
        // flushes is held.
        let hr = unsafe {
            (self.swap.vtable::<dxgi::IDXGISwapChain3_Vtbl>().Present)(
                self.swap.as_raw(),
                sync_interval.min(4),
                0,
            )
        };
        drop(cur);
        // The held frame is spent, whatever the present answered.
        self.frame_held = false;
        // A success code: `Present` "succeeded" at showing nothing.
        if hr == DXGI_STATUS_OCCLUDED {
            return Ok(D3dPresented::Occluded);
        }
        // A composition chain's present needs no commit: its tree was
        // committed once, with the chain as the visual's content.
        self.dev.shared().ok("IDXGISwapChain::Present", hr)?;
        Ok(D3dPresented::Shown)
    }
}

impl Drop for D3dWindowSurface {
    /// Release the chain and force its destruction, so its window can take
    /// another at once (an HDR switch's remake, the 8-bit chain made after a
    /// refused f16 one): a window takes one flip-model chain at a time, and
    /// D3D11 defers a released chain's destruction while its context still
    /// refers to it, keeping it whole while a view of its buffer 0 is bound
    /// (`Current::clear_state_and_flush`).
    fn drop(&mut self) {
        self.back = None;
        self._tree = None;
        // SAFETY: released once, here; nothing reads `swap` after.
        unsafe { ManuallyDrop::drop(&mut self.swap) };
        // SAFETY: the waitable is the chain's, closed once here.
        unsafe { CloseHandle(self.waitable) };
        // A lost device's chain dies with it.
        if let Ok(cur) = self.dev.enter() {
            cur.clear_state_and_flush();
        }
    }
}

/// The swap chains on real windows. They need a desktop: the Windows lane
/// runs them in `lane`'s session (`lab windows exec --desktop`); session 0,
/// SSH's, has no display, and there each ends COULD NOT RUN.
#[cfg(test)]
#[path = "surface_tests.rs"]
pub(crate) mod tests;
