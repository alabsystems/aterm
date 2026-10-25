// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The scRGB tag's two `IDXGISwapChain3` slots on a live swap chain.
//!
//! aterm-gpu tags an f16 swap chain scRGB through aterm-win32's `CheckColorSpaceSupport`
//! (slot 37) and `SetColorSpace1` (slot 38) only where the window's output is in HDR mode,
//! which no display of the Windows lane is (`dxgi_live` reads colour space 0 on both), so a
//! window never reaches them there. This makes the calls aterm-gpu's tag makes, with the
//! same constants, on an f16 composition swap chain of the default adapter, which needs no
//! window. It needs the desktop's compositor: in session 0 (sshd's) the chain is refused with
//! `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE` and the test ends COULD NOT RUN. Direct3D 11, the
//! chain's own interfaces and their IDs are declared here, not taken from aterm-win32.
//!
//! The answers it holds are DXGI's, measured on the Microsoft Basic Render Driver only: an
//! f16 chain presents and takes scRGB, and refuses HDR10's colour space (flags 0,
//! `E_INVALIDARG`). A real GPU's driver may answer the HDR10 pair otherwise, and a failure
//! there is DXGI's answer, not aterm-win32's slots. Those two calls do not hold
//! `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`'s value, since this chain refuses every space
//! but scRGB alike; the oracle's conformance check holds it.
//!
//! STOCK EXCEPTION: Windows only, and Trust has no Windows std, so only stock Rust builds this (the
//! Windows lane's guest pins its version: `aterm-lab/src/windows_guest.rs`).

#![cfg(windows)]
#![allow(non_snake_case, non_upper_case_globals)]

use aterm_win32::com::Com;
use aterm_win32::core::GUID;
use aterm_win32::dxgi::{
    DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
    DXGI_COLOR_SPACE_TYPE, DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT, IDXGISwapChain3_Vtbl,
};
use core::ffi::c_void;
use core::ptr;

/// Direct3D 11's and DXGI's own declarations, independent of aterm-win32's.
mod d3d {
    use core::ffi::c_void;

    pub const D3D_DRIVER_TYPE_HARDWARE: i32 = 1;
    pub const D3D11_SDK_VERSION: u32 = 7;
    pub const DXGI_FORMAT_R16G16B16A16_FLOAT: i32 = 10;
    pub const DXGI_USAGE_RENDER_TARGET_OUTPUT: u32 = 0x20;
    pub const DXGI_SCALING_STRETCH: i32 = 0;
    pub const DXGI_SWAP_EFFECT_FLIP_DISCARD: i32 = 4;
    pub const DXGI_ALPHA_MODE_PREMULTIPLIED: i32 = 1;
    pub const DXGI_ERROR_NOT_CURRENTLY_AVAILABLE: i32 = 0x887a_0022_u32 as i32;

    /// `DXGI_SWAP_CHAIN_DESC1`.
    #[repr(C)]
    pub struct SwapChainDesc1 {
        pub width: u32,
        pub height: u32,
        pub format: i32,
        pub stereo: i32,
        pub sample_count: u32,
        pub sample_quality: u32,
        pub buffer_usage: u32,
        pub buffer_count: u32,
        pub scaling: i32,
        pub swap_effect: i32,
        pub alpha_mode: i32,
        pub flags: u32,
    }

    /// `IDXGIObject::GetParent`, slot 6.
    pub type GetParent =
        unsafe extern "system" fn(*mut c_void, *const super::GUID, *mut *mut c_void) -> i32;
    /// `IDXGIDevice::GetAdapter`, slot 7.
    pub type GetAdapter = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
    /// `IDXGIFactory2::CreateSwapChainForComposition`, slot 24.
    pub type CreateSwapChainForComposition = unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const SwapChainDesc1,
        *mut c_void,
        *mut *mut c_void,
    ) -> i32;

    #[link(name = "d3d11")]
    unsafe extern "system" {
        pub fn D3D11CreateDevice(
            adapter: *mut c_void,
            driver_type: i32,
            software: *mut c_void,
            flags: u32,
            feature_levels: *const i32,
            feature_level_count: u32,
            sdk_version: u32,
            device: *mut *mut c_void,
            feature_level: *mut i32,
            context: *mut *mut c_void,
        ) -> i32;
    }
}

const IID_IDXGIDevice: GUID = GUID::from_u128(0x54ec77fa_1377_44e6_8c32_88fd5f44c84c);
const IID_IDXGIFactory2: GUID = GUID::from_u128(0x50c83a1c_e072_4c48_87b0_3630fa36a6d0);
const IID_IDXGISwapChain3: GUID = GUID::from_u128(0x94d99bdb_f1f8_4ab0_b236_7da0170edab1);

/// The reference a successful call wrote to `out`, owned.
///
/// # Safety
///
/// `out` is what the call whose result is `hr` wrote.
unsafe fn owned(hr: i32, out: *mut c_void) -> Option<Com> {
    // SAFETY: The caller's contract.
    (hr >= 0).then(|| unsafe { Com::from_raw(out) }).flatten()
}

/// Slot `n` of `object`'s vtable, as `F`.
///
/// # Safety
///
/// `object` is a live interface pointer whose vtable has a slot `n` of type `F`.
unsafe fn slot<F: Copy>(object: &Com, n: usize) -> F {
    // SAFETY: A COM object begins with its vtable pointer; the caller vouches for slot `n`.
    unsafe {
        let vtable = *object.as_raw().cast::<*const usize>();
        core::mem::transmute_copy::<usize, F>(&*vtable.add(n))
    }
}

