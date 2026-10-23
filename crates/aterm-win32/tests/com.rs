// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance and reference-counting tests for [`Com`] and DXGI declarations.

#![allow(non_snake_case)]

use aterm_win32::Win32::Foundation::E_POINTER;
use aterm_win32::com::{Com, IUnknown_Vtbl, check};
use aterm_win32::core::{GUID, HRESULT};
use aterm_win32::dxgi as D;
use core::cell::Cell;
use core::ffi::c_void;
use core::ptr::{self, NonNull};

/// Slot `n` of a vtable: its byte offset.
#[cfg(target_pointer_width = "64")]
const fn slot(n: usize) -> usize {
    n * 8
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    use core::mem::{align_of, offset_of, size_of};
    assert!(offset_of!(IUnknown_Vtbl, QueryInterface) == slot(0));
    assert!(offset_of!(IUnknown_Vtbl, AddRef) == slot(1));
    assert!(offset_of!(IUnknown_Vtbl, Release) == slot(2));
    assert!(size_of::<IUnknown_Vtbl>() == slot(3));
    assert!(offset_of!(D::IDXGIFactory1_Vtbl, EnumAdapters1) == slot(12));
    assert!(size_of::<D::IDXGIFactory1_Vtbl>() == slot(13));
    assert!(offset_of!(D::IDXGIAdapter1_Vtbl, EnumOutputs) == slot(7));
    assert!(size_of::<D::IDXGIAdapter1_Vtbl>() == slot(8));
    assert!(offset_of!(D::IDXGIOutput6_Vtbl, GetDesc1) == slot(27));
    assert!(size_of::<D::IDXGIOutput6_Vtbl>() == slot(28));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, GetContainingOutput) == slot(15));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, CheckColorSpaceSupport) == slot(37));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, SetColorSpace1) == slot(38));
    assert!(size_of::<D::IDXGISwapChain3_Vtbl>() == slot(39));

    assert!(size_of::<D::DXGI_OUTPUT_DESC1>() == 152 && align_of::<D::DXGI_OUTPUT_DESC1>() == 8);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, DeviceName) == 0);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, DesktopCoordinates) == 64);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, AttachedToDesktop) == 80);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, Rotation) == 84);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, Monitor) == 88);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, BitsPerColor) == 96);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, ColorSpace) == 100);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, RedPrimary) == 104);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, GreenPrimary) == 112);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, BluePrimary) == 120);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, WhitePoint) == 128);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, MinLuminance) == 136);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, MaxLuminance) == 140);
    assert!(offset_of!(D::DXGI_OUTPUT_DESC1, MaxFullFrameLuminance) == 144);
};

const _: () = {
    assert!(D::DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 == 1);
    assert!(D::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 == 12);
    assert!(D::DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT == 1);
};

