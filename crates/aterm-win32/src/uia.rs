// Copyright (c) Microsoft Corporation.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// HAND-WRITTEN, not generated: `aterm-win32-oracle gen` neither emits nor
// deletes this file, and harvests the generated declarations it names like a
// consumer's. Derived from windows 0.62.2
// src/Windows/Win32/UI/Accessibility/mod.rs and modified by the aterm project
// in 2026: each vtable is one flat struct over `IUnknown_Vtbl` (ITextProvider2's
// over ITextProvider's), with every slot typed. See ../LICENSE-MIT and
// ../LICENSE-APACHE for the upstream licenses.

//! UI Automation's provider interfaces, the ones aterm-a11y's provider
//! implements (WIN-UIA of docs/AUDIT-third-party-2026-10-15.md): the element
//! (simple, fragment, fragment root), its control patterns, and the text
//! pattern's provider and ranges. The imports that hand a provider to UI
//! Automation and raise its events (`UiaReturnRawElementProvider`,
//! `UiaRaiseAutomationEvent`, …) are generated, in
//! [`crate::Win32::UI::Accessibility`].
//!
//! aterm implements these, so every slot is typed (a server fills each one);
//! win32-oracle/conformance/src/com.rs holds them to `windows` 0.62.2. A BSTR
//! travels as upstream spells it, `*mut c_void` in and `*mut *mut c_void` out.

use crate::Win32::System::Com::SAFEARRAY;
use crate::Win32::System::Variant::VARIANT;
use crate::Win32::UI::Accessibility::{
    NavigateDirection, ProviderOptions, SupportedTextSelection, TextPatternRangeEndpoint, TextUnit,
    ToggleState, UIA_PATTERN_ID, UIA_PROPERTY_ID, UIA_TEXTATTRIBUTE_ID, UiaPoint, UiaRect,
};
use crate::com::IUnknown_Vtbl;
use crate::core::{BOOL, GUID, HRESULT, PCWSTR};
use core::ffi::c_void;

pub const IID_IRawElementProviderSimple: GUID =
    GUID::from_u128(0xd6dd68d1_86fd_4332_8666_9abedea2d24c);
pub const IID_IRawElementProviderFragment: GUID =
    GUID::from_u128(0xf7063da8_8359_439c_9297_bbc5299a7d87);
pub const IID_IRawElementProviderFragmentRoot: GUID =
    GUID::from_u128(0x620ce2a5_ab8f_40a9_86cb_de3c75599b58);
pub const IID_IInvokeProvider: GUID = GUID::from_u128(0x54fcb24b_e18e_47a2_b4d3_eccbe77599a2);
pub const IID_IValueProvider: GUID = GUID::from_u128(0xc7935180_6fb3_4201_b174_7df73adbf64a);
pub const IID_IRangeValueProvider: GUID = GUID::from_u128(0x36dc7aef_33e6_4691_afe1_2be7274b3d33);
pub const IID_IToggleProvider: GUID = GUID::from_u128(0x56d00bd0_c4f4_433c_a836_1a52a57e0892);
pub const IID_ISelectionProvider: GUID = GUID::from_u128(0xfb8b03af_3bdf_48d4_bd36_1a65793be168);
pub const IID_ISelectionItemProvider: GUID =
    GUID::from_u128(0x2acad808_b2d4_452d_a407_91ff1ad167b2);
pub const IID_IScrollItemProvider: GUID = GUID::from_u128(0x2360c714_4bf1_4b26_ba65_9b21316127eb);
pub const IID_ITextProvider: GUID = GUID::from_u128(0x3589c92c_63f3_4367_99bb_ada653b77cf2);
pub const IID_ITextProvider2: GUID = GUID::from_u128(0x0dc5e6ed_3e16_4bf1_8f9a_a979878bc195);
pub const IID_ITextRangeProvider: GUID = GUID::from_u128(0x5347ad7b_c355_46f8_aff5_909033582f63);

/// `IRawElementProviderSimple`: an element's properties and patterns.
#[repr(C)]
pub struct IRawElementProviderSimple_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub ProviderOptions: unsafe extern "system" fn(*mut c_void, *mut ProviderOptions) -> HRESULT,
    pub GetPatternProvider:
        unsafe extern "system" fn(*mut c_void, UIA_PATTERN_ID, *mut *mut c_void) -> HRESULT,
    pub GetPropertyValue:
        unsafe extern "system" fn(*mut c_void, UIA_PROPERTY_ID, *mut VARIANT) -> HRESULT,
    pub HostRawElementProvider: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

/// `IRawElementProviderFragment`: an element's place in its fragment.
#[repr(C)]
pub struct IRawElementProviderFragment_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub Navigate:
        unsafe extern "system" fn(*mut c_void, NavigateDirection, *mut *mut c_void) -> HRESULT,
    pub GetRuntimeId: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    pub BoundingRectangle: unsafe extern "system" fn(*mut c_void, *mut UiaRect) -> HRESULT,
    pub GetEmbeddedFragmentRoots:
        unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    pub SetFocus: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub FragmentRoot: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

/// `IRawElementProviderFragmentRoot`: hit testing and focus over a fragment.
#[repr(C)]
pub struct IRawElementProviderFragmentRoot_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub ElementProviderFromPoint:
        unsafe extern "system" fn(*mut c_void, f64, f64, *mut *mut c_void) -> HRESULT,
    pub GetFocus: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

