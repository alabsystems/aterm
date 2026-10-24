// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nullable interface results over the existing generated COM vtables.
//!
//! The generated traits' `Err(Error::empty())` convention returns S_OK without
//! writing the output, so these wrappers null each nullable slot before
//! delegating to the original thunk.

use super::*;
use std::ffi::c_void;
use windows::Win32::Foundation::E_POINTER;
use windows::Win32::System::Variant::VARIANT as AbiVariant;

macro_rules! nullable_interface {
    ($name:ident, $vtable:ident, $interface:ty, $original_vtable:ty, $implementation:path,
        $( $method:ident( $( $arg:ident : $ty:ty ),* ) -> $out:ty );+ $(;)?) => {
        #[repr(transparent)]
        #[derive(Clone)]
        pub(super) struct $name(pub(super) $interface);

        // The wrapper preserves the generated interface's exact pointer layout,
        // IID and vtable layout; only the listed output-initializing thunks differ.
        unsafe impl Interface for $name {
            type Vtable = $vtable;
            const IID: windows::core::GUID = <$interface as Interface>::IID;
        }

        impl windows::core::RuntimeName for $name {
            const NAME: &'static str = <$interface as windows::core::RuntimeName>::NAME;
        }

        #[repr(transparent)]
        pub(super) struct $vtable($original_vtable);

        impl $vtable {
            pub(super) const fn new<Identity: $implementation, const OFFSET: isize>() -> Self {
                let mut vtable = <$original_vtable>::new::<Identity, OFFSET>();
                $(vtable.$method = Self::$method::<Identity, OFFSET>;)+
                Self(vtable)
            }

            pub(super) fn matches(iid: &windows::core::GUID) -> bool {
                <$original_vtable>::matches(iid)
            }

            $(
                unsafe extern "system" fn $method<Identity: $implementation, const OFFSET: isize>(
                    this: *mut c_void, $($arg: $ty,)* out: *mut *mut $out,
                ) -> HRESULT {
                    if !unsafe { crate::uia_abi::initialize_nullable_out(out) } {
                        return E_POINTER;
                    }
                    unsafe { (<$original_vtable>::new::<Identity, OFFSET>().$method)(this, $($arg,)* out) }
                }
            )+
        }
    };
}

nullable_interface!(
    Simple, SimpleVtbl, IRawElementProviderSimple, IRawElementProviderSimple_Vtbl, IRawElementProviderSimple_Impl,
    GetPatternProvider(pattern: UIA_PATTERN_ID) -> c_void;
    HostRawElementProvider() -> c_void;
);
nullable_interface!(
    Fragment, FragmentVtbl, IRawElementProviderFragment, IRawElementProviderFragment_Vtbl, IRawElementProviderFragment_Impl,
    Navigate(direction: NavigateDirection) -> c_void;
    GetRuntimeId() -> SAFEARRAY;
    GetEmbeddedFragmentRoots() -> SAFEARRAY;
);
nullable_interface!(
    FragmentRoot, FragmentRootVtbl, IRawElementProviderFragmentRoot, IRawElementProviderFragmentRoot_Vtbl, IRawElementProviderFragmentRoot_Impl,
    ElementProviderFromPoint(x: f64, y: f64) -> c_void;
    GetFocus() -> c_void;
);
nullable_interface!(
    Range, RangeVtbl, ITextRangeProvider, ITextRangeProvider_Vtbl, ITextRangeProvider_Impl,
    FindAttribute(id: UIA_TEXTATTRIBUTE_ID, value: AbiVariant, backward: BOOL) -> c_void;
    FindText(text: *mut c_void, backward: BOOL, ignore_case: BOOL) -> c_void;
);

nullable_interface!(
    SelectionItem, SelectionItemVtbl, ISelectionItemProvider, ISelectionItemProvider_Vtbl, ISelectionItemProvider_Impl,
    SelectionContainer() -> c_void;
);
