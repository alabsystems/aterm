// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance and reference-counting tests for [`Com`] and the DXGI, Direct3D 11, DirectComposition and UI Automation declarations.

#![allow(non_snake_case)]

use aterm_win32::Win32::Foundation::E_POINTER;
use aterm_win32::com::{Com, IInspectable_Vtbl, IUnknown_Vtbl, check};
use aterm_win32::core::{GUID, HRESULT};
use aterm_win32::d3d11 as X;
use aterm_win32::dcomp as XD;
use aterm_win32::dxgi as D;
use aterm_win32::uia as U;
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
    assert!(offset_of!(D::IDXGIAdapter1_Vtbl, GetParent) == slot(6));
    assert!(offset_of!(D::IDXGIAdapter1_Vtbl, EnumOutputs) == slot(7));
    assert!(offset_of!(D::IDXGIAdapter1_Vtbl, GetDesc1) == slot(10));
    assert!(size_of::<D::IDXGIAdapter1_Vtbl>() == slot(11));
    assert!(offset_of!(D::IDXGIOutput6_Vtbl, GetDesc1) == slot(27));
    assert!(size_of::<D::IDXGIOutput6_Vtbl>() == slot(28));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, Present) == slot(8));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, GetBuffer) == slot(9));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, ResizeBuffers) == slot(13));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, GetContainingOutput) == slot(15));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, SetMaximumFrameLatency) == slot(31));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, GetFrameLatencyWaitableObject) == slot(33));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, CheckColorSpaceSupport) == slot(37));
    assert!(offset_of!(D::IDXGISwapChain3_Vtbl, SetColorSpace1) == slot(38));
    assert!(size_of::<D::IDXGISwapChain3_Vtbl>() == slot(39));
    assert!(offset_of!(D::IDXGIDevice_Vtbl, GetAdapter) == slot(7));
    assert!(size_of::<D::IDXGIDevice_Vtbl>() == slot(8));
    assert!(offset_of!(D::IDXGIFactory2_Vtbl, MakeWindowAssociation) == slot(8));
    assert!(offset_of!(D::IDXGIFactory2_Vtbl, EnumAdapters1) == slot(12));
    assert!(offset_of!(D::IDXGIFactory2_Vtbl, CreateSwapChainForHwnd) == slot(15));
    assert!(offset_of!(D::IDXGIFactory2_Vtbl, CreateSwapChainForComposition) == slot(24));
    assert!(size_of::<D::IDXGIFactory2_Vtbl>() == slot(25));
    assert!(offset_of!(D::IDXGIFactory6_Vtbl, EnumWarpAdapter) == slot(27));
    assert!(offset_of!(D::IDXGIFactory6_Vtbl, EnumAdapterByGpuPreference) == slot(29));
    assert!(size_of::<D::IDXGIFactory6_Vtbl>() == slot(30));
    assert!(offset_of!(X::ID3DBlob_Vtbl, GetBufferPointer) == slot(3));
    assert!(offset_of!(X::ID3DBlob_Vtbl, GetBufferSize) == slot(4));
    assert!(size_of::<X::ID3DBlob_Vtbl>() == slot(5));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateBuffer) == slot(3));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateTexture2D) == slot(5));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateShaderResourceView) == slot(7));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateRenderTargetView) == slot(9));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateInputLayout) == slot(11));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateVertexShader) == slot(12));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreatePixelShader) == slot(15));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateBlendState) == slot(20));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateRasterizerState) == slot(22));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateSamplerState) == slot(23));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CreateQuery) == slot(24));
    assert!(offset_of!(X::ID3D11Device_Vtbl, CheckFormatSupport) == slot(29));
    assert!(offset_of!(X::ID3D11Device_Vtbl, GetFeatureLevel) == slot(37));
    assert!(offset_of!(X::ID3D11Device_Vtbl, GetDeviceRemovedReason) == slot(39));
    assert!(size_of::<X::ID3D11Device_Vtbl>() == slot(40));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, VSSetConstantBuffers) == slot(7));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, PSSetShaderResources) == slot(8));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, PSSetShader) == slot(9));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, PSSetSamplers) == slot(10));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, VSSetShader) == slot(11));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, Draw) == slot(13));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, Map) == slot(14));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, Unmap) == slot(15));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, PSSetConstantBuffers) == slot(16));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, IASetInputLayout) == slot(17));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, IASetVertexBuffers) == slot(18));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, DrawInstanced) == slot(21));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, IASetPrimitiveTopology) == slot(24));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, VSSetShaderResources) == slot(25));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, Begin) == slot(27));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, End) == slot(28));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, GetData) == slot(29));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, OMSetRenderTargets) == slot(33));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, OMSetBlendState) == slot(35));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, RSSetState) == slot(43));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, RSSetViewports) == slot(44));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, RSSetScissorRects) == slot(45));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, CopySubresourceRegion) == slot(46));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, CopyResource) == slot(47));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, UpdateSubresource) == slot(48));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, ClearRenderTargetView) == slot(50));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, ClearState) == slot(110));
    assert!(offset_of!(X::ID3D11DeviceContext_Vtbl, Flush) == slot(111));
    assert!(size_of::<X::ID3D11DeviceContext_Vtbl>() == slot(112));
    assert!(offset_of!(XD::IDCompositionDevice_Vtbl, Commit) == slot(3));
    assert!(offset_of!(XD::IDCompositionDevice_Vtbl, CreateTargetForHwnd) == slot(6));
    assert!(offset_of!(XD::IDCompositionDevice_Vtbl, CreateVisual) == slot(7));
    assert!(size_of::<XD::IDCompositionDevice_Vtbl>() == slot(8));
    assert!(offset_of!(XD::IDCompositionTarget_Vtbl, SetRoot) == slot(3));
    assert!(size_of::<XD::IDCompositionTarget_Vtbl>() == slot(4));
    assert!(offset_of!(XD::IDCompositionVisual_Vtbl, SetContent) == slot(15));
    assert!(size_of::<XD::IDCompositionVisual_Vtbl>() == slot(16));

    // WIN-UIA's provider interfaces, in UIAutomationCore.h's order.
    assert!(offset_of!(IInspectable_Vtbl, GetIids) == slot(3));
    assert!(offset_of!(IInspectable_Vtbl, GetTrustLevel) == slot(5));
    assert!(size_of::<IInspectable_Vtbl>() == slot(6));
    assert!(offset_of!(U::IRawElementProviderSimple_Vtbl, ProviderOptions) == slot(3));
    assert!(offset_of!(U::IRawElementProviderSimple_Vtbl, GetPropertyValue) == slot(5));
    assert!(size_of::<U::IRawElementProviderSimple_Vtbl>() == slot(7));
    assert!(offset_of!(U::IRawElementProviderFragment_Vtbl, Navigate) == slot(3));
    assert!(offset_of!(U::IRawElementProviderFragment_Vtbl, BoundingRectangle) == slot(5));
    assert!(offset_of!(U::IRawElementProviderFragment_Vtbl, FragmentRoot) == slot(8));
    assert!(size_of::<U::IRawElementProviderFragment_Vtbl>() == slot(9));
    assert!(offset_of!(U::IRawElementProviderFragmentRoot_Vtbl, GetFocus) == slot(4));
    assert!(size_of::<U::IRawElementProviderFragmentRoot_Vtbl>() == slot(5));
    assert!(size_of::<U::IInvokeProvider_Vtbl>() == slot(4));
    assert!(offset_of!(U::IValueProvider_Vtbl, IsReadOnly) == slot(5));
    assert!(size_of::<U::IValueProvider_Vtbl>() == slot(6));
    assert!(offset_of!(U::IRangeValueProvider_Vtbl, Maximum) == slot(6));
    assert!(offset_of!(U::IRangeValueProvider_Vtbl, SmallChange) == slot(9));
    assert!(size_of::<U::IRangeValueProvider_Vtbl>() == slot(10));
    assert!(offset_of!(U::IToggleProvider_Vtbl, ToggleState) == slot(4));
    assert!(size_of::<U::IToggleProvider_Vtbl>() == slot(5));
    assert!(offset_of!(U::ISelectionProvider_Vtbl, IsSelectionRequired) == slot(5));
    assert!(size_of::<U::ISelectionProvider_Vtbl>() == slot(6));
    assert!(offset_of!(U::ISelectionItemProvider_Vtbl, IsSelected) == slot(6));
    assert!(size_of::<U::ISelectionItemProvider_Vtbl>() == slot(8));
    assert!(size_of::<U::IScrollItemProvider_Vtbl>() == slot(4));
    assert!(offset_of!(U::ITextProvider_Vtbl, RangeFromPoint) == slot(6));
    assert!(size_of::<U::ITextProvider_Vtbl>() == slot(9));
    assert!(offset_of!(U::ITextProvider2_Vtbl, GetCaretRange) == slot(10));
    assert!(size_of::<U::ITextProvider2_Vtbl>() == slot(11));
    assert!(offset_of!(U::ITextRangeProvider_Vtbl, FindAttribute) == slot(7));
    assert!(offset_of!(U::ITextRangeProvider_Vtbl, GetText) == slot(12));
    assert!(offset_of!(U::ITextRangeProvider_Vtbl, AddToSelection) == slot(17));
    assert!(offset_of!(U::ITextRangeProvider_Vtbl, GetChildren) == slot(20));
    assert!(size_of::<U::ITextRangeProvider_Vtbl>() == slot(21));

    // GW.5's structs, on 64-bit hosts.
    assert!(
        size_of::<D::DXGI_SWAP_CHAIN_DESC1>() == 48 && align_of::<D::DXGI_SWAP_CHAIN_DESC1>() == 4
    );
    assert!(offset_of!(D::DXGI_SWAP_CHAIN_DESC1, SampleDesc) == 16);
    assert!(offset_of!(D::DXGI_SWAP_CHAIN_DESC1, Flags) == 44);
    assert!(size_of::<D::DXGI_SWAP_CHAIN_FULLSCREEN_DESC>() == 20);
    assert!(size_of::<D::DXGI_ADAPTER_DESC1>() == 312 && align_of::<D::DXGI_ADAPTER_DESC1>() == 8);
    assert!(offset_of!(D::DXGI_ADAPTER_DESC1, DedicatedVideoMemory) == 272);
    assert!(offset_of!(D::DXGI_ADAPTER_DESC1, AdapterLuid) == 296);
    assert!(offset_of!(D::DXGI_ADAPTER_DESC1, Flags) == 304);
    assert!(size_of::<X::D3D_SHADER_MACRO>() == 16);
    assert!(size_of::<X::D3D11_BUFFER_DESC>() == 24);
    assert!(size_of::<X::D3D11_TEXTURE2D_DESC>() == 44);
    assert!(offset_of!(X::D3D11_TEXTURE2D_DESC, SampleDesc) == 20);
    assert!(size_of::<X::D3D11_SUBRESOURCE_DATA>() == 16);
    assert!(size_of::<X::D3D11_SHADER_RESOURCE_VIEW_DESC_0>() == 16);
    assert!(size_of::<X::D3D11_SHADER_RESOURCE_VIEW_DESC>() == 24);
    assert!(size_of::<X::D3D11_RENDER_TARGET_VIEW_DESC_0>() == 12);
    assert!(size_of::<X::D3D11_RENDER_TARGET_VIEW_DESC>() == 20);
    assert!(
        size_of::<X::D3D11_INPUT_ELEMENT_DESC>() == 32
            && align_of::<X::D3D11_INPUT_ELEMENT_DESC>() == 8
    );
    assert!(offset_of!(X::D3D11_INPUT_ELEMENT_DESC, InstanceDataStepRate) == 28);
    assert!(size_of::<X::D3D11_RENDER_TARGET_BLEND_DESC>() == 32);
    assert!(offset_of!(X::D3D11_RENDER_TARGET_BLEND_DESC, RenderTargetWriteMask) == 28);
    assert!(size_of::<X::D3D11_BLEND_DESC>() == 264);
    assert!(size_of::<X::D3D11_RASTERIZER_DESC>() == 40);
    assert!(size_of::<X::D3D11_SAMPLER_DESC>() == 52);
    assert!(offset_of!(X::D3D11_SAMPLER_DESC, BorderColor) == 28);
    assert!(size_of::<X::D3D11_MAPPED_SUBRESOURCE>() == 16);
    assert!(size_of::<X::D3D11_BOX>() == 24);
    assert!(size_of::<X::D3D11_VIEWPORT>() == 24);
    assert!(size_of::<X::D3D11_QUERY_DESC>() == 8);

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
fn the_ui_automation_interface_ids_are_distinct() {
    use aterm_win32::com::{IID_IAgileObject, IID_IInspectable, IID_IMarshal, IID_IUnknown};
    let ids = [
        IID_IUnknown,
        IID_IInspectable,
        IID_IAgileObject,
        IID_IMarshal,
        U::IID_IRawElementProviderSimple,
        U::IID_IRawElementProviderFragment,
        U::IID_IRawElementProviderFragmentRoot,
        U::IID_IInvokeProvider,
        U::IID_IValueProvider,
        U::IID_IRangeValueProvider,
        U::IID_IToggleProvider,
        U::IID_ISelectionProvider,
        U::IID_ISelectionItemProvider,
        U::IID_IScrollItemProvider,
        U::IID_ITextProvider,
        U::IID_ITextProvider2,
        U::IID_ITextRangeProvider,
    ];
    for (i, a) in ids.iter().enumerate() {
        for b in &ids[i + 1..] {
            assert!(!same(a, b), "two interfaces share an IID");
        }
    }
    assert!(aterm_win32::com::same_iid(
        &ids[4],
        &U::IID_IRawElementProviderSimple
    ));
    assert!(!aterm_win32::com::same_iid(&ids[4], &ids[5]));
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
