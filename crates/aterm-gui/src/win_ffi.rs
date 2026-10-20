// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The Win32 FFI vocabulary the shell-integration modules share, declared once:
//! the COM half (`Guid`, the `IUnknown` prefix, the owned [`Com`] pointer and
//! [`co_create`]) behind the file picker and the jump list, and the `advapi32`
//! registry half behind the Explorer verb and the DefTerm registration.
//!
//! Hand-rolled flat-C FFI, the house style (no `windows`-crate dependency); the
//! interface vtables stay beside the module that calls them.
#![cfg(windows)]

use std::ffi::c_void;

/// NUL-terminated UTF-16, the `PCWSTR` shape every call takes.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// COM
// ---------------------------------------------------------------------------

#[repr(C)]
pub(crate) struct Guid {
    pub(crate) data1: u32,
    pub(crate) data2: u16,
    pub(crate) data3: u16,
    pub(crate) data4: [u8; 8],
}

/// The 3-slot `IUnknown` prefix every COM vtable begins with; any interface
/// pointer can be viewed through it for `QueryInterface`/`Release` without
/// caring which concrete interface it is.
#[repr(C)]
pub(crate) struct IUnknownVtbl {
    pub(crate) query_interface:
        unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    pub(crate) add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub(crate) release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
pub(crate) struct IUnknownRepr {
    pub(crate) vtbl: *const IUnknownVtbl,
}

pub(crate) const CLSCTX_INPROC_SERVER: u32 = 0x1;
pub(crate) const COINIT_APARTMENTTHREADED: u32 = 0x2;

#[link(name = "ole32")]
unsafe extern "system" {
    pub(crate) fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> i32;
    pub(crate) fn CoUninitialize();
    fn CoCreateInstance(
        rclsid: *const Guid,
        outer: *mut c_void,
        cls_ctx: u32,
        riid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> i32;
}

/// An owned COM interface pointer of any interface, released through the shared
/// `IUnknown` prefix, so an early return cannot leak a reference.
pub(crate) struct Com(pub(crate) *mut c_void);

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live interface pointer obtained from
        // CoCreateInstance/QueryInterface/GetResult on this thread; every COM
        // vtable begins with the IUnknown triple, so the prefix view is sound.
        unsafe {
            let unk = self.0.cast::<IUnknownRepr>();
            ((*(*unk).vtbl).release)(self.0);
        }
    }
}

/// A failed step: which call, and its HRESULT. A failing shell nicety costs one warn
/// line, never more.
pub(crate) type StepError = (&'static str, i32);

/// `RPC_E_CHANGED_MODE` — this thread is already in an apartment of the OTHER
/// model. Not a failure for us: the shell dialog and the imaging component
/// work from either, and the apartment we did not enter is not ours to leave.
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

/// This thread's COM apartment for the duration of one call (the file
/// picker's dialog, a wallpaper decode through the imaging component), so an
/// error path cannot leak it (`Com` releases the references).
///
/// `owned` is the whole subtlety. On the UI thread winit has already run
/// `OleInitialize`, so our `CoInitializeEx` returns `S_FALSE` — still a SUCCESS
/// that took a reference, and one we must therefore drop. `RPC_E_CHANGED_MODE`
/// means the thread is an MTA that somebody else established: both callers are
/// happy there, but we took no reference and must not release one.
pub(crate) struct Apartment {
    owned: bool,
}

impl Apartment {
    pub(crate) fn enter() -> Result<Self, StepError> {
        // SAFETY: no reserved parameter; STA is the apartment a shell dialog
        // wants, and is the one the UI thread is already in.
        let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
        if hr >= 0 {
            // S_OK and S_FALSE both took a reference; both must be balanced.
            Ok(Self { owned: true })
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(Self { owned: false })
        } else {
            Err(("CoInitializeEx", hr))
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balanced against the successful CoInitializeEx in `enter`,
            // on the SAME thread, as COM requires.
            unsafe { CoUninitialize() };
        }
    }
}

pub(crate) fn check(hr: i32, step: &'static str) -> Result<(), StepError> {
    if hr < 0 { Err((step, hr)) } else { Ok(()) }
}

/// `CoCreateInstance` of `clsid` as `iid`, in process, owned.
pub(crate) unsafe fn co_create(
    clsid: &Guid,
    iid: &Guid,
    step: &'static str,
) -> Result<Com, StepError> {
    let mut ppv: *mut c_void = std::ptr::null_mut();
    // SAFETY: standard object creation; `ppv` receives the interface pointer
    // only on success, and a success with a null pointer is rejected.
    let hr = unsafe {
        CoCreateInstance(
            clsid,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            iid,
            &mut ppv,
        )
    };
    if hr < 0 || ppv.is_null() {
        return Err((step, hr));
    }
    Ok(Com(ppv))
}

// ---------------------------------------------------------------------------
// Registry (advapi32)
// ---------------------------------------------------------------------------

pub(crate) type Hkey = isize;
pub(crate) const HKEY_CURRENT_USER: Hkey = 0x8000_0001u32 as Hkey;
pub(crate) const REG_SZ: u32 = 1;
pub(crate) const KEY_WRITE: u32 = 0x0002_0006;
pub(crate) const REG_OPTION_NON_VOLATILE: u32 = 0;

#[link(name = "advapi32")]
unsafe extern "system" {
    pub(crate) fn RegCreateKeyExW(
        hkey: Hkey,
        subkey: *const u16,
        reserved: u32,
        class: *const u16,
        options: u32,
        sam: u32,
        security: *const c_void,
        result: *mut Hkey,
        disposition: *mut u32,
    ) -> i32;
    pub(crate) fn RegSetValueExW(
        hkey: Hkey,
        name: *const u16,
        reserved: u32,
        ty: u32,
        data: *const u8,
        cb: u32,
    ) -> i32;
    pub(crate) fn RegCloseKey(hkey: Hkey) -> i32;
}
