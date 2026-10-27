// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The element and text-range objects: their layouts, their static vtables,
//! and the thunks that call [`Provider`]'s and [`TextRange`]'s methods.
//!
//! The slot contract every thunk here relies on: UI Automation (or a test)
//! calls a slot through an interface pointer this module handed out, which
//! points at that interface's vtable-pointer field of a live object and holds
//! a reference that keeps it alive for the call; each out pointer is the
//! caller's writable slot or null (refused as `E_POINTER` before anything
//! runs); and each interface argument is null or live for the call. A result
//! moves to the caller only on success; a failure writes nothing, except that
//! the nullable results (an element, a range or a runtime ID the method may
//! have none of) are nulled first and are null on S_OK with nothing, as
//! `windows`' wrappers made them.

use std::ffi::c_void;
use std::mem::offset_of;
use std::ptr;

use aterm_win32::Win32::Foundation::S_OK;
use aterm_win32::Win32::System::Com::SAFEARRAY;
use aterm_win32::Win32::System::Variant::VARIANT;
use aterm_win32::Win32::UI::Accessibility::{
    NavigateDirection, ProviderOptions, SupportedTextSelection, TextPatternRangeEndpoint, TextUnit,
    ToggleState, UIA_PATTERN_ID, UIA_PROPERTY_ID, UIA_TEXTATTRIBUTE_ID, UiaPoint, UiaRect,
};
use aterm_win32::com::{Com, IInspectable_Vtbl, same_iid};
use aterm_win32::core::{BOOL, GUID, HRESULT, PCWSTR};
use aterm_win32::uia::{
    IID_IInvokeProvider, IID_IRangeValueProvider, IID_IRawElementProviderFragment,
    IID_IRawElementProviderFragmentRoot, IID_IRawElementProviderSimple, IID_IScrollItemProvider,
    IID_ISelectionItemProvider, IID_ISelectionProvider, IID_ITextProvider, IID_ITextProvider2,
    IID_ITextRangeProvider, IID_IToggleProvider, IID_IValueProvider, IInvokeProvider_Vtbl,
    IRangeValueProvider_Vtbl, IRawElementProviderFragment_Vtbl,
    IRawElementProviderFragmentRoot_Vtbl, IRawElementProviderSimple_Vtbl, IScrollItemProvider_Vtbl,
    ISelectionItemProvider_Vtbl, ISelectionProvider_Vtbl, ITextProvider_Vtbl, ITextProvider2_Vtbl,
    ITextRangeProvider_Vtbl, IToggleProvider_Vtbl, IValueProvider_Vtbl,
};

use super::com::{
    self, Count, Object, Result, inspectable, into_caller, into_raw, put, put_nullable, raw,
    unknown,
};
use super::ranges::TextRange;
use super::values::{Bstr, Variant};
use super::{NOT_SUPPORTED, Provider};

/// An element: identity, then one vtable pointer per interface.
#[repr(C)]
pub(super) struct ProviderObject {
    identity: *const IInspectable_Vtbl,
    simple: *const IRawElementProviderSimple_Vtbl,
    fragment: *const IRawElementProviderFragment_Vtbl,
    root: *const IRawElementProviderFragmentRoot_Vtbl,
    invoke: *const IInvokeProvider_Vtbl,
    value: *const IValueProvider_Vtbl,
    range_value: *const IRangeValueProvider_Vtbl,
    toggle: *const IToggleProvider_Vtbl,
    selection: *const ISelectionProvider_Vtbl,
    selection_item: *const ISelectionItemProvider_Vtbl,
    scroll_item: *const IScrollItemProvider_Vtbl,
    text: *const ITextProvider2_Vtbl,
    count: Count,
    provider: Provider,
}

/// A text range: identity, then its one interface.
#[repr(C)]
pub(super) struct RangeObject {
    identity: *const IInspectable_Vtbl,
    range: *const ITextRangeProvider_Vtbl,
    count: Count,
    text: TextRange,
}

const fn send_sync<T: Send + Sync>() {}
const _: () = send_sync::<Provider>();
const _: () = send_sync::<TextRange>();

