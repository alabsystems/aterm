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
    /// The `IDXGISwapChain3`.
    swap: Com,
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
        let latency = latency.max(1);
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
            swap,
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
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        // Every reference to the buffers, the views included, must be gone.
        self.back = None;
        let cur = self.dev.enter()?;
        // SAFETY: `swap` is an IDXGISwapChain3 whose buffers nothing holds; the
        // count and format are kept (0, UNKNOWN), and so are the flags.
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
    fn drop(&mut self) {
        self.back = None;
        // SAFETY: the waitable is the chain's, closed once here.
        unsafe { CloseHandle(self.waitable) };
    }
}

/// The swap chains on real windows. They need a desktop: the Windows lane
/// runs them in `lane`'s session (`lab windows exec --desktop`); session 0,
/// SSH's, has no display, and there each ends COULD NOT RUN.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::d3d11::encoder::D3dFrame;
    use crate::d3d11::resources::D3dBufferRole;
    use crate::device_layer::{ClearColor4, FrameLoad};
    use aterm_win32::Win32::UI::WindowsAndMessaging as wm;

    /// kernel32's own declarations (processthreadsapi.h), independent of
    /// aterm-win32's, for the session check.
    mod kernel32 {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            pub fn GetCurrentProcessId() -> u32;
            pub fn ProcessIdToSessionId(process: u32, session: *mut u32) -> i32;
        }
    }

    /// COULD NOT RUN in session 0, which has no display.
    pub(crate) fn desktop_or_could_not_run() {
        let mut session = 0;
        // SAFETY: plain Win32 calls; `session` receives the id.
        let ok = unsafe {
            kernel32::ProcessIdToSessionId(kernel32::GetCurrentProcessId(), &mut session)
        };
        if ok == 0 || session == 0 {
            aterm_spec::host::could_not_run(
                "session 0 has no display; run it in a desktop session \
                 (`lab windows exec --desktop`)",
            );
        }
    }

    /// A shown top-level window on this thread, destroyed on drop.
    pub(crate) struct Window(pub(crate) HWND);

    impl Window {
        pub(crate) fn new(ex_style: u32) -> Self {
            static CLASS: std::sync::OnceLock<u16> = std::sync::OnceLock::new();
            let name: Vec<u16> = "aterm-d3d11-surface-test\0".encode_utf16().collect();
            let atom = *CLASS.get_or_init(|| {
                let class = wm::WNDCLASSEXW {
                    cbSize: std::mem::size_of::<wm::WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(wm::DefWindowProcW),
                    lpszClassName: name.as_ptr(),
                    ..Default::default()
                };
                // SAFETY: a complete class description whose name outlives
                // the call (the class copies it).
                unsafe { wm::RegisterClassExW(&class) }
            });
            assert_ne!(atom, 0, "the test window class registers");
            // SAFETY: the class registered above; no parent, menu or data.
            let hwnd = unsafe {
                wm::CreateWindowExW(
                    ex_style,
                    name.as_ptr(),
                    name.as_ptr(),
                    wm::WS_OVERLAPPEDWINDOW,
                    40,
                    40,
                    400,
                    260,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            assert!(!hwnd.is_null(), "the test window opens");
            // SAFETY: the window just made.
            unsafe { wm::ShowWindow(hwnd, wm::SW_SHOWNOACTIVATE) };
            let w = Self(hwnd);
            w.pump();
            w
        }

        /// Drain this thread's queue, so the window is shown and sized.
        pub(crate) fn pump(&self) {
            let mut msg = wm::MSG::default();
            // SAFETY: `msg` receives each message, dispatched to its window.
            while unsafe { wm::PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, wm::PM_REMOVE) }
                != 0
            {
                // SAFETY: as above.
                unsafe {
                    wm::TranslateMessage(&msg);
                    wm::DispatchMessageW(&msg);
                }
            }
        }
    }

    impl Drop for Window {
        fn drop(&mut self) {
            // SAFETY: the window this test made, on its thread.
            unsafe { wm::DestroyWindow(self.0) };
            self.pump();
        }
    }

    /// Clear buffer 0 to `rgba`, read it back, and return its texels.
    fn clear_and_read(surf: &mut D3dWindowSurface, rgba: [f64; 4]) -> Vec<u8> {
        let dev = surf.dev.clone();
        let uniform = dev
            .buffer(D3dBufferRole::Uniform, 16)
            .expect("a uniform block");
        let (w, h) = surf.size();
        let back = surf.back_buffer().expect("buffer 0");
        assert_eq!(
            (back.width, back.height),
            (w, h),
            "buffer 0 has the chain's size"
        );
        let mut frame = D3dFrame::begin(&dev).expect("the context");
        {
            let [r, g, b, a] = rgba;
            let _pass = frame
                .begin_pass(
                    back,
                    false,
                    FrameLoad::Clear(ClearColor4 { r, g, b, a }),
                    None,
                    (&uniform, 0),
                )
                .expect("a pass over buffer 0");
        }
        frame.submit().expect("the clear submits");
        dev.read_rgba8(back, w, h, w as usize * 4)
            .expect("buffer 0 reads back")
    }

    /// Every texel of `texels` is `want`.
    fn uniform(texels: &[u8], want: [u8; 4]) -> bool {
        texels.as_chunks::<4>().0.iter().all(|t| *t == want)
    }

    /// The HWND chain: flip model with the waitable, buffer 0 drawn and read
    /// back exactly, shown, then resized and drawn again at the new size.
    /// The control: the second clear's colour is in the buffer, not the
    /// first's.
    #[test]
    fn a_window_swap_chain_draws_presents_and_resizes() {
        desktop_or_could_not_run();
        let dev = crate::d3d11::test_device();
        let win = Window::new(0);
        // SAFETY: the window is live, on this thread, and outlives `surf`.
        let mut surf = unsafe {
            D3dWindowSurface::create(
                &dev,
                win.0,
                D3dSwapKind::Hwnd,
                (256, 128),
                TexelFormat::Rgba8Unorm,
                2,
            )
        }
        .expect("a flip-model swap chain on the window");
        assert_eq!(surf.kind(), D3dSwapKind::Hwnd);
        assert_eq!(surf.format(), TexelFormat::Rgba8Unorm);
        assert!(
            surf.wait_for_frame(1000),
            "the waitable lets the first frame start"
        );
        let red = clear_and_read(&mut surf, [1.0, 0.0, 0.2, 1.0]);
        assert!(uniform(&red, [255, 0, 51, 255]), "buffer 0 holds the clear");
        assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
        win.pump();

        surf.resize(300, 150).expect("the buffers resize");
        assert_eq!(surf.size(), (300, 150));
        assert!(
            surf.wait_for_frame(1000),
            "the waitable lets the next frame start"
        );
        let blue = clear_and_read(&mut surf, [0.0, 0.2, 1.0, 1.0]);
        assert_eq!(blue.len(), 300 * 150 * 4);
        assert!(
            uniform(&blue, [0, 51, 255, 255]),
            "the resized buffer holds the new clear"
        );
        assert!(!uniform(&blue, [255, 0, 51, 255]), "and not the first one");
        assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
        win.pump();
    }

    /// The composition chain behind Mica and Acrylic: premultiplied buffers
    /// shown by a DirectComposition visual on a window without a redirection
    /// bitmap; a translucent texel is stored as drawn.
    #[test]
    fn a_composition_swap_chain_holds_premultiplied_texels() {
        desktop_or_could_not_run();
        let dev = crate::d3d11::test_device();
        let win = Window::new(wm::WS_EX_NOREDIRECTIONBITMAP);
        // SAFETY: as above.
        let mut surf = unsafe {
            D3dWindowSurface::create(
                &dev,
                win.0,
                D3dSwapKind::Composition,
                (200, 100),
                TexelFormat::Rgba8Unorm,
                2,
            )
        }
        .expect("a composition swap chain shown on the window");
        assert_eq!(surf.kind(), D3dSwapKind::Composition);
        assert!(surf.wait_for_frame(1000));
        let half = clear_and_read(&mut surf, [0.4, 0.0, 0.2, 0.4]);
        assert!(
            uniform(&half, [102, 0, 51, 102]),
            "a premultiplied texel is kept"
        );
        assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
        win.pump();
    }

    /// The EDR chain: f16 buffers that take the scRGB tag (WARP's chains
    /// support it) and present; tagged for the screen only where the
    /// window's output is in HDR mode, refused by a name that says scRGB
    /// otherwise (the attach then makes an 8-bit chain), so never a silent
    /// untagged f16 chain.
    #[test]
    fn an_f16_swap_chain_takes_scrgb_where_the_output_is_hdr() {
        desktop_or_could_not_run();
        let dev = crate::d3d11::test_device();
        let win = Window::new(0);
        // SAFETY: as above.
        let mut surf = unsafe {
            D3dWindowSurface::create(
                &dev,
                win.0,
                D3dSwapKind::Hwnd,
                (128, 64),
                TexelFormat::Rgba16Float,
                2,
            )
        }
        .expect("an f16 swap chain");
        assert_eq!(surf.format(), TexelFormat::Rgba16Float);
        surf.tag_scrgb()
            .expect("the chain takes the scRGB colour space");
        let hdr = surf.output_hdr();
        match surf.tag_scrgb_on_hdr_output() {
            Ok(()) => assert!(hdr, "tagged for the screen only on an HDR output"),
            Err(e) => {
                assert!(!hdr, "an HDR output takes the tag: {e}");
                assert!(e.contains("scRGB"), "the refusal names scRGB: {e}");
            }
        }
        crate::stderr_line!("d3d11 surface: the window's output is HDR: {hdr}");
        assert!(surf.wait_for_frame(1000));
        let back = surf.back_buffer().expect("an f16 buffer 0");
        assert_eq!(back.texel_format(), TexelFormat::Rgba16Float);
        assert_eq!(surf.present(1), Ok(D3dPresented::Shown));
    }

    /// A frame the waitable gave stays held until a present spends it: a
    /// second wait with no present between answers with the held frame, and
    /// the wait after a present answers with the frame that present gives
    /// back. The negative control: the waitable itself has no second frame
    /// to give a latency-1 chain before a present, and times out, which is
    /// the wait every later frame would time out on without the hold.
    #[test]
    fn a_waited_frame_is_held_until_a_present_spends_it() {
        desktop_or_could_not_run();
        let dev = crate::d3d11::test_device();
        let win = Window::new(0);
        // SAFETY: as above.
        let mut surf = unsafe {
            D3dWindowSurface::create(
                &dev,
                win.0,
                D3dSwapKind::Hwnd,
                (64, 64),
                TexelFormat::Rgba8Unorm,
                1,
            )
        }
        .expect("a swap chain");
        assert!(surf.wait_for_frame(60_000), "the first frame");
        assert!(
            surf.wait_for_frame(1000),
            "a frame no present spent answers the next wait"
        );
        assert!(
            !surf.take_frame_unheld_for_test(1000),
            "the control: the waitable has no second frame before a present"
        );
        clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(surf.present(0), Ok(D3dPresented::Shown));
        win.pump();
        assert!(
            surf.wait_for_frame(60_000),
            "the present gives the waitable its frame back"
        );
    }

    /// A minimized window's present gives the waitable its frame back, which
    /// the hold's release on every present relies on. What `Present`
    /// answers there is printed: in the Windows lane's desktop, on WARP, it
    /// is `Shown` (DXGI does not report the minimized flip chain occluded),
    /// so this does not reach the `Occluded` arm. The control: the same
    /// chain, not minimized, answers `Shown` and gives the frame back.
    #[test]
    fn a_minimized_windows_present_gives_the_waitable_its_frame_back() {
        desktop_or_could_not_run();
        let dev = crate::d3d11::test_device();
        let win = Window::new(0);
        // SAFETY: as above.
        let mut surf = unsafe {
            D3dWindowSurface::create(
                &dev,
                win.0,
                D3dSwapKind::Hwnd,
                (64, 64),
                TexelFormat::Rgba8Unorm,
                1,
            )
        }
        .expect("a swap chain");
        assert!(surf.wait_for_frame(60_000), "the first frame");
        clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(surf.present(0), Ok(D3dPresented::Shown), "the control");
        win.pump();
        assert!(
            surf.wait_for_frame(60_000),
            "the control's frame comes back"
        );
        // SAFETY: the window this test made, on its thread.
        unsafe { wm::ShowWindow(win.0, wm::SW_MINIMIZE) };
        win.pump();
        clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
        let minimized = surf.present(0);
        win.pump();
        let back = surf.wait_for_frame(60_000);
        crate::stderr_line!(
            "d3d11 surface: a minimized window's present answered {minimized:?}; \
             the waitable gave the frame back: {back}"
        );
        assert!(minimized.is_ok(), "{minimized:?}");
        assert!(back, "the minimized window's present gives its frame back");
    }

    /// The removal latch at the present: after the injected-loss drill the
    /// present fails and the device reports itself lost. The control: the
    /// same present before the drill shows the frame.
    #[test]
    fn a_removed_device_fails_the_present_and_latches_lost() {
        desktop_or_could_not_run();
        let dev = crate::d3d11::test_device();
        let win = Window::new(0);
        // SAFETY: as above.
        let mut surf = unsafe {
            D3dWindowSurface::create(
                &dev,
                win.0,
                D3dSwapKind::Hwnd,
                (64, 64),
                TexelFormat::Rgba8Unorm,
                1,
            )
        }
        .expect("a swap chain");
        assert!(surf.wait_for_frame(1000));
        clear_and_read(&mut surf, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            surf.present(1),
            Ok(D3dPresented::Shown),
            "the control shows"
        );
        assert!(!dev.shared().lost());
        dev.shared().inject_device_loss();
        let e = surf
            .present(1)
            .expect_err("a removed device's present fails");
        assert!(e.contains("removed"), "{e}");
        assert!(dev.shared().lost(), "the removal is latched");
    }
}
