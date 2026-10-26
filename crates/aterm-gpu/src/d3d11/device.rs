// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The D3D11 arm's device: one DXGI adapter, the `ID3D11Device` made on it at
//! feature level 11_0 or above, its immediate context, and the DXGI factory
//! its window swap chains come from.
//!
//! # The immediate context
//!
//! `ID3D11Device` is free-threaded; its immediate context is not. Every
//! context call happens inside a [`Current`] guard ([`D3dDevice::enter`]),
//! which holds the device's lock for the guard's life, the twin of the GL
//! arm's current-context guard: the device can move between threads (`Send`)
//! and two threads never drive the context at once. A nested `enter` on the
//! thread that already holds the device is a no-op guard, so a resource can be
//! written or read back inside an encode.
//!
//! # Device removal
//!
//! A removed device (a driver reset, a TDR, an unplugged adapter) answers
//! `GetDeviceRemovedReason` with the reason and every later call with
//! `DXGI_ERROR_DEVICE_REMOVED`. [`Current::check`] reads the reason at each
//! frame's submit and each resource step, and any call that returns a
//! removal code latches the device lost ([`D3dShared::lost`]), which
//! `GpuRenderer::device_lost` reports to the frontend's CPU fallback. The
//! injected-loss drill (`ATERM_D3D11_INJECT_LOSS=<n>`, fired after the n-th
//! present) answers the next `enter` as a removed device does, so the latch
//! is driven through the path a real removal takes.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use aterm_win32::com::Com;
use aterm_win32::core::HRESULT;
use aterm_win32::d3d11 as d3d;
use aterm_win32::dxgi;

/// What the device is, read once at creation.
#[derive(Clone, Debug)]
pub(crate) struct D3dCaps {
    /// The adapter's description (`Microsoft Basic Render Driver` for WARP),
    /// the name wgpu's DX12 backend reports for the same adapter.
    pub(crate) adapter: String,
    /// `D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION` at feature level 11_0.
    pub(crate) max_texture_dim: u32,
    /// The feature level the device was made at.
    pub(crate) feature_level: d3d::D3D_FEATURE_LEVEL,
}

/// The shared half of a device: the COM objects, the facts and the lock.
/// Resources hold an `Arc` of it, so a texture can say which device made it.
pub(crate) struct D3dShared {
    /// The `ID3D11Device`.
    device: Com,
    /// The device's immediate `ID3D11DeviceContext`, driven only under `lock`.
    context: Com,
    /// The `IDXGIFactory2` the adapter came from: window swap chains are made
    /// on it.
    factory: Com,
    pub(crate) caps: D3dCaps,
    /// Latched at the first removal code any checked call returns; the device
    /// is then dead.
    lost: AtomicBool,
    /// The injected-loss drill: the next [`D3dShared::enter`] answers as a
    /// removed device.
    inject_loss: AtomicBool,
    lock: Mutex<()>,
}

// SAFETY: `ID3D11Device` and DXGI factories are free-threaded; the immediate
// context is used only inside a `Current` guard, which holds `lock`, so no two
// threads drive it at once.
unsafe impl Send for D3dShared {}
// SAFETY: as above: `&D3dShared` reaches the context only through `enter`.
unsafe impl Sync for D3dShared {}

/// One device child (a texture, view, buffer, state object or shader) as the
/// arm holds it: an owned reference that may move between threads.
pub(crate) struct Child(Com);

// SAFETY: Direct3D 11 device children are free-threaded COM objects
// (reference counting is atomic), and the arm calls their methods only
// through the device (free-threaded) or the immediate context under the
// device's lock.
unsafe impl Send for Child {}
// SAFETY: as above.
unsafe impl Sync for Child {}

impl Child {
    /// The interface pointer, still owned by `self`.
    #[inline]
    pub(crate) fn raw(&self) -> *mut c_void {
        self.0.as_raw()
    }
}

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Child({:p})", self.raw())
    }
}

thread_local! {
    /// The device this thread holds inside a `Current` guard (null: none), so
    /// a nested `enter` on the same device is a no-op.
    static HELD: Cell<*const D3dShared> = const { Cell::new(std::ptr::null()) };
}

/// The device as the renderer holds it.
#[derive(Clone)]
pub(crate) struct D3dDevice(pub(crate) Arc<D3dShared>);

impl std::fmt::Debug for D3dDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("D3dDevice")
            .field("adapter", &self.0.caps.adapter)
            .field("feature_level", &self.0.caps.feature_level)
            .finish_non_exhaustive()
    }
}

/// The immediate context, held for this guard's life (see the module header).
pub(crate) struct Current<'a> {
    shared: &'a D3dShared,
    /// `None` for a nested guard, which releases nothing.
    outer: Option<MutexGuard<'a, ()>>,
}