fn same(a: &GUID, b: &GUID) -> bool {
    (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
}

#[test]
fn interface_ids_are_upstreams() {
    let factory = GUID {
        data1: 0x770a_ae78,
        data2: 0xf26f,
        data3: 0x4dba,
        data4: [0xa8, 0x29, 0x25, 0x3c, 0x83, 0xd1, 0xb3, 0x87],
    };
    let output6 = GUID {
        data1: 0x0683_46e8,
        data2: 0xaaec,
        data3: 0x4b84,
        data4: [0xad, 0xd7, 0x13, 0x7f, 0x51, 0x3f, 0x77, 0xa1],
    };
    assert!(same(&D::IID_IDXGIFactory1, &factory));
    assert!(same(&D::IID_IDXGIOutput6, &output6));
    assert!(
        !same(&D::IID_IDXGIFactory1, &output6),
        "the comparison can fail"
    );
}

#[test]
fn an_output_description_starts_zeroed() {
    let desc = D::DXGI_OUTPUT_DESC1::default();
    assert_eq!(desc.DeviceName, [0; 32]);
    assert!(desc.Monitor.is_null());
    assert_eq!((desc.ColorSpace, desc.MaxLuminance.to_bits()), (0, 0));
}

/// The stand-in's refusal (winerror.h's value; aterm-win32 needs none).
const E_NOINTERFACE: HRESULT = 0x8000_4002_u32.cast_signed();

/// The one interface the stand-in answers `QueryInterface` for.
const KNOWN: GUID = GUID::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
const OTHER: GUID = GUID::from_u128(0xfedc_ba98_7654_3210_fedc_ba98_7654_3210);

/// A COM object stand-in.
#[repr(C)]
struct Object {
    vtable: *const IUnknown_Vtbl,
    refs: Cell<u32>,
    releases: Cell<u32>,
    /// `QueryInterface` succeeds but hands back null.
    null_success: bool,
}

unsafe extern "system" fn query_interface(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: `this` is an `Object` (every `Com` here wraps one); `iid` and `out` are
    // `Com::query`'s live arguments.
    let (object, iid) = unsafe { (&*this.cast::<Object>(), &*iid) };
    let (hr, result) = if !same(iid, &KNOWN) {
        (E_NOINTERFACE, ptr::null_mut())
    } else if object.null_success {
        (0, ptr::null_mut())
    } else {
        object.refs.set(object.refs.get() + 1);
        (0, this)
    };
    // SAFETY: as above.
    unsafe { *out = result };
    hr
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    // SAFETY: `this` is an `Object`.
    let object = unsafe { &*this.cast::<Object>() };
    object.refs.set(object.refs.get() + 1);
    object.refs.get()
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    // SAFETY: `this` is an `Object`.
    let object = unsafe { &*this.cast::<Object>() };
    object.releases.set(object.releases.get() + 1);
    object.refs.set(object.refs.get() - 1);
    object.refs.get()
}

static VTABLE: IUnknown_Vtbl = IUnknown_Vtbl {
    QueryInterface: query_interface,
    AddRef: add_ref,
    Release: release,
};

fn object(null_success: bool) -> Object {
    Object {
        vtable: &VTABLE,
        refs: Cell::new(1),
        releases: Cell::new(0),
        null_success,
    }
}

fn raw(object: &Object) -> *mut c_void {
    ptr::from_ref(object).cast_mut().cast()
}

#[test]
fn null_is_no_object() {
    // SAFETY: null hands over nothing.
    assert!(unsafe { Com::from_raw(ptr::null_mut()) }.is_none());
}

#[test]
fn an_owned_pointer_releases_its_reference_once() {
    let object = object(false);
    // SAFETY: `object` is live past the `Com`, which takes its one reference.
    let com = unsafe { Com::from_raw(raw(&object)) }.expect("non-null");
    assert_eq!(com.as_raw(), raw(&object));
    // SAFETY: the stand-in's vtable begins with IUnknown's slots.
    let vtable = unsafe { com.vtable::<IUnknown_Vtbl>() };
    assert!(ptr::eq(vtable, &VTABLE));
    assert_eq!(object.releases.get(), 0);
    drop(com);
    assert_eq!((object.refs.get(), object.releases.get()), (0, 1));
}

#[test]
fn a_query_owns_one_more_reference_or_none() {
    let object = object(false);
    // SAFETY: as in `an_owned_pointer_releases_its_reference_once`.
    let com = unsafe { Com::from_raw(raw(&object)) }.expect("non-null");
    let other = com.query(&KNOWN).expect("the stand-in answers KNOWN");
    assert_eq!(object.refs.get(), 2);
    assert_eq!(com.query(&OTHER).err(), Some(E_NOINTERFACE));
    assert_eq!(object.refs.get(), 2, "a refused query takes no reference");
    drop(other);
    drop(com);
    assert_eq!((object.refs.get(), object.releases.get()), (0, 2));
}

#[test]
fn a_null_success_is_e_pointer_and_takes_nothing() {
    let object = object(true);
    // SAFETY: as in `an_owned_pointer_releases_its_reference_once`.
    let com = unsafe { Com::from_raw(raw(&object)) }.expect("non-null");
    assert_eq!(com.query(&KNOWN).err(), Some(E_POINTER));
    drop(com);
    assert_eq!((object.refs.get(), object.releases.get()), (0, 1));
}

/// Another binding's interface value.
#[repr(transparent)]
struct Foreign(NonNull<c_void>);

thread_local! {
    static FOREIGN_DROPS: Cell<u32> = const { Cell::new(0) };
}

impl Drop for Foreign {
    fn drop(&mut self) {
        FOREIGN_DROPS.with(|d| d.set(d.get() + 1));
        // SAFETY: a `Foreign` owns one reference to an `Object`.
        unsafe { (VTABLE.Release)(self.0.as_ptr()) };
    }
}

#[test]
fn another_bindings_reference_moves_without_its_drop() {
    let object = object(false);
    let foreign = Foreign(NonNull::new(raw(&object)).expect("non-null"));
    // SAFETY: `Foreign` is one non-null interface pointer owning one
    // reference, released by its drop and nothing else.
    let com = unsafe { Com::from_interface(foreign) };
    assert_eq!(com.as_raw(), raw(&object));
    drop(com);
    assert_eq!(
        FOREIGN_DROPS.with(Cell::get),
        0,
        "the foreign drop never ran"
    );
    assert_eq!((object.refs.get(), object.releases.get()), (0, 1));
}

#[test]
fn a_failure_names_its_step() {
    assert_eq!(check(0, "step"), Ok(()));
    assert_eq!(check(1, "step"), Ok(()), "S_FALSE is a success");
    assert_eq!(check(E_POINTER, "step"), Err(("step", E_POINTER)));
}