// SAFETY: beyond the `Send + Sync` state (asserted above), the fields are
// pointers to immutable statics and an atomic count.
unsafe impl Send for ProviderObject {}
// SAFETY: as above.
unsafe impl Sync for ProviderObject {}
// SAFETY: as above.
unsafe impl Send for RangeObject {}
// SAFETY: as above.
unsafe impl Sync for RangeObject {}

impl ProviderObject {
    pub(super) const fn provider(&self) -> &Provider {
        &self.provider
    }
}

impl RangeObject {
    pub(super) const fn range(&self) -> &TextRange {
        &self.text
    }
}

/// A new element object, as `IRawElementProviderSimple`.
pub(super) fn provider(provider: Provider) -> Com {
    into_raw(
        ProviderObject {
            identity: &PROVIDER_IDENTITY,
            simple: &SIMPLE_VTBL,
            fragment: &FRAGMENT_VTBL,
            root: &ROOT_VTBL,
            invoke: &INVOKE_VTBL,
            value: &VALUE_VTBL,
            range_value: &RANGE_VALUE_VTBL,
            toggle: &TOGGLE_VTBL,
            selection: &SELECTION_VTBL,
            selection_item: &SELECTION_ITEM_VTBL,
            scroll_item: &SCROLL_ITEM_VTBL,
            text: &TEXT_VTBL,
            count: Count::one(),
            provider,
        },
        SIMPLE,
    )
}

/// A new range object, as `ITextRangeProvider`.
pub(super) fn range(text: TextRange) -> Com {
    into_raw(
        RangeObject {
            identity: &RANGE_IDENTITY,
            range: &RANGE_VTBL,
            count: Count::one(),
            text,
        },
        RANGE,
    )
}

/// A field's address, as the interface pointer that names it.
fn field<T>(field: &*const T) -> *const c_void {
    ptr::from_ref(field).cast()
}

// SAFETY: `repr(C)` with the identity first; each slot `slot` returns is one
// of the vtable-pointer fields, whose vtable below begins with
// `unknown::<ProviderObject, offset of that field>()`; made only by `provider`.
unsafe impl Object for ProviderObject {
    fn identity() -> &'static IInspectable_Vtbl {
        &PROVIDER_IDENTITY
    }
    const CAST: GUID = GUID::from_u128(0x6a7d3c2e_5f1b_4e8a_9c4d_2b8e1f0a7c53);
    fn count(&self) -> &Count {
        &self.count
    }
    fn slot(&self, iid: &GUID) -> Option<*const c_void> {
        Some(if same_iid(iid, &IID_IRawElementProviderSimple) {
            field(&self.simple)
        } else if same_iid(iid, &IID_IRawElementProviderFragment) {
            field(&self.fragment)
        } else if same_iid(iid, &IID_IRawElementProviderFragmentRoot) {
            field(&self.root)
        } else if same_iid(iid, &IID_IInvokeProvider) {
            field(&self.invoke)
        } else if same_iid(iid, &IID_IValueProvider) {
            field(&self.value)
        } else if same_iid(iid, &IID_IRangeValueProvider) {
            field(&self.range_value)
        } else if same_iid(iid, &IID_IToggleProvider) {
            field(&self.toggle)
        } else if same_iid(iid, &IID_ISelectionProvider) {
            field(&self.selection)
        } else if same_iid(iid, &IID_ISelectionItemProvider) {
            field(&self.selection_item)
        } else if same_iid(iid, &IID_IScrollItemProvider) {
            field(&self.scroll_item)
        } else if same_iid(iid, &IID_ITextProvider2) || same_iid(iid, &IID_ITextProvider) {
            field(&self.text)
        } else {
            return None;
        })
    }
}

// SAFETY: as for `ProviderObject`, with its one interface; made only by `range`.
unsafe impl Object for RangeObject {
    fn identity() -> &'static IInspectable_Vtbl {
        &RANGE_IDENTITY
    }
    const CAST: GUID = GUID::from_u128(0x9e2b4f61_0d3a_47c5_8b1e_6f4a2c9d0e87);
    fn count(&self) -> &Count {
        &self.count
    }
    fn slot(&self, iid: &GUID) -> Option<*const c_void> {
        same_iid(iid, &IID_ITextRangeProvider).then(|| field(&self.range))
    }
}

