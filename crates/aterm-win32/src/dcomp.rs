// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// HAND-WRITTEN, not generated: `aterm-win32-oracle gen` neither emits nor
// deletes this file, and harvests the generated declarations it names like a
// consumer's. Derived from windows 0.62.2
// src/Windows/Win32/Graphics/DirectComposition/mod.rs and modified by the
// aterm project in 2026: each interface's vtable is flattened into one struct
// that ends at the last slot aterm calls, and only those slots are typed. See
// ../LICENSE-MIT and ../LICENSE-APACHE for the upstream licenses.

//! DirectComposition, the slice aterm-gpu's D3D11 arm calls for a window's
//! composition swap chain (GW.5 of docs/AUDIT-third-party-2026-10-15.md): a
//! device on the D3D11 device's DXGI device ([`DCompositionCreateDevice`]),
//! a target on the window, one visual whose content is the swap chain, and
//! the commit. Per-pixel alpha then blends over DWM's Mica/Acrylic backdrop,
//! which an HWND swap chain's opaque composite covers.
//!
//! As in [`crate::dxgi`], each vtable names every slot up to the last one
//! called, with its upstream name, and only the called slots are typed;
//! win32-oracle/conformance/src/com.rs holds them to `windows` 0.62.2.

use crate::Win32::Foundation::HWND;
use crate::com::IUnknown_Vtbl;
use crate::core::{BOOL, GUID, HRESULT};
use core::ffi::c_void;

link!("dcomp.dll" "system" fn DCompositionCreateDevice(dxgidevice : *mut core::ffi::c_void, iid : *const crate::core::GUID, dcompositiondevice : *mut *mut core::ffi::c_void) -> crate::core::HRESULT);

pub const IID_IDCompositionDevice: GUID = GUID::from_u128(0xc37ea93a_e7aa_450d_b16f_9746cb0407f3);
pub const IID_IDCompositionTarget: GUID = GUID::from_u128(0xeacdd04c_117e_4e17_88f4_d1b12b0e3d89);
pub const IID_IDCompositionVisual: GUID = GUID::from_u128(0x4d93059d_097b_4651_9a60_f0f25116e2f3);

/// `IDCompositionDevice`, through `CreateVisual` (slot 8).
#[repr(C)]
pub struct IDCompositionDevice_Vtbl {
    pub base__: IUnknown_Vtbl,
    // IDCompositionDevice
    pub Commit: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub WaitForCommitCompletion: usize,
    pub GetFrameStatistics: usize,
    pub CreateTargetForHwnd:
        unsafe extern "system" fn(*mut c_void, HWND, BOOL, *mut *mut c_void) -> HRESULT,
    pub CreateVisual: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

/// `IDCompositionTarget`, through `SetRoot` (slot 3).
#[repr(C)]
pub struct IDCompositionTarget_Vtbl {
    pub base__: IUnknown_Vtbl,
    // IDCompositionTarget
    pub SetRoot: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

/// `IDCompositionVisual`, through `SetContent` (slot 16).
#[repr(C)]
pub struct IDCompositionVisual_Vtbl {
    pub base__: IUnknown_Vtbl,
    // IDCompositionVisual
    pub SetOffsetX: usize,
    pub SetOffsetX2: usize,
    pub SetOffsetY: usize,
    pub SetOffsetY2: usize,
    pub SetTransform: usize,
    pub SetTransform2: usize,
    pub SetTransformParent: usize,
    pub SetEffect: usize,
    pub SetBitmapInterpolationMode: usize,
    pub SetBorderMode: usize,
    pub SetClip: usize,
    pub SetClip2: usize,
    pub SetContent: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}
