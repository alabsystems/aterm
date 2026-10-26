// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// HAND-WRITTEN, not generated: `aterm-win32-oracle gen` neither emits nor
// deletes this file, and harvests the generated declarations it names like a
// consumer's. Derived from windows 0.62.2
// src/Windows/Win32/Graphics/Direct3D/mod.rs,
// src/Windows/Win32/Graphics/Direct3D/Fxc/mod.rs and
// src/Windows/Win32/Graphics/Direct3D11/mod.rs and modified by the aterm
// project in 2026: windows' newtypes are written as windows-sys writes such
// types (plain aliases), each interface's vtable is flattened into one struct
// that ends at the last slot aterm calls, only those slots are typed, and
// each view description's union carries only the members aterm writes and
// the largest, which sizes it. See ../LICENSE-MIT and ../LICENSE-APACHE for
// the upstream licenses.

//! Direct3D 11, the slice aterm-gpu's D3D11 arm calls (GW.5 of
//! docs/AUDIT-third-party-2026-10-15.md): the device and its immediate
//! context ([`D3D11CreateDevice`]), the resources, views and fixed-function
//! states a frame binds, and the HLSL compiler's entry point
//! ([`pD3DCompile`], which `d3dcompiler_47.dll` exports and aterm-gpu loads
//! at run time) with its blob.
//!
//! windows-sys carries no COM interfaces, so these are `windows` 0.62.2's
//! declarations. A wrong slot is silent undefined behaviour, so each vtable
//! names every slot up to the last one called, with its upstream name; the
//! slots aterm calls are typed, the others are `usize`.
//! win32-oracle/conformance/src/com.rs holds every slot's offset, every typed
//! slot's fn type, each layout, constant and interface ID to `windows` 0.62.2
//! on both Windows triples, and `aterm-win32-oracle oracle` plants mismatches
//! it must catch; tests/com.rs holds the 64-bit answers on any host.

use crate::Win32::Foundation::RECT;
use crate::com::IUnknown_Vtbl;
use crate::core::{BOOL, GUID, HRESULT, PCSTR};
use crate::dxgi::DXGI_FORMAT;
use core::ffi::c_void;

// --- Direct3D ---------------------------------------------------------------

pub type D3D_DRIVER_TYPE = i32;
pub const D3D_DRIVER_TYPE_UNKNOWN: D3D_DRIVER_TYPE = 0i32;
pub const D3D_DRIVER_TYPE_HARDWARE: D3D_DRIVER_TYPE = 1i32;
pub const D3D_DRIVER_TYPE_WARP: D3D_DRIVER_TYPE = 5i32;
pub type D3D_FEATURE_LEVEL = i32;
pub const D3D_FEATURE_LEVEL_10_0: D3D_FEATURE_LEVEL = 40960i32;
pub const D3D_FEATURE_LEVEL_11_0: D3D_FEATURE_LEVEL = 45056i32;
pub const D3D_FEATURE_LEVEL_11_1: D3D_FEATURE_LEVEL = 45312i32;
pub type D3D_PRIMITIVE_TOPOLOGY = i32;
pub const D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST: D3D_PRIMITIVE_TOPOLOGY = 4i32;
pub const D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP: D3D_PRIMITIVE_TOPOLOGY = 5i32;
pub type D3D_SRV_DIMENSION = i32;
pub const D3D_SRV_DIMENSION_TEXTURE2D: D3D_SRV_DIMENSION = 4i32;
pub const D3D_SRV_DIMENSION_BUFFEREX: D3D_SRV_DIMENSION = 11i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D_SHADER_MACRO {
    pub Name: PCSTR,
    pub Definition: PCSTR,
}

pub const IID_ID3DBlob: GUID = GUID::from_u128(0x8ba5fb08_5195_40e2_ac58_0d989c3a0102);