const SIMPLE: usize = offset_of!(ProviderObject, simple);
const FRAGMENT: usize = offset_of!(ProviderObject, fragment);
const ROOT: usize = offset_of!(ProviderObject, root);
const INVOKE: usize = offset_of!(ProviderObject, invoke);
const VALUE: usize = offset_of!(ProviderObject, value);
const RANGE_VALUE: usize = offset_of!(ProviderObject, range_value);
const TOGGLE: usize = offset_of!(ProviderObject, toggle);
const SELECTION: usize = offset_of!(ProviderObject, selection);
const SELECTION_ITEM: usize = offset_of!(ProviderObject, selection_item);
const SCROLL_ITEM: usize = offset_of!(ProviderObject, scroll_item);
const TEXT: usize = offset_of!(ProviderObject, text);
const RANGE: usize = offset_of!(RangeObject, range);

static PROVIDER_IDENTITY: IInspectable_Vtbl = inspectable::<ProviderObject>();
static RANGE_IDENTITY: IInspectable_Vtbl = inspectable::<RangeObject>();

/// The element a slot at `OFFSET` was called through.
///
/// # Safety
/// The slot contract (module docs).
unsafe fn element<'a, const OFFSET: usize>(this: *mut c_void) -> &'a Provider {
    // SAFETY: by the slot contract, `this` is the field at `OFFSET`.
    unsafe { &com::object::<ProviderObject, OFFSET>(this).provider }
}

/// The range a slot was called through.
///
/// # Safety
/// The slot contract (module docs).
unsafe fn text_range<'a>(this: *mut c_void) -> &'a TextRange {
    // SAFETY: by the slot contract, `this` is the `range` field.
    unsafe { &com::object::<RangeObject, RANGE>(this).text }
}

/// S_OK, or the failure.
fn code(result: Result<()>) -> HRESULT {
    result.err().unwrap_or(S_OK)
}

static SIMPLE_VTBL: IRawElementProviderSimple_Vtbl = IRawElementProviderSimple_Vtbl {
    base__: unknown::<ProviderObject, SIMPLE>(),
    ProviderOptions: provider_options,
    GetPatternProvider: get_pattern_provider,
    GetPropertyValue: get_property_value,
    HostRawElementProvider: host_raw_element_provider,
};

unsafe extern "system" fn provider_options(
    this: *mut c_void,
    out: *mut ProviderOptions,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<SIMPLE>(this).provider_options()) }
}

unsafe extern "system" fn get_pattern_provider(
    this: *mut c_void,
    pattern: UIA_PATTERN_ID,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            element::<SIMPLE>(this)
                .get_pattern_provider(pattern)
                .map(raw)
        })
    }
}

unsafe extern "system" fn get_property_value(
    this: *mut c_void,
    id: UIA_PROPERTY_ID,
    out: *mut VARIANT,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            element::<SIMPLE>(this)
                .get_property_value(id)
                .map(Variant::into_raw)
        })
    }
}

unsafe extern "system" fn host_raw_element_provider(
    this: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            element::<SIMPLE>(this).host_raw_element_provider().map(raw)
        })
    }
}

static FRAGMENT_VTBL: IRawElementProviderFragment_Vtbl = IRawElementProviderFragment_Vtbl {
    base__: unknown::<ProviderObject, FRAGMENT>(),
    Navigate: navigate,
    GetRuntimeId: get_runtime_id,
    BoundingRectangle: bounding_rectangle,
    GetEmbeddedFragmentRoots: get_embedded_fragment_roots,
    SetFocus: set_focus,
    FragmentRoot: fragment_root,
};

unsafe extern "system" fn navigate(
    this: *mut c_void,
    direction: NavigateDirection,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            element::<FRAGMENT>(this).navigate(direction).map(raw)
        })
    }
}

unsafe extern "system" fn get_runtime_id(this: *mut c_void, out: *mut *mut SAFEARRAY) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put_nullable(out, || element::<FRAGMENT>(this).get_runtime_id()) }
}

unsafe extern "system" fn bounding_rectangle(this: *mut c_void, out: *mut UiaRect) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<FRAGMENT>(this).bounding_rectangle()) }
}

unsafe extern "system" fn get_embedded_fragment_roots(
    this: *mut c_void,
    out: *mut *mut SAFEARRAY,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            element::<FRAGMENT>(this).get_embedded_fragment_roots()
        })
    }
}

