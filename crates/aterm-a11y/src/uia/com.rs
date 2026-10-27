// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The provider's COM objects, written out by hand (WIN-UIA in
//! docs/AUDIT-third-party-2026-10-15.md): each object is `repr(C)`, an
//! `IInspectable`-shaped identity vtable pointer, then one vtable pointer per
//! interface it implements, an atomic reference count and its Rust state.
//!
//! `QueryInterface` answers what `windows` 0.62.2's `#[implement]` answered for
//! the same objects: `IUnknown`, `IInspectable` and `IAgileObject` with the
//! identity, each implemented interface with its own slot (an interface's base
//! with the derived slot), and `IMarshal` through the free-threaded marshaler,
//! aggregated, so UI Automation calls the objects from any thread. A private
//! interface ID answers with the identity too: a provider or range a client
//! hands back is recognised as one of these objects by it and by its identity
//! vtable. `IWeakReferenceSource`, the WinRT weak reference `#[implement]` also
//! answered, is not: nothing in UI Automation asks for it.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering, fence};

use aterm_win32::Win32::Foundation::{E_NOINTERFACE, E_OUTOFMEMORY, E_POINTER, S_OK};
use aterm_win32::Win32::System::Com::CoCreateFreeThreadedMarshaler;
use aterm_win32::com::{
    Com, IID_IAgileObject, IID_IInspectable, IID_IMarshal, IID_IUnknown, IInspectable_Vtbl,
    IUnknown_Vtbl, same_iid,
};
use aterm_win32::core::{GUID, HRESULT};

/// A failed call's `HRESULT`.
pub(super) type Result<T> = std::result::Result<T, HRESULT>;

/// One of this module's objects.
///
/// # Safety
/// `Self` is `repr(C)` and begins with a `*const IInspectable_Vtbl` field that
/// holds [`Object::identity`]; every pointer [`Object::slot`] returns is to one
/// of its own vtable-pointer fields, whose vtable's IUnknown slots are
/// [`unknown`]`::<Self, OFFSET>()` with that field's offset; and it lives only
/// in a `Box` made by [`into_raw`], released by its last reference.
pub(super) unsafe trait Object: Send + Sync + Sized + 'static {
    /// The identity vtable, whose address marks the type.
    fn identity() -> &'static IInspectable_Vtbl;
    /// The private interface ID that answers with the identity.
    const CAST: GUID;
    fn count(&self) -> &Count;
    /// The vtable-pointer field answering `iid`, if any.
    fn slot(&self, iid: &GUID) -> Option<*const c_void>;
}

/// An object's reference count and its aggregated free-threaded marshaler.
pub(super) struct Count {
    refs: AtomicU32,
    marshaler: AtomicPtr<c_void>,
}