/// `ID3DBlob` (`ID3D10Blob`), through `GetBufferSize` (slot 4).
#[repr(C)]
pub struct ID3DBlob_Vtbl {
    pub base__: IUnknown_Vtbl,
    // ID3DBlob
    pub GetBufferPointer: unsafe extern "system" fn(*mut c_void) -> *mut c_void,
    pub GetBufferSize: unsafe extern "system" fn(*mut c_void) -> usize,
}

// --- Direct3D::Fxc ----------------------------------------------------------

pub const D3DCOMPILE_DEBUG: u32 = 1u32;
pub const D3DCOMPILE_SKIP_OPTIMIZATION: u32 = 4u32;
pub const D3DCOMPILE_ENABLE_STRICTNESS: u32 = 2048u32;

/// `D3DCompile`, as `d3dcompiler_47.dll` exports it: the fn type of
/// upstream's `pD3DCompile`, whose include and blob parameters are each one
/// interface pointer.
pub type pD3DCompile = Option<
    unsafe extern "system" fn(
        psrcdata: *const c_void,
        srcdatasize: usize,
        pfilename: PCSTR,
        pdefines: *const D3D_SHADER_MACRO,
        pinclude: *mut c_void,
        pentrypoint: PCSTR,
        ptarget: PCSTR,
        flags1: u32,
        flags2: u32,
        ppcode: *mut *mut c_void,
        pperrormsgs: *mut *mut c_void,
    ) -> HRESULT,
>;

// --- Direct3D11 -------------------------------------------------------------