/// The immediate context's pointer and vtable, for one call site.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    pub(crate) raw: *mut c_void,
    pub(crate) vt: &'a d3d::ID3D11DeviceContext_Vtbl,
}

impl Current<'_> {
    /// The immediate context.
    #[inline]
    pub(crate) fn ctx(&self) -> Ctx<'_> {
        Ctx {
            raw: self.shared.context.as_raw(),
            // SAFETY: `context` is the device's ID3D11DeviceContext.
            vt: unsafe {
                self.shared
                    .context
                    .vtable::<d3d::ID3D11DeviceContext_Vtbl>()
            },
        }
    }

    /// `Err` naming `what` when the device has been removed (latching it
    /// lost), else `Ok`.
    pub(crate) fn check(&self, what: &str) -> Result<(), String> {
        self.shared.check(what)
    }
}

impl Drop for Current<'_> {
    fn drop(&mut self) {
        if self.outer.is_some() {
            HELD.with(|h| h.set(std::ptr::null()));
        }
    }
}

/// A removal code: the device is gone and every later call fails.
#[inline]
pub(crate) const fn is_removal(hr: HRESULT) -> bool {
    matches!(
        hr,
        dxgi::DXGI_ERROR_DEVICE_REMOVED
            | dxgi::DXGI_ERROR_DEVICE_HUNG
            | dxgi::DXGI_ERROR_DEVICE_RESET
            | dxgi::DXGI_ERROR_DRIVER_INTERNAL_ERROR
    )
}

impl D3dShared {
    /// Whether the device has been removed (or the drill injected it).
    #[inline]
    pub(crate) fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Arm the injected-loss drill for the next [`Self::enter`]
    /// (`GpuRenderer::d3d11_maybe_inject_loss`, the tests).
    pub(crate) fn inject_device_loss(&self) {
        self.inject_loss.store(true, Ordering::Release);
    }

    /// Take the immediate context for this thread (see the module header).
    pub(crate) fn enter(&self) -> Result<Current<'_>, String> {
        if HELD.with(Cell::get) == std::ptr::from_ref(self) {
            return Ok(Current {
                shared: self,
                outer: None,
            });
        }
        if self.inject_loss.swap(false, Ordering::AcqRel) {
            self.lost.store(true, Ordering::Release);
        }
        if self.lost() {
            return Err("d3d11: the device was removed".to_owned());
        }
        let guard = self
            .lock
            .lock()
            .map_err(|_| "d3d11: the device lock is poisoned".to_owned())?;
        HELD.with(|h| h.set(std::ptr::from_ref(self)));
        Ok(Current {
            shared: self,
            outer: Some(guard),
        })
    }

    /// `Err` naming `what` when the device has been removed, latching it.
    pub(crate) fn check(&self, what: &str) -> Result<(), String> {
        // SAFETY: `device` is an ID3D11Device; the call takes no arguments
        // and is legal on any thread.
        let hr = unsafe { (self.device_vt().GetDeviceRemovedReason)(self.device.as_raw()) };
        if hr < 0 {
            self.lost.store(true, Ordering::Release);
            return Err(format!(
                "d3d11: {what}: the device was removed ({hr:#010x})"
            ));
        }
        Ok(())
    }

    /// Why the device is lost, for `device_loss_reason`: DXGI's removal code
    /// by name, or the drill's when the device itself is live.
    pub(crate) fn loss_reason(&self) -> String {
        // SAFETY: as in `check`.
        let hr = unsafe { (self.device_vt().GetDeviceRemovedReason)(self.device.as_raw()) };
        let name = match hr {
            dxgi::DXGI_ERROR_DEVICE_REMOVED => "DXGI_ERROR_DEVICE_REMOVED (the GPU was removed)",
            dxgi::DXGI_ERROR_DEVICE_HUNG => "DXGI_ERROR_DEVICE_HUNG (the GPU stopped responding)",
            dxgi::DXGI_ERROR_DEVICE_RESET => "DXGI_ERROR_DEVICE_RESET (the GPU was reset)",
            dxgi::DXGI_ERROR_DRIVER_INTERNAL_ERROR => {
                "DXGI_ERROR_DRIVER_INTERNAL_ERROR (the driver failed)"
            }
            0 => "the injected-loss drill (ATERM_D3D11_INJECT_LOSS); the device itself is live",
            other => return format!("the D3D11 device was removed ({other:#010x})"),
        };
        name.to_owned()
    }

    /// A failed call's message, naming `what`; a removal code latches the
    /// device lost.
    pub(crate) fn failure(&self, what: &str, hr: HRESULT) -> String {
        if is_removal(hr) {
            self.lost.store(true, Ordering::Release);
        }
        format!("d3d11: {what} failed ({hr:#010x})")
    }

    /// `Ok` for a successful `hr`, else [`Self::failure`].
    #[inline]
    pub(crate) fn ok(&self, what: &str, hr: HRESULT) -> Result<(), String> {
        if hr < 0 {
            Err(self.failure(what, hr))
        } else {
            Ok(())
        }
    }

    /// The device's pointer.
    #[inline]
    pub(crate) fn device_raw(&self) -> *mut c_void {
        self.device.as_raw()
    }

    /// The device's vtable.
    #[inline]
    pub(crate) fn device_vt(&self) -> &d3d::ID3D11Device_Vtbl {
        // SAFETY: `device` is an ID3D11Device.
        unsafe { self.device.vtable::<d3d::ID3D11Device_Vtbl>() }
    }

    /// The DXGI factory window swap chains are made on.
    #[inline]
    pub(crate) const fn factory(&self) -> &Com {
        &self.factory
    }

    /// The device itself, as a swap chain's creation takes it.
    #[inline]
    pub(crate) const fn device(&self) -> &Com {
        &self.device
    }

    /// Wrap a creation's out-pointer: `Ok` with the owned object, or the
    /// failure (a success with null is refused too).
    pub(crate) fn created(
        &self,
        what: &str,
        hr: HRESULT,
        out: *mut c_void,
    ) -> Result<Child, String> {
        if hr < 0 {
            return Err(self.failure(what, hr));
        }
        // SAFETY: a successful creation hands over one reference.
        unsafe { Com::from_raw(out) }
            .map(Child)
            .ok_or_else(|| format!("d3d11: {what} returned null"))
    }
}