/// An f16 composition swap chain of the default adapter's Direct3D 11 device, as its
/// `IDXGISwapChain3`.
fn f16_chain() -> Com {
    let mut out = ptr::null_mut();
    // SAFETY: The default adapter, no software module, the default feature levels, and one
    // out pointer written on success.
    let hr = unsafe {
        d3d::D3D11CreateDevice(
            ptr::null_mut(),
            d3d::D3D_DRIVER_TYPE_HARDWARE,
            ptr::null_mut(),
            0,
            ptr::null(),
            0,
            d3d::D3D11_SDK_VERSION,
            &mut out,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    // SAFETY: `out` is that call's.
    let device = unsafe { owned(hr, out) }.unwrap_or_else(|| panic!("D3D11CreateDevice: {hr:#x}"));
    let dxgi_device = device.query(&IID_IDXGIDevice).expect("an IDXGIDevice");
    let mut out = ptr::null_mut();
    // SAFETY: An IDXGIDevice's slot 7; one owned IDXGIAdapter on success.
    let hr = unsafe { slot::<d3d::GetAdapter>(&dxgi_device, 7)(dxgi_device.as_raw(), &mut out) };
    // SAFETY: `out` is that call's.
    let adapter = unsafe { owned(hr, out) }.unwrap_or_else(|| panic!("GetAdapter: {hr:#x}"));
    let mut out = ptr::null_mut();
    // SAFETY: An IDXGIAdapter's slot 6; one owned IDXGIFactory2 on success.
    let hr = unsafe {
        slot::<d3d::GetParent>(&adapter, 6)(adapter.as_raw(), &IID_IDXGIFactory2, &mut out)
    };
    // SAFETY: `out` is that call's.
    let factory = unsafe { owned(hr, out) }.unwrap_or_else(|| panic!("GetParent: {hr:#x}"));
    let desc = d3d::SwapChainDesc1 {
        width: 64,
        height: 64,
        format: d3d::DXGI_FORMAT_R16G16B16A16_FLOAT,
        stereo: 0,
        sample_count: 1,
        sample_quality: 0,
        buffer_usage: d3d::DXGI_USAGE_RENDER_TARGET_OUTPUT,
        buffer_count: 2,
        scaling: d3d::DXGI_SCALING_STRETCH,
        swap_effect: d3d::DXGI_SWAP_EFFECT_FLIP_DISCARD,
        alpha_mode: d3d::DXGI_ALPHA_MODE_PREMULTIPLIED,
        flags: 0,
    };
    let mut out = ptr::null_mut();
    // SAFETY: An IDXGIFactory2's slot 24, the live device, a filled description, no output to
    // restrict to; one owned IDXGISwapChain1 on success.
    let hr = unsafe {
        slot::<d3d::CreateSwapChainForComposition>(&factory, 24)(
            factory.as_raw(),
            device.as_raw(),
            &desc,
            ptr::null_mut(),
            &mut out,
        )
    };
    if hr == d3d::DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
        panic!(
            "aterm-gate: COULD NOT RUN — no desktop compositor here (session 0), so no \
             composition swap chain; run it in a desktop session (`lab windows exec --desktop`)"
        );
    }
    // SAFETY: `out` is that call's.
    let chain = unsafe { owned(hr, out) }
        .unwrap_or_else(|| panic!("CreateSwapChainForComposition: {hr:#x}"));
    chain
        .query(&IID_IDXGISwapChain3)
        .expect("Windows 11's flip chains are IDXGISwapChain3")
}

/// `CheckColorSpaceSupport` through aterm-win32's slot: its result and the flags it wrote,
/// `u32::MAX` where it wrote none.
fn support(chain: &Com, space: DXGI_COLOR_SPACE_TYPE) -> (i32, u32) {
    let mut flags = u32::MAX;
    // SAFETY: `chain` is an IDXGISwapChain3; `flags` is written on success.
    let hr = unsafe {
        (chain
            .vtable::<IDXGISwapChain3_Vtbl>()
            .CheckColorSpaceSupport)(chain.as_raw(), space, &mut flags)
    };
    (hr, flags)
}

/// `SetColorSpace1` through aterm-win32's slot.
fn set(chain: &Com, space: DXGI_COLOR_SPACE_TYPE) -> i32 {
    // SAFETY: `chain` is an IDXGISwapChain3.
    unsafe { (chain.vtable::<IDXGISwapChain3_Vtbl>().SetColorSpace1)(chain.as_raw(), space) }
}

#[test]
fn an_f16_chain_takes_scrgb_and_refuses_hdr10_through_aterm_win32s_slots() {
    let chain = f16_chain();
    let present = DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT as u32;

    // Each answer is held before the next call, so a wrong slot stops at the first call it
    // reaches instead of handing a later one arguments of another slot's shape.
    let (hr, flags) = support(&chain, DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709);
    eprintln!("CheckColorSpaceSupport(G10_NONE_P709): {hr:#x}, flags {flags:#x}");
    assert!(hr >= 0 && flags != u32::MAX, "the support check answered");
    assert_ne!(flags & present, 0, "an f16 chain presents scRGB");

    let (hr, flags) = support(&chain, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
    eprintln!("CheckColorSpaceSupport(G2084_NONE_P2020): {hr:#x}, flags {flags:#x}");
    assert!(hr >= 0 && flags != u32::MAX, "the support check answered");
    assert_eq!(
        flags & present,
        0,
        "HDR10's colour space needs 10-bit buffers"
    );

    let hr = set(&chain, DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
    eprintln!("SetColorSpace1(G2084_NONE_P2020): {hr:#x}");
    assert!(
        hr < 0,
        "SetColorSpace1 refuses a space the chain cannot present"
    );

    let hr = set(&chain, DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709);
    eprintln!("SetColorSpace1(G10_NONE_P709): {hr:#x}");
    assert!(hr >= 0, "SetColorSpace1 takes scRGB");
}