pub const D3D11_SDK_VERSION: u32 = 7u32;
pub const D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION: u32 = 16384u32;
pub type D3D11_CREATE_DEVICE_FLAG = u32;
pub const D3D11_CREATE_DEVICE_SINGLETHREADED: D3D11_CREATE_DEVICE_FLAG = 1u32;
pub const D3D11_CREATE_DEVICE_DEBUG: D3D11_CREATE_DEVICE_FLAG = 2u32;
pub const D3D11_CREATE_DEVICE_BGRA_SUPPORT: D3D11_CREATE_DEVICE_FLAG = 32u32;
pub type D3D11_USAGE = i32;
pub const D3D11_USAGE_DEFAULT: D3D11_USAGE = 0i32;
pub const D3D11_USAGE_IMMUTABLE: D3D11_USAGE = 1i32;
pub const D3D11_USAGE_DYNAMIC: D3D11_USAGE = 2i32;
pub const D3D11_USAGE_STAGING: D3D11_USAGE = 3i32;
pub type D3D11_BIND_FLAG = i32;
pub const D3D11_BIND_VERTEX_BUFFER: D3D11_BIND_FLAG = 1i32;
pub const D3D11_BIND_CONSTANT_BUFFER: D3D11_BIND_FLAG = 4i32;
pub const D3D11_BIND_SHADER_RESOURCE: D3D11_BIND_FLAG = 8i32;
pub const D3D11_BIND_RENDER_TARGET: D3D11_BIND_FLAG = 32i32;
pub type D3D11_CPU_ACCESS_FLAG = i32;
pub const D3D11_CPU_ACCESS_WRITE: D3D11_CPU_ACCESS_FLAG = 65536i32;
pub const D3D11_CPU_ACCESS_READ: D3D11_CPU_ACCESS_FLAG = 131072i32;
pub type D3D11_RESOURCE_MISC_FLAG = i32;
pub const D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS: D3D11_RESOURCE_MISC_FLAG = 32i32;
pub type D3D11_BUFFEREX_SRV_FLAG = i32;
pub const D3D11_BUFFEREX_SRV_FLAG_RAW: D3D11_BUFFEREX_SRV_FLAG = 1i32;
pub type D3D11_RTV_DIMENSION = i32;
pub const D3D11_RTV_DIMENSION_TEXTURE2D: D3D11_RTV_DIMENSION = 4i32;
pub type D3D11_MAP = i32;
pub const D3D11_MAP_READ: D3D11_MAP = 1i32;
pub const D3D11_MAP_WRITE_DISCARD: D3D11_MAP = 4i32;
pub type D3D11_INPUT_CLASSIFICATION = i32;
pub const D3D11_INPUT_PER_VERTEX_DATA: D3D11_INPUT_CLASSIFICATION = 0i32;
pub const D3D11_INPUT_PER_INSTANCE_DATA: D3D11_INPUT_CLASSIFICATION = 1i32;
pub type D3D11_BLEND = i32;
pub const D3D11_BLEND_ZERO: D3D11_BLEND = 1i32;
pub const D3D11_BLEND_ONE: D3D11_BLEND = 2i32;
pub const D3D11_BLEND_INV_SRC_COLOR: D3D11_BLEND = 4i32;
pub const D3D11_BLEND_SRC_ALPHA: D3D11_BLEND = 5i32;
pub const D3D11_BLEND_INV_SRC_ALPHA: D3D11_BLEND = 6i32;
pub type D3D11_BLEND_OP = i32;
pub const D3D11_BLEND_OP_ADD: D3D11_BLEND_OP = 1i32;
pub type D3D11_COLOR_WRITE_ENABLE = i32;
pub const D3D11_COLOR_WRITE_ENABLE_RED: D3D11_COLOR_WRITE_ENABLE = 1i32;
pub const D3D11_COLOR_WRITE_ENABLE_GREEN: D3D11_COLOR_WRITE_ENABLE = 2i32;
pub const D3D11_COLOR_WRITE_ENABLE_BLUE: D3D11_COLOR_WRITE_ENABLE = 4i32;
pub const D3D11_COLOR_WRITE_ENABLE_ALPHA: D3D11_COLOR_WRITE_ENABLE = 8i32;
pub const D3D11_COLOR_WRITE_ENABLE_ALL: D3D11_COLOR_WRITE_ENABLE = 15i32;
pub type D3D11_FILL_MODE = i32;
pub const D3D11_FILL_SOLID: D3D11_FILL_MODE = 3i32;
pub type D3D11_CULL_MODE = i32;
pub const D3D11_CULL_NONE: D3D11_CULL_MODE = 1i32;
pub type D3D11_FILTER = i32;
pub const D3D11_FILTER_MIN_MAG_MIP_POINT: D3D11_FILTER = 0i32;
pub const D3D11_FILTER_MIN_MAG_LINEAR_MIP_POINT: D3D11_FILTER = 20i32;
pub type D3D11_TEXTURE_ADDRESS_MODE = i32;
pub const D3D11_TEXTURE_ADDRESS_CLAMP: D3D11_TEXTURE_ADDRESS_MODE = 3i32;
pub type D3D11_COMPARISON_FUNC = i32;
pub const D3D11_COMPARISON_NEVER: D3D11_COMPARISON_FUNC = 1i32;
pub type D3D11_QUERY = i32;
pub const D3D11_QUERY_EVENT: D3D11_QUERY = 0i32;
pub type D3D11_ASYNC_GETDATA_FLAG = i32;
pub const D3D11_ASYNC_GETDATA_DONOTFLUSH: D3D11_ASYNC_GETDATA_FLAG = 1i32;
pub type D3D11_FORMAT_SUPPORT = i32;
pub const D3D11_FORMAT_SUPPORT_RENDER_TARGET: D3D11_FORMAT_SUPPORT = 16384i32;
pub const D3D11_FORMAT_SUPPORT_BLENDABLE: D3D11_FORMAT_SUPPORT = 32768i32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_BUFFER_DESC {
    pub ByteWidth: u32,
    pub Usage: D3D11_USAGE,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
    pub StructureByteStride: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_TEXTURE2D_DESC {
    pub Width: u32,
    pub Height: u32,
    pub MipLevels: u32,
    pub ArraySize: u32,
    pub Format: DXGI_FORMAT,
    pub SampleDesc: crate::dxgi::DXGI_SAMPLE_DESC,
    pub Usage: D3D11_USAGE,
    pub BindFlags: u32,
    pub CPUAccessFlags: u32,
    pub MiscFlags: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D11_SUBRESOURCE_DATA {
    pub pSysMem: *const c_void,
    pub SysMemPitch: u32,
    pub SysMemSlicePitch: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_TEX2D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_TEX2D_ARRAY_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_BUFFEREX_SRV {
    pub FirstElement: u32,
    pub NumElements: u32,
    pub Flags: u32,
}
/// Upstream's union has eleven members; these three are the ones aterm
/// writes and the largest (`Texture2DArray`, 16 bytes), which sizes it.
#[repr(C)]
#[derive(Clone, Copy)]
pub union D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
    pub Texture2D: D3D11_TEX2D_SRV,
    pub Texture2DArray: D3D11_TEX2D_ARRAY_SRV,
    pub BufferEx: D3D11_BUFFEREX_SRV,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D11_SHADER_RESOURCE_VIEW_DESC {
    pub Format: DXGI_FORMAT,
    pub ViewDimension: D3D_SRV_DIMENSION,
    pub Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_TEX2D_RTV {
    pub MipSlice: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_TEX2D_ARRAY_RTV {
    pub MipSlice: u32,
    pub FirstArraySlice: u32,
    pub ArraySize: u32,
}
/// Upstream's union has eight members; these two are the one aterm writes
/// and the largest (`Texture2DArray`, 12 bytes), which sizes it.
#[repr(C)]
#[derive(Clone, Copy)]
pub union D3D11_RENDER_TARGET_VIEW_DESC_0 {
    pub Texture2D: D3D11_TEX2D_RTV,
    pub Texture2DArray: D3D11_TEX2D_ARRAY_RTV,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D11_RENDER_TARGET_VIEW_DESC {
    pub Format: DXGI_FORMAT,
    pub ViewDimension: D3D11_RTV_DIMENSION,
    pub Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D11_INPUT_ELEMENT_DESC {
    pub SemanticName: PCSTR,
    pub SemanticIndex: u32,
    pub Format: DXGI_FORMAT,
    pub InputSlot: u32,
    pub AlignedByteOffset: u32,
    pub InputSlotClass: D3D11_INPUT_CLASSIFICATION,
    pub InstanceDataStepRate: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_RENDER_TARGET_BLEND_DESC {
    pub BlendEnable: BOOL,
    pub SrcBlend: D3D11_BLEND,
    pub DestBlend: D3D11_BLEND,
    pub BlendOp: D3D11_BLEND_OP,
    pub SrcBlendAlpha: D3D11_BLEND,
    pub DestBlendAlpha: D3D11_BLEND,
    pub BlendOpAlpha: D3D11_BLEND_OP,
    pub RenderTargetWriteMask: u8,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_BLEND_DESC {
    pub AlphaToCoverageEnable: BOOL,
    pub IndependentBlendEnable: BOOL,
    pub RenderTarget: [D3D11_RENDER_TARGET_BLEND_DESC; 8],
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_RASTERIZER_DESC {
    pub FillMode: D3D11_FILL_MODE,
    pub CullMode: D3D11_CULL_MODE,
    pub FrontCounterClockwise: BOOL,
    pub DepthBias: i32,
    pub DepthBiasClamp: f32,
    pub SlopeScaledDepthBias: f32,
    pub DepthClipEnable: BOOL,
    pub ScissorEnable: BOOL,
    pub MultisampleEnable: BOOL,
    pub AntialiasedLineEnable: BOOL,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_SAMPLER_DESC {
    pub Filter: D3D11_FILTER,
    pub AddressU: D3D11_TEXTURE_ADDRESS_MODE,
    pub AddressV: D3D11_TEXTURE_ADDRESS_MODE,
    pub AddressW: D3D11_TEXTURE_ADDRESS_MODE,
    pub MipLODBias: f32,
    pub MaxAnisotropy: u32,
    pub ComparisonFunc: D3D11_COMPARISON_FUNC,
    pub BorderColor: [f32; 4],
    pub MinLOD: f32,
    pub MaxLOD: f32,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D11_MAPPED_SUBRESOURCE {
    pub pData: *mut c_void,
    pub RowPitch: u32,
    pub DepthPitch: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_BOX {
    pub left: u32,
    pub top: u32,
    pub front: u32,
    pub right: u32,
    pub bottom: u32,
    pub back: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_VIEWPORT {
    pub TopLeftX: f32,
    pub TopLeftY: f32,
    pub Width: f32,
    pub Height: f32,
    pub MinDepth: f32,
    pub MaxDepth: f32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D11_QUERY_DESC {
    pub Query: D3D11_QUERY,
    pub MiscFlags: u32,
}

link!("d3d11.dll" "system" fn D3D11CreateDevice(padapter : *mut core::ffi::c_void, drivertype : D3D_DRIVER_TYPE, software : crate::Win32::Foundation::HMODULE, flags : D3D11_CREATE_DEVICE_FLAG, pfeaturelevels : *const D3D_FEATURE_LEVEL, featurelevels : u32, sdkversion : u32, ppdevice : *mut *mut core::ffi::c_void, pfeaturelevel : *mut D3D_FEATURE_LEVEL, ppimmediatecontext : *mut *mut core::ffi::c_void) -> crate::core::HRESULT);

pub const IID_ID3D11Device: GUID = GUID::from_u128(0xdb6f6ddb_ac77_4e88_8253_819df9bbf140);
pub const IID_ID3D11DeviceContext: GUID = GUID::from_u128(0xc0bfa96c_e089_44fb_8eaf_26f8796190da);
pub const IID_ID3D11Resource: GUID = GUID::from_u128(0xdc8e63f3_d12b_4952_b47b_5e45026a862d);
pub const IID_ID3D11Texture2D: GUID = GUID::from_u128(0x6f15aaf2_d208_4e89_9ab4_489535d34f9c);

/// `ID3D11Device`, through `GetDeviceRemovedReason` (slot 39).
#[repr(C)]
pub struct ID3D11Device_Vtbl {
    pub base__: IUnknown_Vtbl,
    // ID3D11Device
    pub CreateBuffer: unsafe extern "system" fn(
        *mut c_void,
        *const D3D11_BUFFER_DESC,
        *const D3D11_SUBRESOURCE_DATA,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateTexture1D: usize,
    pub CreateTexture2D: unsafe extern "system" fn(
        *mut c_void,
        *const D3D11_TEXTURE2D_DESC,
        *const D3D11_SUBRESOURCE_DATA,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateTexture3D: usize,
    pub CreateShaderResourceView: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const D3D11_SHADER_RESOURCE_VIEW_DESC,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateUnorderedAccessView: usize,
    pub CreateRenderTargetView: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const D3D11_RENDER_TARGET_VIEW_DESC,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateDepthStencilView: usize,
    pub CreateInputLayout: unsafe extern "system" fn(
        *mut c_void,
        *const D3D11_INPUT_ELEMENT_DESC,
        u32,
        *const c_void,
        usize,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateVertexShader: unsafe extern "system" fn(
        *mut c_void,
        *const c_void,
        usize,
        *mut c_void,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateGeometryShader: usize,
    pub CreateGeometryShaderWithStreamOutput: usize,
    pub CreatePixelShader: unsafe extern "system" fn(
        *mut c_void,
        *const c_void,
        usize,
        *mut c_void,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateHullShader: usize,
    pub CreateDomainShader: usize,
    pub CreateComputeShader: usize,
    pub CreateClassLinkage: usize,
    pub CreateBlendState: unsafe extern "system" fn(
        *mut c_void,
        *const D3D11_BLEND_DESC,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateDepthStencilState: usize,
    pub CreateRasterizerState: unsafe extern "system" fn(
        *mut c_void,
        *const D3D11_RASTERIZER_DESC,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateSamplerState: unsafe extern "system" fn(
        *mut c_void,
        *const D3D11_SAMPLER_DESC,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateQuery: unsafe extern "system" fn(
        *mut c_void,
        *const D3D11_QUERY_DESC,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreatePredicate: usize,
    pub CreateCounter: usize,
    pub CreateDeferredContext: usize,
    pub OpenSharedResource: usize,
    pub CheckFormatSupport:
        unsafe extern "system" fn(*mut c_void, DXGI_FORMAT, *mut u32) -> HRESULT,
    pub CheckMultisampleQualityLevels: usize,
    pub CheckCounterInfo: usize,
    pub CheckCounter: usize,
    pub CheckFeatureSupport: usize,
    pub GetPrivateData: usize,
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    pub GetFeatureLevel: unsafe extern "system" fn(*mut c_void) -> D3D_FEATURE_LEVEL,
    pub GetCreationFlags: usize,
    pub GetDeviceRemovedReason: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

/// `ID3D11DeviceContext`, through `Flush` (slot 111).
#[repr(C)]
pub struct ID3D11DeviceContext_Vtbl {
    pub base__: IUnknown_Vtbl,
    // ID3D11DeviceChild
    pub GetDevice: usize,
    pub GetPrivateData: usize,
    pub SetPrivateData: usize,
    pub SetPrivateDataInterface: usize,
    // ID3D11DeviceContext
    pub VSSetConstantBuffers: unsafe extern "system" fn(*mut c_void, u32, u32, *const *mut c_void),
    pub PSSetShaderResources: unsafe extern "system" fn(*mut c_void, u32, u32, *const *mut c_void),
    pub PSSetShader: unsafe extern "system" fn(*mut c_void, *mut c_void, *const *mut c_void, u32),
    pub PSSetSamplers: unsafe extern "system" fn(*mut c_void, u32, u32, *const *mut c_void),
    pub VSSetShader: unsafe extern "system" fn(*mut c_void, *mut c_void, *const *mut c_void, u32),
    pub DrawIndexed: usize,
    pub Draw: unsafe extern "system" fn(*mut c_void, u32, u32),
    pub Map: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        u32,
        D3D11_MAP,
        u32,
        *mut D3D11_MAPPED_SUBRESOURCE,
    ) -> HRESULT,
    pub Unmap: unsafe extern "system" fn(*mut c_void, *mut c_void, u32),
    pub PSSetConstantBuffers: unsafe extern "system" fn(*mut c_void, u32, u32, *const *mut c_void),
    pub IASetInputLayout: unsafe extern "system" fn(*mut c_void, *mut c_void),
    pub IASetVertexBuffers: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u32,
        *const *mut c_void,
        *const u32,
        *const u32,
    ),
    pub IASetIndexBuffer: usize,
    pub DrawIndexedInstanced: usize,
    pub DrawInstanced: unsafe extern "system" fn(*mut c_void, u32, u32, u32, u32),
    pub GSSetConstantBuffers: usize,
    pub GSSetShader: usize,
    pub IASetPrimitiveTopology: unsafe extern "system" fn(*mut c_void, D3D_PRIMITIVE_TOPOLOGY),
    pub VSSetShaderResources: unsafe extern "system" fn(*mut c_void, u32, u32, *const *mut c_void),
    pub VSSetSamplers: usize,
    pub Begin: unsafe extern "system" fn(*mut c_void, *mut c_void),
    pub End: unsafe extern "system" fn(*mut c_void, *mut c_void),
    pub GetData:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, u32, u32) -> HRESULT,
    pub SetPredication: usize,
    pub GSSetShaderResources: usize,
    pub GSSetSamplers: usize,
    pub OMSetRenderTargets:
        unsafe extern "system" fn(*mut c_void, u32, *const *mut c_void, *mut c_void),
    pub OMSetRenderTargetsAndUnorderedAccessViews: usize,
    pub OMSetBlendState: unsafe extern "system" fn(*mut c_void, *mut c_void, *const f32, u32),
    pub OMSetDepthStencilState: usize,
    pub SOSetTargets: usize,
    pub DrawAuto: usize,
    pub DrawIndexedInstancedIndirect: usize,
    pub DrawInstancedIndirect: usize,
    pub Dispatch: usize,
    pub DispatchIndirect: usize,
    pub RSSetState: unsafe extern "system" fn(*mut c_void, *mut c_void),
    pub RSSetViewports: unsafe extern "system" fn(*mut c_void, u32, *const D3D11_VIEWPORT),
    pub RSSetScissorRects: unsafe extern "system" fn(*mut c_void, u32, *const RECT),
    pub CopySubresourceRegion: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        u32,
        u32,
        u32,
        u32,
        *mut c_void,
        u32,
        *const D3D11_BOX,
    ),
    pub CopyResource: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void),
    pub UpdateSubresource: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        u32,
        *const D3D11_BOX,
        *const c_void,
        u32,
        u32,
    ),
    pub CopyStructureCount: usize,
    pub ClearRenderTargetView: unsafe extern "system" fn(*mut c_void, *mut c_void, *const f32),
    pub ClearUnorderedAccessViewUint: usize,
    pub ClearUnorderedAccessViewFloat: usize,
    pub ClearDepthStencilView: usize,
    pub GenerateMips: usize,
    pub SetResourceMinLOD: usize,
    pub GetResourceMinLOD: usize,
    pub ResolveSubresource: usize,
    pub ExecuteCommandList: usize,
    pub HSSetShaderResources: usize,
    pub HSSetShader: usize,
    pub HSSetSamplers: usize,
    pub HSSetConstantBuffers: usize,
    pub DSSetShaderResources: usize,
    pub DSSetShader: usize,
    pub DSSetSamplers: usize,
    pub DSSetConstantBuffers: usize,
    pub CSSetShaderResources: usize,
    pub CSSetUnorderedAccessViews: usize,
    pub CSSetShader: usize,
    pub CSSetSamplers: usize,
    pub CSSetConstantBuffers: usize,
    pub VSGetConstantBuffers: usize,
    pub PSGetShaderResources: usize,
    pub PSGetShader: usize,
    pub PSGetSamplers: usize,
    pub VSGetShader: usize,
    pub PSGetConstantBuffers: usize,
    pub IAGetInputLayout: usize,
    pub IAGetVertexBuffers: usize,
    pub IAGetIndexBuffer: usize,
    pub GSGetConstantBuffers: usize,
    pub GSGetShader: usize,
    pub IAGetPrimitiveTopology: usize,
    pub VSGetShaderResources: usize,
    pub VSGetSamplers: usize,
    pub GetPredication: usize,
    pub GSGetShaderResources: usize,
    pub GSGetSamplers: usize,
    pub OMGetRenderTargets: usize,
    pub OMGetRenderTargetsAndUnorderedAccessViews: usize,
    pub OMGetBlendState: usize,
    pub OMGetDepthStencilState: usize,
    pub SOGetTargets: usize,
    pub RSGetState: usize,
    pub RSGetViewports: usize,
    pub RSGetScissorRects: usize,
    pub HSGetShaderResources: usize,
    pub HSGetShader: usize,
    pub HSGetSamplers: usize,
    pub HSGetConstantBuffers: usize,
    pub DSGetShaderResources: usize,
    pub DSGetShader: usize,
    pub DSGetSamplers: usize,
    pub DSGetConstantBuffers: usize,
    pub CSGetShaderResources: usize,
    pub CSGetUnorderedAccessViews: usize,
    pub CSGetShader: usize,
    pub CSGetSamplers: usize,
    pub CSGetConstantBuffers: usize,
    pub ClearState: unsafe extern "system" fn(*mut c_void),
    pub Flush: unsafe extern "system" fn(*mut c_void),
}