/// UTF-16 description bytes up to the first NUL.
fn utf16_name(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// An adapter's description.
fn adapter_desc(adapter: &Com) -> Result<dxgi::DXGI_ADAPTER_DESC1, String> {
    let mut desc = dxgi::DXGI_ADAPTER_DESC1::default();
    // SAFETY: `adapter` is an IDXGIAdapter1; GetDesc1 fills `desc`.
    let hr = unsafe {
        (adapter.vtable::<dxgi::IDXGIAdapter1_Vtbl>().GetDesc1)(adapter.as_raw(), &mut desc)
    };
    if hr < 0 {
        return Err(format!(
            "d3d11: IDXGIAdapter1::GetDesc1 failed ({hr:#010x})"
        ));
    }
    Ok(desc)
}

/// The `ATERM_GPU_POWER` seam as DXGI's preference: `low` (the default, which
/// keeps a hybrid laptop on its integrated GPU) or `high`.
fn gpu_preference() -> dxgi::DXGI_GPU_PREFERENCE {
    match aterm_types::dev_seam!("ATERM_GPU_POWER").map(|v| v.to_string_lossy().into_owned()) {
        Some(v) if v.eq_ignore_ascii_case("high") => dxgi::DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
        Some(v) if !v.is_empty() && !v.eq_ignore_ascii_case("low") => {
            crate::stderr_line!(
                "aterm-gpu: unknown ATERM_GPU_POWER {v:?} (want low|high); using low"
            );
            dxgi::DXGI_GPU_PREFERENCE_MINIMUM_POWER
        }
        _ => dxgi::DXGI_GPU_PREFERENCE_MINIMUM_POWER,
    }
}

/// Adapter `i` of `factory` in enumeration order, `None` past the last.
fn enum_adapter(factory: &Com, i: u32) -> Option<Com> {
    let mut out = std::ptr::null_mut();
    // SAFETY: `factory` is an IDXGIFactory2; EnumAdapters1 writes one owned
    // adapter on success.
    let hr = unsafe {
        (factory.vtable::<dxgi::IDXGIFactory2_Vtbl>().EnumAdapters1)(factory.as_raw(), i, &mut out)
    };
    // SAFETY: as above.
    (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten()
}

/// The adapter to render on: the `ATERM_GPU_ADAPTER` seam's match, else
/// DXGI's first adapter by the power preference (`IDXGIFactory6`), else its
/// first adapter.
fn pick_adapter(factory: &Com) -> Result<Com, String> {
    if let Some(want) = aterm_types::dev_seam!("ATERM_GPU_ADAPTER")
        .map(|v| v.to_string_lossy().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
    {
        let found = (0..64).map_while(|i| enum_adapter(factory, i)).find(|a| {
            adapter_desc(a).is_ok_and(|d| {
                utf16_name(&d.Description)
                    .to_ascii_lowercase()
                    .contains(&want)
            })
        });
        match found {
            Some(a) => return Ok(a),
            None => crate::stderr_line!(
                "aterm-gpu: ATERM_GPU_ADAPTER={want:?} matched no adapter; using default selection"
            ),
        }
    }
    if let Ok(f6) = factory.query(&dxgi::IID_IDXGIFactory6) {
        let mut out = std::ptr::null_mut();
        // SAFETY: `f6` is an IDXGIFactory6; one owned IDXGIAdapter1 is written
        // on success.
        let hr = unsafe {
            (f6.vtable::<dxgi::IDXGIFactory6_Vtbl>()
                .EnumAdapterByGpuPreference)(
                f6.as_raw(),
                0,
                gpu_preference(),
                &dxgi::IID_IDXGIAdapter1,
                &mut out,
            )
        };
        // SAFETY: as above.
        if let Some(a) = (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten() {
            return Ok(a);
        }
    }
    enum_adapter(factory, 0).ok_or_else(|| "d3d11: DXGI lists no adapter".to_owned())
}

impl D3dDevice {
    /// A device on the adapter [`pick_adapter`] chooses. It presents to
    /// windows as well: a D3D11 device needs no window system to make swap
    /// chains, only the factory its adapter came from.
    pub(crate) fn headless() -> Result<Self, String> {
        let mut factory = std::ptr::null_mut();
        // SAFETY: CreateDXGIFactory1 writes one owned factory of the asked
        // interface on success.
        let hr = unsafe { dxgi::CreateDXGIFactory1(&dxgi::IID_IDXGIFactory2, &mut factory) };
        if hr < 0 {
            return Err(format!("d3d11: CreateDXGIFactory1 failed ({hr:#010x})"));
        }
        // SAFETY: as above.
        let factory =
            unsafe { Com::from_raw(factory) }.ok_or("d3d11: CreateDXGIFactory1 returned null")?;
        let adapter = pick_adapter(&factory)?;
        let desc = adapter_desc(&adapter)?;
        let (device, context, feature_level) = create_device(&adapter)?;
        let caps = D3dCaps {
            adapter: utf16_name(&desc.Description),
            max_texture_dim: d3d::D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION,
            feature_level,
        };
        Ok(Self(Arc::new(D3dShared {
            device,
            context,
            factory,
            caps,
            lost: AtomicBool::new(false),
            inject_loss: AtomicBool::new(false),
            lock: Mutex::new(()),
        })))
    }

    /// Take the immediate context (see the module header).
    #[inline]
    pub(crate) fn enter(&self) -> Result<Current<'_>, String> {
        self.0.enter()
    }

    /// What the device is.
    #[inline]
    pub(crate) fn caps(&self) -> &D3dCaps {
        &self.0.caps
    }

    /// The shared half.
    #[inline]
    pub(crate) const fn shared(&self) -> &Arc<D3dShared> {
        &self.0
    }
}

/// `D3D11CreateDevice` on `adapter` at feature level 11_1 or 11_0 (a runtime
/// that predates 11_1 refuses the list naming it, and is asked again without
/// it).
fn create_device(adapter: &Com) -> Result<(Com, Com, d3d::D3D_FEATURE_LEVEL), String> {
    let lists: [&[d3d::D3D_FEATURE_LEVEL]; 2] = [
        &[d3d::D3D_FEATURE_LEVEL_11_1, d3d::D3D_FEATURE_LEVEL_11_0],
        &[d3d::D3D_FEATURE_LEVEL_11_0],
    ];
    let mut last = 0;
    for levels in lists {
        let (mut device, mut context, mut level) = (std::ptr::null_mut(), std::ptr::null_mut(), 0);
        // SAFETY: a live adapter (so the driver type is UNKNOWN); a feature
        // level list of the stated length; three out-pointers, written on
        // success with one owned reference each.
        let hr = unsafe {
            d3d::D3D11CreateDevice(
                adapter.as_raw(),
                d3d::D3D_DRIVER_TYPE_UNKNOWN,
                std::ptr::null_mut(),
                d3d::D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                levels.as_ptr(),
                levels.len() as u32,
                d3d::D3D11_SDK_VERSION,
                &mut device,
                &mut level,
                &mut context,
            )
        };
        if hr >= 0 {
            // SAFETY: as above.
            let device = unsafe { Com::from_raw(device) };
            // SAFETY: as above.
            let context = unsafe { Com::from_raw(context) };
            return match (device, context) {
                (Some(d), Some(c)) => Ok((d, c, level)),
                _ => Err("d3d11: D3D11CreateDevice returned null".to_owned()),
            };
        }
        last = hr;
    }
    Err(format!(
        "d3d11: D3D11CreateDevice at feature level 11_0 failed ({last:#010x})"
    ))
}
