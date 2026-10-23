// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// HAND-WRITTEN, not generated: `aterm-win32-oracle gen` neither emits nor
// deletes this file, and harvests the generated declarations it names like a
// consumer's. Derived from windows 0.62.2
// src/Windows/Win32/Graphics/Dxgi/mod.rs and src/Windows/Win32/Graphics/Dxgi/Common/mod.rs
// and modified by the aterm project in 2026: windows' newtypes are written
// as windows-sys writes such types (plain aliases), each interface's vtable
// is flattened into one struct that ends at the last slot aterm calls, and
// only those slots are typed. See ../LICENSE-MIT and ../LICENSE-APACHE for
// the upstream licenses.

//! DXGI bindings used by aterm.

use crate::Win32::Foundation::RECT;
use crate::Win32::Graphics::Gdi::HMONITOR;
use crate::com::IUnknown_Vtbl;
use crate::core::{BOOL, GUID, HRESULT};
use core::ffi::c_void;

pub type DXGI_COLOR_SPACE_TYPE = i32;
pub const DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709: DXGI_COLOR_SPACE_TYPE = 1i32;
pub const DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020: DXGI_COLOR_SPACE_TYPE = 12i32;
pub type DXGI_MODE_ROTATION = i32;

pub type DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG = i32;
pub const DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT:
    DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG = 1i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DXGI_OUTPUT_DESC1 {
    pub DeviceName: [u16; 32],
    pub DesktopCoordinates: RECT,
    pub AttachedToDesktop: BOOL,
    pub Rotation: DXGI_MODE_ROTATION,
    pub Monitor: HMONITOR,
    pub BitsPerColor: u32,
    pub ColorSpace: DXGI_COLOR_SPACE_TYPE,
    pub RedPrimary: [f32; 2],
    pub GreenPrimary: [f32; 2],
    pub BluePrimary: [f32; 2],
    pub WhitePoint: [f32; 2],
    pub MinLuminance: f32,
    pub MaxLuminance: f32,
    pub MaxFullFrameLuminance: f32,
}
impl Default for DXGI_OUTPUT_DESC1 {
    fn default() -> Self {
        // SAFETY: integers, floats and a pointer: all-zero is a valid value of each, as upstream's
        // own `Default` has it.
        unsafe { core::mem::zeroed() }
    }
}

link!("dxgi.dll" "system" fn CreateDXGIFactory1(riid : *const crate::core::GUID, ppfactory : *mut *mut core::ffi::c_void) -> crate::core::HRESULT);

pub const IID_IDXGIFactory1: GUID = GUID::from_u128(0x770aae78_f26f_4dba_a829_253c83d1b387);
pub const IID_IDXGIOutput6: GUID = GUID::from_u128(0x068346e8_aaec_4b84_add7_137f513f77a1);

/// `IDXGIFactory1`, through `EnumAdapters1` (slot 12).
#[repr(C)]
pub struct IDXGIFactory1_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: usize,
    pub EnumAdapters: usize,
    pub MakeWindowAssociation: usize,
    pub GetWindowAssociation: usize,
    pub CreateSwapChain: usize,
    pub CreateSoftwareAdapter: usize,
    pub EnumAdapters1: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
}

/// `IDXGIAdapter1`, through `EnumOutputs` (slot 7).
#[repr(C)]
pub struct IDXGIAdapter1_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: usize,
    pub EnumOutputs: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
}

/// `IDXGIOutput6`, through `GetDesc1` (slot 27).
#[repr(C)]
pub struct IDXGIOutput6_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: usize,
    pub GetDesc: usize,
    pub GetDisplayModeList: usize,
    pub FindClosestMatchingMode: usize,
    pub WaitForVBlank: usize,
    pub TakeOwnership: usize,
    pub ReleaseOwnership: usize,
    pub GetGammaControlCapabilities: usize,
    pub SetGammaControl: usize,
    pub GetGammaControl: usize,
    pub SetDisplaySurface: usize,
    pub GetDisplaySurfaceData: usize,
    pub GetFrameStatistics: usize,
    pub GetDisplayModeList1: usize,
    pub FindClosestMatchingMode1: usize,
    pub GetDisplaySurfaceData1: usize,
    pub DuplicateOutput: usize,
    pub SupportsOverlays: usize,
    pub CheckOverlaySupport: usize,
    pub CheckOverlayColorSpaceSupport: usize,
    pub DuplicateOutput1: usize,
    pub GetDesc1: unsafe extern "system" fn(*mut c_void, *mut DXGI_OUTPUT_DESC1) -> HRESULT,
}

/// `IDXGISwapChain3`, through `SetColorSpace1` (slot 38).
#[repr(C)]
pub struct IDXGISwapChain3_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: usize,
    pub GetDevice: usize,
    pub Present: usize,
    pub GetBuffer: usize,
    pub SetFullscreenState: usize,
    pub GetFullscreenState: usize,
    pub GetDesc: usize,
    pub ResizeBuffers: usize,
    pub ResizeTarget: usize,
    pub GetContainingOutput: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub GetFrameStatistics: usize,
    pub GetLastPresentCount: usize,
    pub GetDesc1: usize,
    pub GetFullscreenDesc: usize,
    pub GetHwnd: usize,
    pub GetCoreWindow: usize,
    pub Present1: usize,
    pub IsTemporaryMonoSupported: usize,
    pub GetRestrictToOutput: usize,
    pub SetBackgroundColor: usize,
    pub GetBackgroundColor: usize,
    pub SetRotation: usize,
    pub GetRotation: usize,
    pub SetSourceSize: usize,
    pub GetSourceSize: usize,
    pub SetMaximumFrameLatency: usize,
    pub GetMaximumFrameLatency: usize,
    pub GetFrameLatencyWaitableObject: usize,
    pub SetMatrixTransform: usize,
    pub GetMatrixTransform: usize,
    pub GetCurrentBackBufferIndex: usize,
    pub CheckColorSpaceSupport:
        unsafe extern "system" fn(*mut c_void, DXGI_COLOR_SPACE_TYPE, *mut u32) -> HRESULT,
    pub SetColorSpace1: unsafe extern "system" fn(*mut c_void, DXGI_COLOR_SPACE_TYPE) -> HRESULT,
}
