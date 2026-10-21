// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// HAND-WRITTEN, not generated: `aterm-win32-oracle gen` neither emits nor
// deletes this file, and harvests the generated declarations it names like a
// consumer's. `IUnknown_Vtbl` is derived from windows-core 0.62.2
// src/unknown.rs (MIT OR Apache-2.0, see ../LICENSE-MIT and
// ../LICENSE-APACHE); the rest moved here from aterm-gui's src/win_ffi.rs
// (WB.2 of docs/AUDIT-third-party-2026-10-15.md).

//! COM's vocabulary: an owned interface pointer of any interface ([`Com`]),
//! the `IUnknown` slots every vtable begins with ([`IUnknown_Vtbl`]), and the
//! creation, apartment and result helpers aterm's COM callers share.
//!
//! windows-sys declares no interfaces, so none of this is generated. The
//! vtable is `windows` 0.62.2's, and win32-oracle/conformance/src/com.rs holds
//! it to that crate: each slot's offset and fn type.

use crate::Win32::Foundation::{E_POINTER, RPC_E_CHANGED_MODE};
use crate::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use crate::core::{GUID, HRESULT};
use core::ffi::c_void;
use core::marker::PhantomData;
use core::mem::{ManuallyDrop, needs_drop, size_of};
use core::ptr::{self, NonNull};

