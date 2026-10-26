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

use crate::Win32::Foundation::{HANDLE, HWND, LUID, RECT};
use crate::Win32::Graphics::Gdi::HMONITOR;
use crate::com::IUnknown_Vtbl;
use crate::core::{BOOL, GUID, HRESULT};
use core::ffi::c_void;

pub type DXGI_COLOR_SPACE_TYPE = i32;
pub const DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709: DXGI_COLOR_SPACE_TYPE = 1i32;
pub const DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020: DXGI_COLOR_SPACE_TYPE = 12i32;
pub type DXGI_MODE_ROTATION = i32;
pub type DXGI_FORMAT = i32;
pub const DXGI_FORMAT_UNKNOWN: DXGI_FORMAT = 0i32;
pub const DXGI_FORMAT_R32G32B32A32_FLOAT: DXGI_FORMAT = 2i32;
pub const DXGI_FORMAT_R16G16B16A16_FLOAT: DXGI_FORMAT = 10i32;
pub const DXGI_FORMAT_R16G16B16A16_UINT: DXGI_FORMAT = 12i32;
pub const DXGI_FORMAT_R10G10B10A2_UNORM: DXGI_FORMAT = 24i32;
pub const DXGI_FORMAT_R8G8B8A8_TYPELESS: DXGI_FORMAT = 27i32;
pub const DXGI_FORMAT_R8G8B8A8_UNORM: DXGI_FORMAT = 28i32;
pub const DXGI_FORMAT_R8G8B8A8_UNORM_SRGB: DXGI_FORMAT = 29i32;
pub const DXGI_FORMAT_R8G8B8A8_UINT: DXGI_FORMAT = 30i32;
pub const DXGI_FORMAT_R16G16_UINT: DXGI_FORMAT = 36i32;
pub const DXGI_FORMAT_R16G16_SINT: DXGI_FORMAT = 38i32;
pub const DXGI_FORMAT_R32_TYPELESS: DXGI_FORMAT = 39i32;
pub const DXGI_FORMAT_R32_UINT: DXGI_FORMAT = 42i32;
pub const DXGI_FORMAT_R8_UNORM: DXGI_FORMAT = 61i32;
pub const DXGI_FORMAT_B8G8R8A8_UNORM: DXGI_FORMAT = 87i32;
pub const DXGI_FORMAT_B8G8R8A8_TYPELESS: DXGI_FORMAT = 90i32;
pub const DXGI_FORMAT_B8G8R8A8_UNORM_SRGB: DXGI_FORMAT = 91i32;
pub type DXGI_ALPHA_MODE = i32;
pub const DXGI_ALPHA_MODE_UNSPECIFIED: DXGI_ALPHA_MODE = 0i32;
pub const DXGI_ALPHA_MODE_PREMULTIPLIED: DXGI_ALPHA_MODE = 1i32;
pub const DXGI_ALPHA_MODE_STRAIGHT: DXGI_ALPHA_MODE = 2i32;
pub const DXGI_ALPHA_MODE_IGNORE: DXGI_ALPHA_MODE = 3i32;
pub type DXGI_MODE_SCANLINE_ORDER = i32;
pub type DXGI_MODE_SCALING = i32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DXGI_SAMPLE_DESC {
    pub Count: u32,
    pub Quality: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DXGI_RATIONAL {
    pub Numerator: u32,
    pub Denominator: u32,
}

pub type DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG = i32;
pub const DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG_PRESENT:
    DXGI_SWAP_CHAIN_COLOR_SPACE_SUPPORT_FLAG = 1i32;
pub type DXGI_USAGE = u32;
pub const DXGI_USAGE_SHADER_INPUT: DXGI_USAGE = 16u32;
pub const DXGI_USAGE_RENDER_TARGET_OUTPUT: DXGI_USAGE = 32u32;
pub type DXGI_SCALING = i32;
pub const DXGI_SCALING_STRETCH: DXGI_SCALING = 0i32;
pub const DXGI_SCALING_NONE: DXGI_SCALING = 1i32;
pub type DXGI_SWAP_EFFECT = i32;
pub const DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL: DXGI_SWAP_EFFECT = 3i32;
pub const DXGI_SWAP_EFFECT_FLIP_DISCARD: DXGI_SWAP_EFFECT = 4i32;
pub type DXGI_SWAP_CHAIN_FLAG = i32;
pub const DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT: DXGI_SWAP_CHAIN_FLAG = 64i32;
pub type DXGI_PRESENT = u32;
pub type DXGI_MWA_FLAGS = u32;
pub const DXGI_MWA_NO_WINDOW_CHANGES: DXGI_MWA_FLAGS = 1u32;
pub const DXGI_MWA_NO_ALT_ENTER: DXGI_MWA_FLAGS = 2u32;
pub type DXGI_GPU_PREFERENCE = i32;
pub const DXGI_GPU_PREFERENCE_UNSPECIFIED: DXGI_GPU_PREFERENCE = 0i32;
pub const DXGI_GPU_PREFERENCE_MINIMUM_POWER: DXGI_GPU_PREFERENCE = 1i32;
pub const DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE: DXGI_GPU_PREFERENCE = 2i32;
pub type DXGI_ADAPTER_FLAG = i32;
pub const DXGI_ADAPTER_FLAG_SOFTWARE: DXGI_ADAPTER_FLAG = 2i32;
pub const DXGI_ERROR_NOT_FOUND: HRESULT = 0x887A_0002_u32 as _;
pub const DXGI_ERROR_DEVICE_REMOVED: HRESULT = 0x887A_0005_u32 as _;
pub const DXGI_ERROR_DEVICE_HUNG: HRESULT = 0x887A_0006_u32 as _;
pub const DXGI_ERROR_DEVICE_RESET: HRESULT = 0x887A_0007_u32 as _;
pub const DXGI_ERROR_WAS_STILL_DRAWING: HRESULT = 0x887A_000A_u32 as _;
pub const DXGI_ERROR_DRIVER_INTERNAL_ERROR: HRESULT = 0x887A_0020_u32 as _;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DXGI_SWAP_CHAIN_DESC1 {
    pub Width: u32,
    pub Height: u32,
    pub Format: DXGI_FORMAT,
    pub Stereo: BOOL,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub BufferUsage: DXGI_USAGE,
    pub BufferCount: u32,
    pub Scaling: DXGI_SCALING,
    pub SwapEffect: DXGI_SWAP_EFFECT,
    pub AlphaMode: DXGI_ALPHA_MODE,
    pub Flags: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DXGI_SWAP_CHAIN_FULLSCREEN_DESC {
    pub RefreshRate: DXGI_RATIONAL,
    pub ScanlineOrdering: DXGI_MODE_SCANLINE_ORDER,
    pub Scaling: DXGI_MODE_SCALING,
    pub Windowed: BOOL,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DXGI_ADAPTER_DESC1 {
    pub Description: [u16; 128],
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DedicatedVideoMemory: usize,
    pub DedicatedSystemMemory: usize,
    pub SharedSystemMemory: usize,
    pub AdapterLuid: LUID,
    pub Flags: u32,
}
impl Default for DXGI_ADAPTER_DESC1 {
    fn default() -> Self {
        // SAFETY: integers only: all-zero is a valid value of each, as
        // upstream's own `Default` has it.
        unsafe { core::mem::zeroed() }
    }
}

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
pub const IID_IDXGIAdapter1: GUID = GUID::from_u128(0x29038f61_3839_4626_91fd_086879011a05);
pub const IID_IDXGIDevice: GUID = GUID::from_u128(0x54ec77fa_1377_44e6_8c32_88fd5f44c84c);
pub const IID_IDXGIFactory2: GUID = GUID::from_u128(0x50c83a1c_e072_4c48_87b0_3630fa36a6d0);
pub const IID_IDXGIFactory6: GUID = GUID::from_u128(0xc1b6694f_ff09_44a9_b03c_77900a0a1d17);
pub const IID_IDXGISwapChain1: GUID = GUID::from_u128(0x790a45f7_0d42_4876_983a_0a55cfe6f4aa);
pub const IID_IDXGISwapChain3: GUID = GUID::from_u128(0x94d99bdb_f1f8_4ab0_b236_7da0170edab1);

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

/// `IDXGIAdapter1`, through `GetDesc1` (slot 10).
#[repr(C)]
pub struct IDXGIAdapter1_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub EnumOutputs: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    pub GetDesc: usize,
    pub CheckInterfaceSupport: usize,
    // IDXGIAdapter1
    pub GetDesc1: unsafe extern "system" fn(*mut c_void, *mut DXGI_ADAPTER_DESC1) -> HRESULT,
}

/// `IDXGIDevice`, through `GetAdapter` (slot 7).
#[repr(C)]
pub struct IDXGIDevice_Vtbl {
    pub base__: IUnknown_Vtbl,
    // IDXGIObject
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: usize,
    // IDXGIDevice
    pub GetAdapter: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

/// `IDXGIFactory2`, through `CreateSwapChainForComposition` (slot 24).
#[repr(C)]
pub struct IDXGIFactory2_Vtbl {
    pub base__: IUnknown_Vtbl,
    // IDXGIObject
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: usize,
    // IDXGIFactory
    pub EnumAdapters: usize,
    pub MakeWindowAssociation:
        unsafe extern "system" fn(*mut c_void, HWND, DXGI_MWA_FLAGS) -> HRESULT,
    pub GetWindowAssociation: usize,
    pub CreateSwapChain: usize,
    pub CreateSoftwareAdapter: usize,
    // IDXGIFactory1
    pub EnumAdapters1: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> HRESULT,
    pub IsCurrent: usize,
    // IDXGIFactory2
    pub IsWindowedStereoEnabled: usize,
    pub CreateSwapChainForHwnd: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        HWND,
        *const DXGI_SWAP_CHAIN_DESC1,
        *const DXGI_SWAP_CHAIN_FULLSCREEN_DESC,
        *mut c_void,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateSwapChainForCoreWindow: usize,
    pub GetSharedResourceAdapterLuid: usize,
    pub RegisterStereoStatusWindow: usize,
    pub RegisterStereoStatusEvent: usize,
    pub UnregisterStereoStatus: usize,
    pub RegisterOcclusionStatusWindow: usize,
    pub RegisterOcclusionStatusEvent: usize,
    pub UnregisterOcclusionStatus: usize,
    pub CreateSwapChainForComposition: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const DXGI_SWAP_CHAIN_DESC1,
        *mut c_void,
        *mut *mut c_void,
    ) -> HRESULT,
}

/// `IDXGIFactory6`, through `EnumAdapterByGpuPreference` (slot 29).
#[repr(C)]
pub struct IDXGIFactory6_Vtbl {
    pub base__: IUnknown_Vtbl,
    // IDXGIObject
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetPrivateData: usize,
    pub GetParent: usize,
    // IDXGIFactory
    pub EnumAdapters: usize,
    pub MakeWindowAssociation: usize,
    pub GetWindowAssociation: usize,
    pub CreateSwapChain: usize,
    pub CreateSoftwareAdapter: usize,
    // IDXGIFactory1
    pub EnumAdapters1: usize,
    pub IsCurrent: usize,
    // IDXGIFactory2
    pub IsWindowedStereoEnabled: usize,
    pub CreateSwapChainForHwnd: usize,
    pub CreateSwapChainForCoreWindow: usize,
    pub GetSharedResourceAdapterLuid: usize,
    pub RegisterStereoStatusWindow: usize,
    pub RegisterStereoStatusEvent: usize,
    pub UnregisterStereoStatus: usize,
    pub RegisterOcclusionStatusWindow: usize,
    pub RegisterOcclusionStatusEvent: usize,
    pub UnregisterOcclusionStatus: usize,
    pub CreateSwapChainForComposition: usize,
    // IDXGIFactory3
    pub GetCreationFlags: usize,
    // IDXGIFactory4
    pub EnumAdapterByLuid: usize,
    pub EnumWarpAdapter:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    // IDXGIFactory5
    pub CheckFeatureSupport: usize,
    // IDXGIFactory6
    pub EnumAdapterByGpuPreference: unsafe extern "system" fn(
        *mut c_void,
        u32,
        DXGI_GPU_PREFERENCE,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
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
    pub Present: unsafe extern "system" fn(*mut c_void, u32, DXGI_PRESENT) -> HRESULT,
    pub GetBuffer:
        unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut *mut c_void) -> HRESULT,
    pub SetFullscreenState: usize,
    pub GetFullscreenState: usize,
    pub GetDesc: usize,
    pub ResizeBuffers:
        unsafe extern "system" fn(*mut c_void, u32, u32, u32, DXGI_FORMAT, u32) -> HRESULT,
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
    pub SetMaximumFrameLatency: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    pub GetMaximumFrameLatency: usize,
    pub GetFrameLatencyWaitableObject: unsafe extern "system" fn(*mut c_void) -> HANDLE,
    pub SetMatrixTransform: usize,
    pub GetMatrixTransform: usize,
    pub GetCurrentBackBufferIndex: usize,
    pub CheckColorSpaceSupport:
        unsafe extern "system" fn(*mut c_void, DXGI_COLOR_SPACE_TYPE, *mut u32) -> HRESULT,
    pub SetColorSpace1: unsafe extern "system" fn(*mut c_void, DXGI_COLOR_SPACE_TYPE) -> HRESULT,
}
