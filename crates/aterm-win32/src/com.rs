// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// `IUnknown_Vtbl` is derived from windows-core 0.62.2 src/unknown.rs, `IInspectable_Vtbl` from its
// src/inspectable.rs, and the interface IDs from those files, its src/imp/com_bindings.rs and
// src/imp/marshaler.rs (MIT OR Apache-2.0, see ../LICENSE-MIT and ../LICENSE-APACHE); the rest of
// the file is aterm's.

//! COM's vocabulary.

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

/// `IUnknown`'s vtable.
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

pub const IID_IUnknown: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
pub const IID_IInspectable: GUID = GUID::from_u128(0xaf86e2e0_b12d_4c6a_9c5a_d7aa65101e90);
/// `IAgileObject`: the object may be called from any apartment.
pub const IID_IAgileObject: GUID = GUID::from_u128(0x94ea2b94_e9cc_49e0_c0ff_ee64ca8f5b90);
/// `IMarshal`, which a free-threaded object answers through the free-threaded marshaler.
pub const IID_IMarshal: GUID = GUID::from_u128(0x00000003_0000_0000_c000_000000000046);

/// `IInspectable`'s vtable, the identity a WinRT-shaped object begins with.
#[repr(C)]
pub struct IInspectable_Vtbl {
    pub base: IUnknown_Vtbl,
    pub GetIids: unsafe extern "system" fn(
        this: *mut c_void,
        count: *mut u32,
        values: *mut *mut GUID,
    ) -> HRESULT,
    pub GetRuntimeClassName:
        unsafe extern "system" fn(this: *mut c_void, value: *mut *mut c_void) -> HRESULT,
    pub GetTrustLevel: unsafe extern "system" fn(this: *mut c_void, value: *mut i32) -> HRESULT,
}

/// Whether `a` and `b` are one interface ID.
#[inline]
#[must_use]
pub const fn same_iid(a: &GUID, b: &GUID) -> bool {
    let (x, y) = (a.data4, b.data4);
    let mut i = 0;
    while i < 8 {
        if x[i] != y[i] {
            return false;
        }
        i += 1;
    }
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3
}

/// One owned reference to a COM object, through an interface pointer of any interface.
pub struct Com(NonNull<c_void>);

impl Com {
    /// Takes over the reference `raw` holds; `None` for null.
    /// # Safety
    /// A non-null `raw` is a live interface pointer whose one reference the caller hands over.
    #[inline]
    pub unsafe fn from_raw(raw: *mut c_void) -> Option<Self> {
        NonNull::new(raw).map(Self)
    }

    /// Takes over the reference an interface value of another binding owns.
    /// # Safety
    /// `T` is exactly such a value: a non-null interface pointer owning one reference, whose
    /// drop does nothing but release it.
    #[inline]
    pub unsafe fn from_interface<T>(interface: T) -> Self {
        const { assert!(size_of::<T>() == size_of::<NonNull<c_void>>()) };
        const { assert!(needs_drop::<T>(), "an owned interface releases on drop") };
        let interface = ManuallyDrop::new(interface);
        // SAFETY: `T` is pointer-sized (asserted above) and, by the caller's contract, is the
        // non-null interface pointer itself; `ManuallyDrop` keeps its own release from running,
        // so the reference moves here.
        Self(unsafe {
            ptr::read_unaligned(ptr::from_ref::<T>(&interface).cast::<NonNull<c_void>>())
        })
    }

    /// The interface pointer, still owned by `self`.
    #[inline]
    #[must_use]
    pub const fn as_raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The object's vtable, viewed as `V`.
    /// # Safety
    /// The object's vtable begins with `V`'s slots, in order.
    #[inline]
    #[must_use]
    pub const unsafe fn vtable<V>(&self) -> &V {
        // SAFETY: every COM object begins with its vtable pointer, which lives as long as the
        // reference `self` holds; the caller vouches for `V`.
        unsafe { &**self.as_raw().cast::<*const V>() }
    }

    /// `QueryInterface` for `iid`: the same object as that interface, owned.
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

/// A failed step: which call, and its `HRESULT`.
pub type StepError = (&'static str, HRESULT);

/// `Err((step, hr))` when `hr` is a failure.
#[inline]
pub const fn check(hr: HRESULT, step: &'static str) -> Result<(), StepError> {
    if hr < 0 { Err((step, hr)) } else { Ok(()) }
}

/// `CoCreateInstance` of `clsid` as `iid`, in process, owned.
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

/// This thread's single-threaded COM apartment for the duration of one call (a shell dialog,
/// a decode through the imaging component), so an error path cannot leak it.
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