/// `IUnknown`'s vtable: the three slots every COM vtable begins with, so any
/// interface pointer is queried and released through them.
#[repr(C)]
pub struct IUnknown_Vtbl {
    pub QueryInterface: unsafe extern "system" fn(
        this: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(this: *mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(this: *mut c_void) -> u32,
}

/// One owned reference to a COM object, through an interface pointer of any
/// interface: dropping it calls `Release` once, so an early return cannot
/// leak the reference.
pub struct Com(NonNull<c_void>);

impl Com {
    /// Takes over the reference `raw` holds; `None` for null.
    ///
    /// # Safety
    ///
    /// A non-null `raw` is a live interface pointer whose one reference the
    /// caller hands over: a creation's or a `QueryInterface`'s result that
    /// nothing else releases.
    pub unsafe fn from_raw(raw: *mut c_void) -> Option<Self> {
        NonNull::new(raw).map(Self)
    }

    /// Takes over the reference an interface value of another binding owns,
    /// without naming that binding: `windows`' interface types (which
    /// wgpu-hal's DX12 backend hands out) are one non-null interface pointer,
    /// `#[repr(transparent)]`, released once on drop. That drop does not run;
    /// this `Com`'s releases the reference instead. A `T` that is not
    /// pointer-sized does not compile, and neither does one without a drop: a
    /// borrowed `&IDXGISwapChain3` or a raw pointer is pointer-sized too, but
    /// owns no reference to move.
    ///
    /// # Safety
    ///
    /// `T` is exactly such a value: a non-null interface pointer owning one
    /// reference, whose drop does nothing but release it.
    pub unsafe fn from_interface<T>(interface: T) -> Self {
        const { assert!(size_of::<T>() == size_of::<NonNull<c_void>>()) };
        const { assert!(needs_drop::<T>(), "an owned interface releases on drop") };
        let interface = ManuallyDrop::new(interface);
        // SAFETY: `T` is pointer-sized (asserted above) and, by the caller's
        // contract, is the non-null interface pointer itself; `ManuallyDrop`
        // keeps its own release from running, so the reference moves here.
        Self(unsafe {
            ptr::read_unaligned(ptr::from_ref::<T>(&interface).cast::<NonNull<c_void>>())
        })
    }

    /// The interface pointer, still owned by `self`.
    #[must_use]
    pub fn as_raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The object's vtable, viewed as `V`.
    ///
    /// # Safety
    ///
    /// The object's vtable begins with `V`'s slots, in order.
    #[must_use]
    pub unsafe fn vtable<V>(&self) -> &V {
        // SAFETY: every COM object begins with its vtable pointer, which
        // lives as long as the reference `self` holds; the caller vouches
        // for `V`.
        unsafe { &**self.as_raw().cast::<*const V>() }
    }

    /// `QueryInterface` for `iid`: the same object as that interface, owned.
    /// `Err` is the failing `HRESULT`, or `E_POINTER` for a success that
    /// handed back null.
    pub fn query(&self, iid: &GUID) -> Result<Self, HRESULT> {
        let mut out = ptr::null_mut();
        // SAFETY: every vtable begins with IUnknown's slots; `self` holds a
        // live reference, and `out` is written only on success.
        let hr = unsafe {
            (self.vtable::<IUnknown_Vtbl>().QueryInterface)(self.as_raw(), iid, &mut out)
        };
        if hr < 0 {
            return Err(hr);
        }
        // SAFETY: a successful QueryInterface hands over one reference.
        unsafe { Self::from_raw(out) }.ok_or(E_POINTER)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: `self` owns one reference to a live object, released here
        // once through the IUnknown slots every vtable begins with.
        unsafe { (self.vtable::<IUnknown_Vtbl>().Release)(self.as_raw()) };
    }
}

/// A failed step: which call, and its `HRESULT`. A failing shell nicety costs
/// its caller one warn line, never more.
pub type StepError = (&'static str, HRESULT);

/// `Err((step, hr))` when `hr` is a failure.
pub fn check(hr: HRESULT, step: &'static str) -> Result<(), StepError> {
    if hr < 0 { Err((step, hr)) } else { Ok(()) }
}

/// `CoCreateInstance` of `clsid` as `iid`, in process, owned. Fails (it is
/// not undefined) on a thread with no COM apartment.
pub fn co_create(clsid: &GUID, iid: &GUID, step: &'static str) -> Result<Com, StepError> {
    let mut out = ptr::null_mut();
    // SAFETY: standard object creation; `out` receives the interface pointer
    // only on success, and a success with a null pointer is refused below.
    let hr =
        unsafe { CoCreateInstance(clsid, ptr::null_mut(), CLSCTX_INPROC_SERVER, iid, &mut out) };
    check(hr, step)?;
    // SAFETY: a successful creation hands over one reference.
    unsafe { Com::from_raw(out) }.ok_or((step, E_POINTER))
}

/// This thread's single-threaded COM apartment for the duration of one call
/// (a shell dialog, a decode through the imaging component), so an error path
/// cannot leak it.
///
/// `owned` is the whole subtlety. On a UI thread whose window library already
/// ran `OleInitialize`, `CoInitializeEx` returns `S_FALSE`: still a success
/// that took a reference, which must therefore be dropped.
/// `RPC_E_CHANGED_MODE` means the thread is in a multithreaded apartment
/// somebody else entered: callers that work in either are served there, but
/// no reference was taken and none may be released.
pub struct Apartment {
    owned: bool,
    /// Not `Send`: it is left on the thread that entered it.
    _thread: PhantomData<*const ()>,
}

impl Apartment {
    /// Enters (or shares) this thread's single-threaded apartment.
    pub fn enter() -> Result<Self, StepError> {
        // SAFETY: no reserved parameter; STA is the apartment a shell dialog
        // wants, and is the one a UI thread is already in.
        let hr = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if hr >= 0 {
            // S_OK and S_FALSE both took a reference; both must be balanced.
            Ok(Self {
                owned: true,
                _thread: PhantomData,
            })
        } else if hr == RPC_E_CHANGED_MODE {
            Ok(Self {
                owned: false,
                _thread: PhantomData,
            })
        } else {
            Err(("CoInitializeEx", hr))
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balanced against the successful CoInitializeEx in
            // `enter`; the type is not Send, so this is the same thread, as
            // COM requires.
            unsafe { CoUninitialize() };
        }
    }
}