unsafe extern "system" fn set_focus(this: *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { element::<FRAGMENT>(this) }.set_focus())
}

unsafe extern "system" fn fragment_root(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            element::<FRAGMENT>(this).fragment_root().map(into_caller)
        })
    }
}

static ROOT_VTBL: IRawElementProviderFragmentRoot_Vtbl = IRawElementProviderFragmentRoot_Vtbl {
    base__: unknown::<ProviderObject, ROOT>(),
    ElementProviderFromPoint: element_provider_from_point,
    GetFocus: get_focus,
};

unsafe extern "system" fn element_provider_from_point(
    this: *mut c_void,
    x: f64,
    y: f64,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            element::<ROOT>(this)
                .element_provider_from_point(x, y)
                .map(raw)
        })
    }
}

unsafe extern "system" fn get_focus(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put_nullable(out, || element::<ROOT>(this).get_focus().map(raw)) }
}

static INVOKE_VTBL: IInvokeProvider_Vtbl = IInvokeProvider_Vtbl {
    base__: unknown::<ProviderObject, INVOKE>(),
    Invoke: invoke,
};

unsafe extern "system" fn invoke(this: *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { element::<INVOKE>(this) }.invoke())
}

static VALUE_VTBL: IValueProvider_Vtbl = IValueProvider_Vtbl {
    base__: unknown::<ProviderObject, VALUE>(),
    SetValue: set_value,
    Value: value,
    IsReadOnly: is_read_only,
};

unsafe extern "system" fn set_value(this: *mut c_void, value: PCWSTR) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { element::<VALUE>(this) }.set_value(value))
}

unsafe extern "system" fn value(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs); the caller frees the BSTR.
    unsafe {
        put(out, || {
            element::<VALUE>(this)
                .value()
                .map(|text| text.into_raw().cast_mut().cast())
        })
    }
}

unsafe extern "system" fn is_read_only(this: *mut c_void, out: *mut BOOL) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<VALUE>(this).is_read_only()) }
}

static RANGE_VALUE_VTBL: IRangeValueProvider_Vtbl = IRangeValueProvider_Vtbl {
    base__: unknown::<ProviderObject, RANGE_VALUE>(),
    SetValue: set_range_value,
    Value: range_value,
    IsReadOnly: range_is_read_only,
    Maximum: maximum,
    Minimum: minimum,
    LargeChange: change,
    SmallChange: change,
};

unsafe extern "system" fn set_range_value(this: *mut c_void, value: f64) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { element::<RANGE_VALUE>(this) }.set_range_value(value))
}

unsafe extern "system" fn range_value(this: *mut c_void, out: *mut f64) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<RANGE_VALUE>(this).range_value()) }
}

unsafe extern "system" fn range_is_read_only(this: *mut c_void, out: *mut BOOL) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<RANGE_VALUE>(this).is_read_only()) }
}

unsafe extern "system" fn maximum(this: *mut c_void, out: *mut f64) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<RANGE_VALUE>(this).maximum()) }
}

unsafe extern "system" fn minimum(this: *mut c_void, out: *mut f64) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<RANGE_VALUE>(this).minimum()) }
}

unsafe extern "system" fn change(this: *mut c_void, out: *mut f64) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<RANGE_VALUE>(this).change()) }
}

static TOGGLE_VTBL: IToggleProvider_Vtbl = IToggleProvider_Vtbl {
    base__: unknown::<ProviderObject, TOGGLE>(),
    Toggle: toggle,
    ToggleState: toggle_state,
};

unsafe extern "system" fn toggle(this: *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { element::<TOGGLE>(this) }.toggle())
}

unsafe extern "system" fn toggle_state(this: *mut c_void, out: *mut ToggleState) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<TOGGLE>(this).toggle_state()) }
}

static SELECTION_VTBL: ISelectionProvider_Vtbl = ISelectionProvider_Vtbl {
    base__: unknown::<ProviderObject, SELECTION>(),
    GetSelection: get_selection,
    CanSelectMultiple: no,
    IsSelectionRequired: no,
};

unsafe extern "system" fn get_selection(this: *mut c_void, out: *mut *mut SAFEARRAY) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<SELECTION>(this).get_selection()) }
}