/// `IInvokeProvider`.
#[repr(C)]
pub struct IInvokeProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub Invoke: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

/// `IValueProvider`; `Value` hands out a BSTR.
#[repr(C)]
pub struct IValueProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub SetValue: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT,
    pub Value: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub IsReadOnly: unsafe extern "system" fn(*mut c_void, *mut BOOL) -> HRESULT,
}

/// `IRangeValueProvider`.
#[repr(C)]
pub struct IRangeValueProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub SetValue: unsafe extern "system" fn(*mut c_void, f64) -> HRESULT,
    pub Value: unsafe extern "system" fn(*mut c_void, *mut f64) -> HRESULT,
    pub IsReadOnly: unsafe extern "system" fn(*mut c_void, *mut BOOL) -> HRESULT,
    pub Maximum: unsafe extern "system" fn(*mut c_void, *mut f64) -> HRESULT,
    pub Minimum: unsafe extern "system" fn(*mut c_void, *mut f64) -> HRESULT,
    pub LargeChange: unsafe extern "system" fn(*mut c_void, *mut f64) -> HRESULT,
    pub SmallChange: unsafe extern "system" fn(*mut c_void, *mut f64) -> HRESULT,
}

/// `IToggleProvider`.
#[repr(C)]
pub struct IToggleProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub Toggle: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub ToggleState: unsafe extern "system" fn(*mut c_void, *mut ToggleState) -> HRESULT,
}

/// `ISelectionProvider`.
#[repr(C)]
pub struct ISelectionProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub GetSelection: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    pub CanSelectMultiple: unsafe extern "system" fn(*mut c_void, *mut BOOL) -> HRESULT,
    pub IsSelectionRequired: unsafe extern "system" fn(*mut c_void, *mut BOOL) -> HRESULT,
}

/// `ISelectionItemProvider`.
#[repr(C)]
pub struct ISelectionItemProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub Select: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub AddToSelection: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub RemoveFromSelection: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub IsSelected: unsafe extern "system" fn(*mut c_void, *mut BOOL) -> HRESULT,
    pub SelectionContainer: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

/// `IScrollItemProvider`.
#[repr(C)]
pub struct IScrollItemProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub ScrollIntoView: unsafe extern "system" fn(*mut c_void) -> HRESULT,
}

/// `ITextProvider`: the text pattern's document.
#[repr(C)]
pub struct ITextProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub GetSelection: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    pub GetVisibleRanges: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    pub RangeFromChild:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    pub RangeFromPoint:
        unsafe extern "system" fn(*mut c_void, UiaPoint, *mut *mut c_void) -> HRESULT,
    pub DocumentRange: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub SupportedTextSelection:
        unsafe extern "system" fn(*mut c_void, *mut SupportedTextSelection) -> HRESULT,
}

/// `ITextProvider2`, which extends [`ITextProvider_Vtbl`].
#[repr(C)]
pub struct ITextProvider2_Vtbl {
    pub base__: ITextProvider_Vtbl,
    pub RangeFromAnnotation:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT,
    pub GetCaretRange:
        unsafe extern "system" fn(*mut c_void, *mut BOOL, *mut *mut c_void) -> HRESULT,
}

/// `ITextRangeProvider`; `FindAttribute` takes its VARIANT by value (the caller
/// keeps it), `FindText` a BSTR the caller owns, and `GetText` hands one out.
#[repr(C)]
pub struct ITextRangeProvider_Vtbl {
    pub base__: IUnknown_Vtbl,
    pub Clone: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub Compare: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut BOOL) -> HRESULT,
    pub CompareEndpoints: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut i32,
    ) -> HRESULT,
    pub ExpandToEnclosingUnit: unsafe extern "system" fn(*mut c_void, TextUnit) -> HRESULT,
    pub FindAttribute: unsafe extern "system" fn(
        *mut c_void,
        UIA_TEXTATTRIBUTE_ID,
        VARIANT,
        BOOL,
        *mut *mut c_void,
    ) -> HRESULT,
    pub FindText: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        BOOL,
        BOOL,
        *mut *mut c_void,
    ) -> HRESULT,
    pub GetAttributeValue:
        unsafe extern "system" fn(*mut c_void, UIA_TEXTATTRIBUTE_ID, *mut VARIANT) -> HRESULT,
    pub GetBoundingRectangles:
        unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
    pub GetEnclosingElement: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    pub GetText: unsafe extern "system" fn(*mut c_void, i32, *mut *mut c_void) -> HRESULT,
    pub Move: unsafe extern "system" fn(*mut c_void, TextUnit, i32, *mut i32) -> HRESULT,
    pub MoveEndpointByUnit: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        TextUnit,
        i32,
        *mut i32,
    ) -> HRESULT,
    pub MoveEndpointByRange: unsafe extern "system" fn(
        *mut c_void,
        TextPatternRangeEndpoint,
        *mut c_void,
        TextPatternRangeEndpoint,
    ) -> HRESULT,
    pub Select: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub AddToSelection: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub RemoveFromSelection: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pub ScrollIntoView: unsafe extern "system" fn(*mut c_void, BOOL) -> HRESULT,
    pub GetChildren: unsafe extern "system" fn(*mut c_void, *mut *mut SAFEARRAY) -> HRESULT,
}