impl Count {
    /// One reference: the creator's.
    pub(super) const fn one() -> Self {
        Self {
            refs: AtomicU32::new(1),
            marshaler: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

impl Drop for Count {
    fn drop(&mut self) {
        let inner = *self.marshaler.get_mut();
        if !inner.is_null() {
            // SAFETY: the marshaler's own (non-delegating) IUnknown, owned by
            // this count since `marshal` stored it; released once, here.
            unsafe { (vtable(inner).Release)(inner) };
        }
    }
}

/// The IUnknown slots every vtable begins with.
///
/// # Safety
/// `this` is a live interface pointer.
unsafe fn vtable<'a>(this: *mut c_void) -> &'a IUnknown_Vtbl {
    // SAFETY: every COM object begins with its vtable pointer, whose first
    // three slots are IUnknown's.
    unsafe { &**this.cast::<*const IUnknown_Vtbl>() }
}

/// The object `this`, a pointer to its field at `OFFSET`, belongs to.
///
/// # Safety
/// `this` points at the field at `OFFSET` of a live `O`.
pub(super) unsafe fn object<'a, O, const OFFSET: usize>(this: *mut c_void) -> &'a O {
    // SAFETY: by the caller's contract, `OFFSET` bytes back is the object.
    unsafe { &*this.byte_sub(OFFSET).cast::<O>() }
}

/// Boxes `object` (its count is [`Count::one`]) and hands out that reference
/// through its field at `offset`.
pub(super) fn into_raw<O: Object>(object: O, offset: usize) -> Com {
    let raw = Box::into_raw(Box::new(object));
    // SAFETY: `offset` is one of `O`'s vtable-pointer fields (the callers name
    // it with `offset_of!`), so the pointer is that interface's; the box's one
    // reference moves into the `Com`.
    unsafe { Com::from_raw(raw.byte_add(offset).cast()) }
        .unwrap_or_else(|| unreachable!("a box is never null"))
}

/// IUnknown's slots for `O`'s field at `OFFSET`.
pub(super) const fn unknown<O: Object, const OFFSET: usize>() -> IUnknown_Vtbl {
    IUnknown_Vtbl {
        QueryInterface: query::<O, OFFSET>,
        AddRef: add_ref::<O, OFFSET>,
        Release: release::<O, OFFSET>,
    }
}

/// The identity vtable for `O`: IUnknown's slots and IInspectable's, which
/// answer as `#[implement]`'s did for a type with no runtime name.
pub(super) const fn inspectable<O: Object>() -> IInspectable_Vtbl {
    IInspectable_Vtbl {
        base: unknown::<O, 0>(),
        GetIids: get_iids,
        GetRuntimeClassName: get_runtime_class_name,
        GetTrustLevel: get_trust_level,
    }
}

unsafe extern "system" fn query<O: Object, const OFFSET: usize>(
    this: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM calls this slot through the field at `OFFSET` of a live `O`
    // (`unknown`'s contract), with a readable `iid`.
    let (object, iid) = unsafe { (object::<O, OFFSET>(this), &*iid) };
    let found = if same_iid(iid, &IID_IUnknown)
        || same_iid(iid, &IID_IInspectable)
        || same_iid(iid, &IID_IAgileObject)
        || same_iid(iid, &O::CAST)
    {
        ptr::from_ref(object).cast::<c_void>()
    } else if let Some(slot) = object.slot(iid) {
        slot
    } else if same_iid(iid, &IID_IMarshal) {
        // SAFETY: `out` is writable, checked above.
        return unsafe { marshal(object, out) };
    } else {
        // SAFETY: as above.
        unsafe { out.write(ptr::null_mut()) };
        return E_NOINTERFACE;
    };
    object.count().refs.fetch_add(1, Ordering::Relaxed);
    // SAFETY: as above; the reference just taken is the caller's.
    unsafe { out.write(found.cast_mut()) };
    S_OK
}

/// `IMarshal`, from the free-threaded marshaler aggregated on first request.
///
/// # Safety
/// `out` is writable.
unsafe fn marshal<O: Object>(object: &O, out: *mut *mut c_void) -> HRESULT {
    let count = object.count();
    let mut inner = count.marshaler.load(Ordering::Acquire);
    if inner.is_null() {
        let mut made = ptr::null_mut();
        let outer = ptr::from_ref(object).cast_mut().cast::<c_void>();
        // SAFETY: `outer` is this object's identity, which outlives the
        // aggregate (its count releases it); `made` receives the marshaler's
        // non-delegating IUnknown.
        let hr = unsafe { CoCreateFreeThreadedMarshaler(outer, &mut made) };
        if hr < 0 || made.is_null() {
            // SAFETY: the caller's writable `out`.
            unsafe { out.write(ptr::null_mut()) };
            return if hr < 0 { hr } else { E_OUTOFMEMORY };
        }
        match count.marshaler.compare_exchange(
            ptr::null_mut(),
            made,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => inner = made,
            Err(won) => {
                // SAFETY: another thread's marshaler won; this one is released
                // through its own non-delegating IUnknown, never handed out.
                unsafe { (vtable(made).Release)(made) };
                inner = won;
            }
        }
    }
    // SAFETY: the stored marshaler is live while the object is; its
    // `IMarshal` delegates its IUnknown to this object, so the reference the
    // query takes is the object's.
    unsafe { (vtable(inner).QueryInterface)(inner, &IID_IMarshal, out) }
}

unsafe extern "system" fn add_ref<O: Object, const OFFSET: usize>(this: *mut c_void) -> u32 {
    // SAFETY: COM calls this slot through the field at `OFFSET` of a live `O`.
    let object = unsafe { object::<O, OFFSET>(this) };
    object
        .count()
        .refs
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
}

unsafe extern "system" fn release<O: Object, const OFFSET: usize>(this: *mut c_void) -> u32 {
    // SAFETY: COM calls this slot through the field at `OFFSET` of a live `O`,
    // with a reference of its own to give back.
    let object = unsafe { object::<O, OFFSET>(this) };
    let left = object
        .count()
        .refs
        .fetch_sub(1, Ordering::Release)
        .wrapping_sub(1);
    if left == 0 {
        fence(Ordering::Acquire);
        // SAFETY: that was the last reference to the box `into_raw` made.
        drop(unsafe { Box::from_raw(ptr::from_ref(object).cast_mut()) });
    }
    left
}

unsafe extern "system" fn get_iids(
    _: *mut c_void,
    count: *mut u32,
    values: *mut *mut GUID,
) -> HRESULT {
    if count.is_null() || values.is_null() {
        return E_POINTER;
    }
    // SAFETY: both are the caller's writable slots, checked non-null above.
    unsafe {
        count.write(0);
        values.write(ptr::null_mut());
    }
    S_OK
}

unsafe extern "system" fn get_runtime_class_name(
    _: *mut c_void,
    value: *mut *mut c_void,
) -> HRESULT {
    if value.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's writable slot; the empty name is the null HSTRING.
    unsafe { value.write(ptr::null_mut()) };
    S_OK
}

unsafe extern "system" fn get_trust_level(_: *mut c_void, value: *mut i32) -> HRESULT {
    if value.is_null() {
        return E_POINTER;
    }
    // SAFETY: the caller's writable slot; 0 is BaseTrust.
    unsafe { value.write(0) };
    S_OK
}

/// One of this module's `O`s, held by one reference.
pub(super) struct Held<O> {
    _reference: Com,
    object: NonNull<O>,
}

impl<O> Deref for Held<O> {
    type Target = O;
    fn deref(&self) -> &O {
        // SAFETY: the reference `self` holds keeps the object alive.
        unsafe { self.object.as_ref() }
    }
}

/// The `O` behind `interface`, an interface pointer a client handed in
/// (borrowed), or `None` when it is null or not one of this module's `O`s.
///
/// # Safety
/// A non-null `interface` is a live interface pointer.
pub(super) unsafe fn held<O: Object>(interface: *mut c_void) -> Option<Held<O>> {
    // SAFETY: the caller's live pointer, borrowed: `ManuallyDrop` keeps this
    // view from releasing the reference it does not own.
    let borrowed = ManuallyDrop::new(unsafe { Com::from_raw(interface) }?);
    let identity = borrowed.query(&O::CAST).ok()?;
    // SAFETY: a successful query hands back a live interface pointer, which
    // begins with its vtable pointer.
    let vtable = unsafe { *identity.as_raw().cast::<*const IInspectable_Vtbl>() };
    if !ptr::eq(vtable, O::identity()) {
        return None;
    }
    let object = NonNull::new(identity.as_raw().cast::<O>())?;
    Some(Held {
        _reference: identity,
        object,
    })
}

/// One owned reference to one of this module's objects, which are agile:
/// callable and releasable from any thread.
pub(super) struct Agile(Com);

// SAFETY: the objects behind an `Agile` are `Send + Sync` (`Object`'s bound),
// count references atomically, and answer `IAgileObject`.
unsafe impl Send for Agile {}
// SAFETY: as above.
unsafe impl Sync for Agile {}

impl Agile {
    /// `com`, which must be one of this module's objects.
    pub(super) const fn new(com: Com) -> Self {
        Self(com)
    }
}

impl Deref for Agile {
    type Target = Com;
    fn deref(&self) -> &Com {
        &self.0
    }
}

/// Writes `f`'s value to the caller's `out`, or returns its failure and leaves
/// `out` as it was; a null `out` is `E_POINTER`, and `f` does not run.
///
/// # Safety
/// A non-null `out` is the caller's writable slot.
pub(super) unsafe fn put<T>(out: *mut T, f: impl FnOnce() -> Result<T>) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    match f() {
        Ok(value) => {
            // SAFETY: the caller's writable slot; ownership of `value` moves
            // to the caller.
            unsafe { out.write(value) };
            S_OK
        }
        Err(hr) => hr,
    }
}

/// A nullable result: `out` is nulled before `f` runs, so an absent value (a
/// null pointer) and a failure both leave it null.
///
/// # Safety
/// As [`put`].
pub(super) unsafe fn put_nullable<T>(
    out: *mut *mut T,
    f: impl FnOnce() -> Result<*mut T>,
) -> HRESULT {
    // SAFETY: the caller's slot, by this function's contract.
    if !unsafe { crate::uia_abi::initialize_nullable_out(out) } {
        return E_POINTER;
    }
    match f() {
        Ok(value) => {
            // SAFETY: as above; ownership of `value` moves to the caller.
            unsafe { out.write(value) };
            S_OK
        }
        Err(hr) => hr,
    }
}

/// A nullable interface: `None` is null.
pub(super) fn raw(com: Option<Com>) -> *mut c_void {
    com.map_or(ptr::null_mut(), into_caller)
}

/// Hands `com`'s reference to the caller.
pub(super) fn into_caller(com: Com) -> *mut c_void {
    ManuallyDrop::new(com).as_raw()
}