/// `FALSE`: neither multiple nor required selection.
unsafe extern "system" fn no(_: *mut c_void, out: *mut BOOL) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || Ok(0)) }
}

static SELECTION_ITEM_VTBL: ISelectionItemProvider_Vtbl = ISelectionItemProvider_Vtbl {
    base__: unknown::<ProviderObject, SELECTION_ITEM>(),
    Select: select,
    AddToSelection: not_supported,
    RemoveFromSelection: not_supported,
    IsSelected: is_selected,
    SelectionContainer: selection_container,
};

unsafe extern "system" fn select(this: *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { element::<SELECTION_ITEM>(this) }.select())
}

/// A request the element refuses as not supported.
unsafe extern "system" fn not_supported(_: *mut c_void) -> HRESULT {
    NOT_SUPPORTED
}

unsafe extern "system" fn is_selected(this: *mut c_void, out: *mut BOOL) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<SELECTION_ITEM>(this).is_selected()) }
}

unsafe extern "system" fn selection_container(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            element::<SELECTION_ITEM>(this)
                .selection_container()
                .map(raw)
        })
    }
}

static SCROLL_ITEM_VTBL: IScrollItemProvider_Vtbl = IScrollItemProvider_Vtbl {
    base__: unknown::<ProviderObject, SCROLL_ITEM>(),
    ScrollIntoView: scroll_into_view,
};

unsafe extern "system" fn scroll_into_view(this: *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { element::<SCROLL_ITEM>(this) }.scroll_into_view())
}

static TEXT_VTBL: ITextProvider2_Vtbl = ITextProvider2_Vtbl {
    base__: ITextProvider_Vtbl {
        base__: unknown::<ProviderObject, TEXT>(),
        GetSelection: text_selection,
        GetVisibleRanges: visible_ranges,
        RangeFromChild: range_from_child,
        RangeFromPoint: range_from_point,
        DocumentRange: document_range,
        SupportedTextSelection: supported_text_selection,
    },
    RangeFromAnnotation: range_from_annotation,
    GetCaretRange: caret_range,
};

unsafe extern "system" fn text_selection(this: *mut c_void, out: *mut *mut SAFEARRAY) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<TEXT>(this).text_selection()) }
}

unsafe extern "system" fn visible_ranges(this: *mut c_void, out: *mut *mut SAFEARRAY) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<TEXT>(this).visible_ranges()) }
}

unsafe extern "system" fn range_from_child(
    this: *mut c_void,
    child: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            element::<TEXT>(this)
                .range_from_child(child)
                .map(into_caller)
        })
    }
}

unsafe extern "system" fn range_from_point(
    this: *mut c_void,
    point: UiaPoint,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            element::<TEXT>(this)
                .range_from_point(point)
                .map(into_caller)
        })
    }
}

unsafe extern "system" fn document_range(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            element::<TEXT>(this).document_range().map(into_caller)
        })
    }
}

unsafe extern "system" fn supported_text_selection(
    this: *mut c_void,
    out: *mut SupportedTextSelection,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || element::<TEXT>(this).supported_text_selection()) }
}

unsafe extern "system" fn range_from_annotation(
    _: *mut c_void,
    _: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || Err(NOT_SUPPORTED)) }
}

unsafe extern "system" fn caret_range(
    this: *mut c_void,
    active: *mut BOOL,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs); `active` is the caller's slot
    // or null, which `caret_range` refuses.
    unsafe {
        put(out, || {
            element::<TEXT>(this).caret_range(active).map(into_caller)
        })
    }
}

static RANGE_VTBL: ITextRangeProvider_Vtbl = ITextRangeProvider_Vtbl {
    base__: unknown::<RangeObject, RANGE>(),
    Clone: clone_range,
    Compare: compare,
    CompareEndpoints: compare_endpoints,
    ExpandToEnclosingUnit: expand_to_enclosing_unit,
    FindAttribute: find_attribute,
    FindText: find_text,
    GetAttributeValue: get_attribute_value,
    GetBoundingRectangles: get_bounding_rectangles,
    GetEnclosingElement: get_enclosing_element,
    GetText: get_text,
    Move: move_by,
    MoveEndpointByUnit: move_endpoint_by_unit,
    MoveEndpointByRange: move_endpoint_by_range,
    Select: select_range,
    AddToSelection: not_supported,
    RemoveFromSelection: not_supported,
    ScrollIntoView: scroll_range_into_view,
    GetChildren: get_children,
};

unsafe extern "system" fn clone_range(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || text_range(this).clone_range().map(into_caller)) }
}

unsafe extern "system" fn compare(
    this: *mut c_void,
    other: *mut c_void,
    out: *mut BOOL,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || text_range(this).compare(other)) }
}

unsafe extern "system" fn compare_endpoints(
    this: *mut c_void,
    end: TextPatternRangeEndpoint,
    other: *mut c_void,
    other_end: TextPatternRangeEndpoint,
    out: *mut i32,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            text_range(this).compare_endpoints(end, other, other_end)
        })
    }
}

unsafe extern "system" fn expand_to_enclosing_unit(this: *mut c_void, unit: TextUnit) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { text_range(this) }.expand_to_enclosing_unit(unit))
}

/// `value` arrives by value and stays the caller's: it is read, never cleared.
unsafe extern "system" fn find_attribute(
    this: *mut c_void,
    id: UIA_TEXTATTRIBUTE_ID,
    value: VARIANT,
    backward: BOOL,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            text_range(this)
                .find_attribute(id, &value, backward)
                .map(raw)
        })
    }
}

/// `text` is the caller's BSTR, read and kept.
unsafe extern "system" fn find_text(
    this: *mut c_void,
    text: *mut c_void,
    backward: BOOL,
    ignore_case: BOOL,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put_nullable(out, || {
            text_range(this)
                .find_text(text.cast_const().cast(), backward, ignore_case)
                .map(raw)
        })
    }
}

unsafe extern "system" fn get_attribute_value(
    this: *mut c_void,
    id: UIA_TEXTATTRIBUTE_ID,
    out: *mut VARIANT,
) -> HRESULT {
    if out.is_null() {
        return aterm_win32::Win32::Foundation::E_POINTER;
    }
    // SAFETY: the slot contract (module docs).
    match unsafe { text_range(this) }.get_attribute_value(id) {
        Ok(Some(value)) => {
            // SAFETY: the caller's writable slot; the VARIANT moves to it.
            unsafe { out.write(value.into_raw()) };
            S_OK
        }
        Ok(None) => S_OK,
        Err(hr) => hr,
    }
}

unsafe extern "system" fn get_bounding_rectangles(
    this: *mut c_void,
    out: *mut *mut SAFEARRAY,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || text_range(this).get_bounding_rectangles()) }
}

unsafe extern "system" fn get_enclosing_element(
    this: *mut c_void,
    out: *mut *mut c_void,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            text_range(this).get_enclosing_element().map(into_caller)
        })
    }
}

unsafe extern "system" fn get_text(this: *mut c_void, max: i32, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs); the caller frees the BSTR.
    unsafe {
        put(out, || {
            text_range(this)
                .get_text(max)
                .map(|text: Bstr| text.into_raw().cast_mut().cast())
        })
    }
}

unsafe extern "system" fn move_by(
    this: *mut c_void,
    unit: TextUnit,
    count: i32,
    out: *mut i32,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || text_range(this).move_by(unit, count)) }
}

unsafe extern "system" fn move_endpoint_by_unit(
    this: *mut c_void,
    end: TextPatternRangeEndpoint,
    unit: TextUnit,
    count: i32,
    out: *mut i32,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe {
        put(out, || {
            text_range(this).move_endpoint_by_unit(end, unit, count)
        })
    }
}

unsafe extern "system" fn move_endpoint_by_range(
    this: *mut c_void,
    end: TextPatternRangeEndpoint,
    other: *mut c_void,
    other_end: TextPatternRangeEndpoint,
) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { text_range(this) }.move_endpoint_by_range(end, other, other_end))
}

unsafe extern "system" fn select_range(this: *mut c_void) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { text_range(this) }.select())
}

unsafe extern "system" fn scroll_range_into_view(this: *mut c_void, _top: BOOL) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    code(unsafe { text_range(this) }.scroll_into_view())
}

unsafe extern "system" fn get_children(this: *mut c_void, out: *mut *mut SAFEARRAY) -> HRESULT {
    // SAFETY: the slot contract (module docs).
    unsafe { put(out, || text_range(this).get_children()) }
}
