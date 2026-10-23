// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The raw runtime surface: symbols, typed sends, ownership, pools.

use std::ffi::{CStr, c_char, c_uint, c_void};

/// An Objective-C object pointer.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(*mut c_void);

impl Id {
    /// The Objective-C `nil`.
    pub const NIL: Self = Self(std::ptr::null_mut());

    /// Wrap a raw object pointer.
    #[inline]
    #[must_use]
    pub const fn from_ptr(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    /// The raw object pointer.
    #[inline]
    #[must_use]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Whether this is `nil`.
    #[inline]
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// The address, with provenance exposed — what ivar arithmetic needs.
    #[inline]
    #[must_use]
    pub fn expose_provenance(self) -> usize {
        self.0.expose_provenance()
    }

    /// The address without exposing provenance, for identity comparisons and `-hash` (the instance pointer on `NSObject`).
    #[inline]
    #[must_use]
    pub fn addr(self) -> usize {
        self.0.addr()
    }

    /// The same address as a `*mut T`, for the zero-sized markers [`crate::declare_class!`] mints.
    #[inline]
    #[must_use]
    pub const fn cast<T>(self) -> *mut T {
        self.0.cast()
    }
}

impl std::fmt::Debug for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_null() {
            return f.write_str("id(nil)");
        }
        write!(f, "id({:p})", self.0)
    }
}
/// An Objective-C selector — an interned, immortal string the runtime owns.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sel(*const c_void);

impl Sel {
    /// The nil selector.
    pub const NULL: Self = Self(std::ptr::null());

    /// The raw selector pointer.
    #[inline]
    #[must_use]
    pub const fn as_ptr(self) -> *const c_void {
        self.0
    }

    /// Whether this is the null selector — what an unresolved lookup returns.
    #[inline]
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// The selector's name, borrowed from the runtime's immortal selector table.
    #[must_use]
    pub fn name(self) -> &'static CStr {
        if self.0.is_null() {
            return c"(null)";
        }
        // SAFETY: non-null selector from the runtime; `sel_getName` returns a NUL-terminated string in the immortal selector table, so `'static`.
        unsafe { CStr::from_ptr(sel_getName(self)) }
    }

    /// Wrap a pointer the runtime returned.
    #[inline]
    pub(crate) const fn from_ptr(ptr: *const c_void) -> Self {
        Self(ptr)
    }
}

// SAFETY: a selector points into libobjc's immortal selector table: never freed or written, interning is idempotent and synchronised, so the pointer is valid on every thread for the life of the process.
unsafe impl Send for Sel {}
// SAFETY: as above — the pointee is immutable and immortal.
unsafe impl Sync for Sel {}

impl std::fmt::Debug for Sel {
    /// Prints the selector's name via `sel_getName`; that a `SEL` points at its
    /// own name is an objc4 implementation detail, not ABI.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_null() {
            return f.write_str("Sel(null)");
        }
        // SAFETY: non-null selector from `sel_registerName`; `sel_getName` returns a NUL-terminated string the runtime never frees.
        let name = unsafe { CStr::from_ptr(sel_getName(*self)) };
        write!(f, "Sel({})", name.to_string_lossy())
    }
}

/// An Objective-C class object.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClassPtr(*mut c_void);

impl ClassPtr {
    /// The null class — what a failed [`class`] lookup returns.
    pub const NULL: Self = Self(std::ptr::null_mut());

    /// Wrap a raw class pointer.
    ///
    /// Safe code cannot build one from an arbitrary pointer:
    ///
    /// ```compile_fail,E0133
    /// let bogus = std::ptr::without_provenance_mut::<std::ffi::c_void>(0x10);
    /// let cls = aterm_objc::ClassPtr::from_ptr(bogus);
    /// ```
    ///
    /// # Safety
    /// `ptr` must be null or a class object the Objective-C runtime has registered (from `objc_getClass`, `object_getClass`, `objc_registerClassPair` or an address one of them returned).
    #[inline]
    #[must_use]
    pub const unsafe fn from_ptr(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    /// The raw class pointer.
    #[inline]
    #[must_use]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Whether the lookup failed (or the receiver was `nil`).
    #[inline]
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0.is_null()
    }

    /// The address with provenance exposed — what [`crate::ClassMeta`] stores so it can live in a `OnceLock`.
    #[inline]
    #[must_use]
    pub fn expose_provenance(self) -> usize {
        self.0.expose_provenance()
    }

    /// The address without exposing provenance, for identity assertions.
    #[inline]
    #[must_use]
    pub fn addr(self) -> usize {
        self.0.addr()
    }

    /// This class as an object, for class messages (`+alloc`, `+sharedWorkspace`).
    #[inline]
    #[must_use]
    pub const fn as_id(self) -> Id {
        Id::from_ptr(self.0)
    }
}

impl std::fmt::Debug for ClassPtr {
    /// Prints the class's name via `class_getName`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: the type's invariant: `self` is null (reported as `"(nil)"`
        // without a dereference) or a registered class object, which is
        // immortal. No safe constructor admits anything else.
        let name = unsafe { class_name(*self) };
        write!(f, "Class({})", name.to_string_lossy())
    }
}

/// An Objective-C protocol object, as returned by `objc_getProtocol`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProtocolPtr(*mut c_void);

impl ProtocolPtr {
    /// Wrap a raw protocol pointer.
    #[inline]
    #[must_use]
    pub const fn from_ptr(ptr: *mut c_void) -> Self {
        Self(ptr)
    }

    /// The raw protocol pointer.
    #[inline]
    #[must_use]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Whether the protocol is absent from this process.
    #[inline]
    #[must_use]
    pub const fn is_null(self) -> bool {
        self.0.is_null()
    }
}

impl std::fmt::Debug for ProtocolPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Protocol({:p})", self.0)
    }
}

/// An `Ivar` handle, as returned by `class_getInstanceVariable`.
pub type IvarPtr = *const c_void;

/// The `objc_super` struct `objc_msgSendSuper` takes in place of a receiver.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ObjcSuper {
    /// The object the message is actually delivered to.
    pub receiver: Id,
    /// Where method lookup starts (the superclass of the defining class).
    pub super_class: ClassPtr,
}

#[link(name = "objc")]
unsafe extern "C-unwind" {
    /// Declared with no parameter list on purpose.
    fn objc_msgSend();
    /// Same discipline as [`objc_msgSend`], but the first argument is a
    /// `*const ObjcSuper` rather than the receiver.
    fn objc_msgSendSuper();
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> ClassPtr;
    fn objc_getProtocol(name: *const c_char) -> ProtocolPtr;
    fn object_getClass(obj: Id) -> ClassPtr;
    fn class_getSuperclass(cls: ClassPtr) -> ClassPtr;
    fn class_getName(cls: ClassPtr) -> *const c_char;
    fn sel_registerName(name: *const c_char) -> Sel;
    /// The selector's name, for [`Sel`]'s `Debug`.
    fn sel_getName(sel: Sel) -> *const c_char;
    fn objc_retain(obj: Id) -> Id;
    fn objc_release(obj: Id);
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
    /// Hand a +1 reference to the innermost pool, for [`autorelease`]: a
    /// declared method returning an object not named
    /// `new*`/`alloc`/`copy*`/`mutableCopy*` owes its caller +0.
    fn objc_autorelease(obj: Id) -> Id;

    pub(crate) fn objc_allocateClassPair(
        superclass: ClassPtr,
        name: *const c_char,
        extra_bytes: usize,
    ) -> ClassPtr;
    pub(crate) fn objc_registerClassPair(cls: ClassPtr);
    pub(crate) fn class_addMethod(
        cls: ClassPtr,
        name: Sel,
        imp: *const c_void,
        types: *const c_char,
    ) -> crate::encode::Bool;
    pub(crate) fn class_addIvar(
        cls: ClassPtr,
        name: *const c_char,
        size: usize,
        alignment: u8,
        types: *const c_char,
    ) -> crate::encode::Bool;
    pub(crate) fn class_addProtocol(cls: ClassPtr, protocol: ProtocolPtr) -> crate::encode::Bool;
    fn objc_allocateProtocol(name: *const c_char) -> ProtocolPtr;
    fn objc_registerProtocol(proto: ProtocolPtr);
    pub(crate) fn class_getInstanceVariable(cls: ClassPtr, name: *const c_char) -> IvarPtr;
    pub(crate) fn ivar_getOffset(ivar: IvarPtr) -> isize;

    pub(crate) fn class_getInstanceMethod(cls: ClassPtr, name: Sel) -> *const c_void;
    pub(crate) fn method_getTypeEncoding(method: *const c_void) -> *const c_char;

    pub(crate) fn method_getImplementation(method: *const c_void) -> *const c_void;

    pub(crate) fn method_setImplementation(
        method: *const c_void,
        imp: *const c_void,
    ) -> *const c_void;

    fn class_copyMethodList(cls: ClassPtr, out_count: *mut c_uint) -> *mut *const c_void;
    fn method_getName(method: *const c_void) -> Sel;
    fn class_copyProtocolList(cls: ClassPtr, out_count: *mut c_uint) -> *mut ProtocolPtr;
    fn protocol_getName(proto: ProtocolPtr) -> *const c_char;

    fn protocol_getMethodDescription(
        proto: ProtocolPtr,
        sel: Sel,
        is_required_method: crate::encode::Bool,
        is_instance_method: crate::encode::Bool,
    ) -> ObjcMethodDescription;

    fn _protocol_getMethodTypeEncoding(
        proto: ProtocolPtr,
        sel: Sel,
        is_required_method: crate::encode::Bool,
        is_instance_method: crate::encode::Bool,
    ) -> *const c_char;
}

unsafe extern "C" {
    fn free(ptr: *mut c_void);
}

/// `struct objc_method_description` — a selector and its type string.
#[repr(C)]
#[derive(Clone, Copy)]
struct ObjcMethodDescription {
    name: Sel,
    types: *const c_char,
}

#[cfg(target_arch = "x86_64")]
#[link(name = "objc")]
unsafe extern "C-unwind" {
    /// Same discipline and `"C-unwind"` ABI as [`objc_msgSend`].
    fn objc_msgSend_stret();
    /// Same, against the super entry point.
    fn objc_msgSendSuper_stret();
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

/// A message-send prototype, decomposed far enough that [`msg`] can read its return type and pick the right entry point.
///
/// Fourteen arguments are sendable (`tests/declared_abi.rs`); fifteen do not
/// compile:
///
/// ```compile_fail
/// # use aterm_objc::{Id, Sel, msg};
/// // 17 parameters: `self`, `_cmd`, and fifteen arguments.
/// let _: unsafe extern "C-unwind" fn(
///     Id, Sel,
///     i64, i64, i64, i64, i64, i64, i64, i64,
///     i64, i64, i64, i64, i64, i64, i64,
/// ) -> i64 = unsafe { msg() };
/// ```
///
/// There is no impl for `unsafe extern "C" fn(..)`: a `"C"` send is
/// `nounwind`, so an `NSException` raised inside it skips the calling frame's
/// `Drop`s or aborts (see the note on `objc_msgSend`):
///
/// ```compile_fail
/// # use aterm_objc::{Id, Sel, msg};
/// let _: unsafe extern "C" fn(Id, Sel) -> Id = unsafe { msg() };
/// ```
///
/// # Safety
/// An implementor must be a bare `unsafe extern "C-unwind"` function pointer whose return type is `Ret`: [`msg`] transmutes a symbol address into `Self` and dispatches on `size_of::<Self::Ret>()`, so a lying `Ret` picks the wrong entry point.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a message-send prototype `aterm-objc` can build",
    label = "not an `unsafe extern \"C-unwind\" fn(..) -> R` of at most 16 parameters",
    note = "a prototype must be a bare `unsafe extern \"C-unwind\"` function pointer whose             first two parameters are the implicit `self` and `_cmd` — e.g.             `unsafe extern \"C-unwind\" fn(Id, Sel, i64) -> Bool`. Plain `extern \"C\"`             is refused on purpose: an NSException raised inside such a send skips             the frame's destructors (measured; see `aterm_objc::exception`)",
    note = "the ceiling is SIXTEEN parameters INCLUDING `self` and `_cmd`, i.e. a             fourteen-colon selector; a wider one has no `MsgFn` impl and             `declare_class!` refuses to register it for the same reason"
)]
pub unsafe trait MsgFn: Copy {
    /// The prototype's return type.
    type Ret;
}

macro_rules! impl_msg_fn {
    ($($arg:ident),*) => {
        // SAFETY: the implementing type is an `unsafe extern "C-unwind"` function pointer and `Ret` is its own return type.
        unsafe impl<Ret, $($arg),*> MsgFn for unsafe extern "C-unwind" fn($($arg),*) -> Ret {
            type Ret = Ret;
        }
    };
}
impl_msg_fn!();
impl_msg_fn!(A0);
impl_msg_fn!(A0, A1);
impl_msg_fn!(A0, A1, A2);
impl_msg_fn!(A0, A1, A2, A3);
impl_msg_fn!(A0, A1, A2, A3, A4);
impl_msg_fn!(A0, A1, A2, A3, A4, A5);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_msg_fn!(
    A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14
);
impl_msg_fn!(
    A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15
);

/// Whether `R` comes back through a hidden pointer on this target's C ABI, which decides whether a send must go through `objc_msgSend_stret`.
#[inline]
#[must_use]
pub const fn returns_indirectly<R: crate::encode::Encode>() -> bool {
    cfg!(target_arch = "x86_64") && (size_of::<R>() > 16 || R::HAS_UNALIGNED_FIELDS)
}

/// `objc_msgSend_stret`, on the one architecture that has it.
#[cfg(target_arch = "x86_64")]
#[inline]
fn stret_entry() -> *const c_void {
    objc_msgSend_stret as *const c_void
}

/// Unreachable off x86_64: [`returns_indirectly`] is `const false` there, so
/// the call folds away.
#[cfg(not(target_arch = "x86_64"))]
#[inline]
fn stret_entry() -> *const c_void {
    unreachable!("objc_msgSend_stret exists only on x86_64")
}

/// `objc_msgSendSuper_stret`; see [`stret_entry`].
#[cfg(target_arch = "x86_64")]
#[inline]
fn super_stret_entry() -> *const c_void {
    objc_msgSendSuper_stret as *const c_void
}

/// See [`stret_entry`].
#[cfg(not(target_arch = "x86_64"))]
#[inline]
fn super_stret_entry() -> *const c_void {
    unreachable!("objc_msgSendSuper_stret exists only on x86_64")
}

/// Cast the untyped `objc_msgSend` to a concrete prototype, choosing the `_stret` entry point when `F`'s return type needs it.
///
/// A return type whose ABI nobody has stated does not compile:
///
/// ```compile_fail
/// # use aterm_objc::{Id, Sel, msg};
/// #[repr(C, packed)]
/// struct Packed { c: i8, q: i64 }
/// // Nine bytes, so `size_of` says "registers"; clang says
/// // `_objc_msgSend_stret`. Without an `Encode` impl stating which, this is
/// // `E0277` rather than a wrong entry point.
/// let _: unsafe extern "C-unwind" fn(Id, Sel) -> Packed = unsafe { msg() };
/// ```
///
/// Nor does a `BOOL` return spelled as a Rust `bool`:
///
/// ```compile_fail
/// # use aterm_objc::{Id, Sel, msg};
/// let _: unsafe extern "C-unwind" fn(Id, Sel) -> bool = unsafe { msg() };
/// ```
///
/// # Safety
/// `F` must be the exact C prototype of the selector about to be sent, including the implicit `(self, _cmd)` pair.
#[inline]
#[must_use]
pub unsafe fn msg<F: MsgFn>() -> F
where
    <F as MsgFn>::Ret: crate::encode::Encode,
{
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    let entry = if const { returns_indirectly::<<F as MsgFn>::Ret>() } {
        stret_entry()
    } else {
        objc_msgSend as *const c_void
    };
    // SAFETY: `entry` is a function symbol's address and `F` is pointer-wide. `F` supplies the
    // prototype; the entry point matches its return class.
    unsafe { std::mem::transmute_copy(&entry) }
}

/// Cast the untyped `objc_msgSendSuper` to a concrete prototype, with the same automatic `_stret` selection as [`msg`].
/// # Safety
/// `F` must be the exact C prototype of the selector, with `*const ObjcSuper` in the receiver position and `Sel` second; otherwise as [`msg`].
#[inline]
#[must_use]
pub unsafe fn msg_super<F: MsgFn>() -> F
where
    <F as MsgFn>::Ret: crate::encode::Encode,
{
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    let entry = if const { returns_indirectly::<<F as MsgFn>::Ret>() } {
        super_stret_entry()
    } else {
        objc_msgSendSuper as *const c_void
    };
    // SAFETY: identical to `msg`, against the super entry points.
    unsafe { std::mem::transmute_copy(&entry) }
}

/// The type encoding the runtime holds for `cls`'s instance method `name`, or `None` if neither it nor a superclass implements it.
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn method_types(cls: ClassPtr, name: Sel) -> Option<String> {
    // SAFETY: `cls` is live; `name` is interned; an unimplemented selector yields NULL.
    let method = unsafe { class_getInstanceMethod(cls, name) };
    if method.is_null() {
        return None;
    }
    // SAFETY: `method` is a live `Method`; the encoding string is owned by the runtime for the class's life, and may be null (checked).
    let types = unsafe { method_getTypeEncoding(method) };
    if types.is_null() {
        return None;
    }
    // SAFETY: as above — a non-null, runtime-owned, NUL-terminated string.
    Some(
        unsafe { CStr::from_ptr(types) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// The IMP the runtime will call for `name` on `cls`, or `None` if the class does not respond to it.
/// # Safety
/// `cls` must be a live class object or null.
#[must_use]
pub unsafe fn method_imp(cls: ClassPtr, name: Sel) -> Option<*const c_void> {
    // SAFETY: `cls` is live or null; `class_getInstanceMethod` answers NULL for null or an unimplemented selector and walks the superclass chain.
    let method = unsafe { class_getInstanceMethod(cls, name) };
    if method.is_null() {
        return None;
    }
    // SAFETY: `method` is a live `Method` handle from the call above.
    let imp = unsafe { method_getImplementation(method) };
    (!imp.is_null()).then_some(imp)
}

/// The type encoding a protocol declares for `name`, or `None` if it does not.
/// # Safety
/// `proto` must be a live protocol object or nil (answers `None`).
#[must_use]
pub unsafe fn protocol_method_types(
    proto: ProtocolPtr,
    name: Sel,
    instance_method: bool,
) -> Option<String> {
    if proto.is_null() {
        return None;
    }
    let is_instance = crate::encode::Bool::new(instance_method);
    // SAFETY: `proto` is live; `name` is interned. An undeclared selector yields a zeroed
    // description, so `types` is null-checked.
    let mut desc = unsafe {
        protocol_getMethodDescription(proto, name, crate::encode::Bool::YES, is_instance)
    };
    if desc.types.is_null() {
        // SAFETY: as above, against the optional table, where delegate rows usually live.
        desc = unsafe {
            protocol_getMethodDescription(proto, name, crate::encode::Bool::NO, is_instance)
        };
    }
    if desc.types.is_null() {
        return None;
    }
    // SAFETY: a non-null, runtime-owned, NUL-terminated string, immortal for the life of the protocol object.
    Some(
        unsafe { CStr::from_ptr(desc.types) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// The EXTENDED type encoding a protocol declares for `name`, or `None` if it does not: [`protocol_method_types`]' row with each object's class and each block's signature, which is the one place the runtime records a block's prototype.
/// # Safety
/// `proto` must be a live protocol object or nil (answers `None`).
#[must_use]
pub unsafe fn protocol_method_extended_types(
    proto: ProtocolPtr,
    name: Sel,
    instance_method: bool,
) -> Option<String> {
    if proto.is_null() {
        return None;
    }
    let is_instance = crate::encode::Bool::new(instance_method);
    [crate::encode::Bool::YES, crate::encode::Bool::NO]
        .into_iter()
        .find_map(|required| {
            // SAFETY: `proto` is live; `name` is interned. An undeclared selector answers NULL.
            let types =
                unsafe { _protocol_getMethodTypeEncoding(proto, name, required, is_instance) };
            // SAFETY: a non-null, runtime-owned, NUL-terminated string, immortal for the life of the protocol object.
            (!types.is_null()).then(|| {
                unsafe { CStr::from_ptr(types) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
}

/// Resolve a selector from a checked C-string literal, e.g. `sel(c"length")`.
#[inline]
#[must_use]
pub fn sel(name: &'static CStr) -> Sel {
    // SAFETY: `CStr` guarantees one trailing NUL and no interior NUL; `sel_registerName` copies it into the runtime's immortal selector table.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// Look up a class from a checked C-string literal, e.g. `class(c"NSString")`.
#[inline]
#[must_use]
pub fn class(name: &'static CStr) -> ClassPtr {
    // SAFETY: `CStr` gives a valid C string. Classes are immortal; the pointer is never released.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// Look up a protocol by name, e.g. `protocol(c"NSMenuDelegate")`.
#[inline]
#[must_use]
pub fn protocol(name: &'static CStr) -> ProtocolPtr {
    // SAFETY: `CStr` gives a valid C string. Protocol objects are immortal and never released.
    unsafe { objc_getProtocol(name.as_ptr()) }
}

/// The names of every protocol this crate registered because no loaded image had, in registration order, each once.
#[must_use]
pub fn protocols_registered_by_aterm() -> Vec<&'static CStr> {
    registered()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The registry behind [`protocols_registered_by_aterm`], and the lock that
/// serializes [`protocol_or_register`].
fn registered() -> &'static std::sync::Mutex<Vec<&'static CStr>> {
    static REGISTERED: std::sync::Mutex<Vec<&'static CStr>> = std::sync::Mutex::new(Vec::new());
    &REGISTERED
}

/// The protocol object for `name`, registered by this crate if no loaded image registers it.
/// # Panics
/// If the runtime refuses to register (allocation nil although the lookup found nothing, or the lookup still nil after registration).
#[must_use]
pub fn protocol_or_register(name: &'static CStr) -> ProtocolPtr {
    let found = protocol(name);
    if !found.is_null() {
        return found;
    }
    let mut registered = registered()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let found = protocol(name);
    if !found.is_null() {
        return found;
    }
    // SAFETY: `name` is a valid C string; `objc_allocateProtocol` returns a fresh, unregistered protocol object, or nil if the name is taken.
    let proto = unsafe { objc_allocateProtocol(name.as_ptr()) };
    assert!(
        !proto.is_null(),
        "aterm-objc: objc_allocateProtocol({name:?}) answered nil although \
         objc_getProtocol found nothing under that name"
    );
    // SAFETY: `proto` is a live, allocated, unregistered protocol object, and registering it is the documented terminal operation on one.
    unsafe { objc_registerProtocol(proto) };
    let now = protocol(name);
    assert!(
        !now.is_null(),
        "aterm-objc: protocol {name:?} is still absent after objc_registerProtocol"
    );
    registered.push(name);
    now
}

/// The class of a live object (`object_getClass`).
/// # Safety
/// `obj` must be a live object pointer, or null.
#[inline]
#[must_use]
pub unsafe fn class_of(obj: Id) -> ClassPtr {
    // SAFETY: the caller pins `obj` as live-or-null; `object_getClass` reads only the isa field and returns an immortal class pointer.
    unsafe { object_getClass(obj) }
}

/// A class's superclass.
/// # Safety
/// `cls` must be a live class object, or null.
#[must_use]
pub unsafe fn superclass_of(cls: ClassPtr) -> ClassPtr {
    // SAFETY: class pointers are immortal once registered; the accessor has no side effects and tolerates nil.
    unsafe { class_getSuperclass(cls) }
}

/// A class's name, borrowed from the runtime's immortal storage.
/// # Safety
/// `cls` must be a live class object, or null (which reports `"(nil)"`).
#[must_use]
pub unsafe fn class_name(cls: ClassPtr) -> &'static CStr {
    if cls.is_null() {
        return c"(nil)";
    }
    // SAFETY: `class_getName` returns a NUL-terminated string owned by the runtime for a registered class; it outlives the process.
    unsafe { CStr::from_ptr(class_getName(cls)) }
}

/// Every instance method a class registered itself, with the type encoding the runtime holds for each.
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn class_methods(cls: ClassPtr) -> Vec<(Sel, Option<String>)> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut count: c_uint = 0;
    // SAFETY: `cls` is live. `class_copyMethodList` writes the count and returns a malloc'd array
    // (freed below), or null with a zero count.
    let list = unsafe { class_copyMethodList(cls, &raw mut count) };
    if list.is_null() {
        return Vec::new();
    }
    let mut rows = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: `i < count` live `Method` handles at `list`; a `Method` is owned by its class.
        let method = unsafe { *list.add(i) };
        // SAFETY: `method` is a live handle from the array above.
        let sel = unsafe { method_getName(method) };
        // SAFETY: as above. Null for a method registered without types (checked).
        let types = unsafe { method_getTypeEncoding(method) };
        let encoding = if types.is_null() {
            None
        } else {
            // SAFETY: a non-null, NUL-terminated string the runtime owns for the life of the class.
            Some(
                unsafe { CStr::from_ptr(types) }
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        rows.push((sel, encoding));
    }
    // SAFETY: the array from `class_copyMethodList` is the caller's to `free`; the `Method` handles stay owned by the runtime.
    unsafe { free(list.cast()) };
    rows
}

/// The protocols a class object claims — its own `class_addProtocol` list, not its superclass's.
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn class_protocols(cls: ClassPtr) -> Vec<String> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut count: c_uint = 0;
    // SAFETY: `cls` is live; `class_copyProtocolList` writes the count and returns a malloc'd array this function frees, or null with a zero count.
    let list = unsafe { class_copyProtocolList(cls, &raw mut count) };
    if list.is_null() {
        return Vec::new();
    }
    let mut names = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: `i < count`, and protocol objects are immortal.
        let proto = unsafe { *list.add(i) };
        // SAFETY: `proto` is a live protocol object; `protocol_getName` returns a NUL-terminated string the runtime owns forever.
        let name = unsafe { CStr::from_ptr(protocol_getName(proto)) };
        names.push(name.to_string_lossy().into_owned());
    }
    // SAFETY: the array is the caller's to `free`; the protocol objects it held are immortal and are not touched.
    unsafe { free(list.cast()) };
    names
}

/// An owned (+1) Objective-C object.
#[derive(Debug)]
pub struct Obj(Id);

impl Obj {
    /// Adopt a +1 reference (an `alloc`/`new*`/`copy*` return).
    /// # Safety
    /// `id` must be a +1 reference the caller is handing over, or null.
    #[must_use]
    pub const unsafe fn from_owned(id: Id) -> Option<Self> {
        if id.is_null() { None } else { Some(Self(id)) }
    }

    /// Retain a borrowed reference (an autoreleased or +0 return) to +1.
    /// # Safety
    /// `id` must be a live object pointer, or null.
    #[must_use]
    pub unsafe fn retain(id: Id) -> Option<Self> {
        if id.is_null() {
            return None;
        }
        // SAFETY: caller pins `id` as live; `objc_retain` returns the same pointer at +1, which `Drop` balances exactly once.
        Some(Self(unsafe { objc_retain(id) }))
    }

    #[inline]
    #[must_use]
    pub const fn id(&self) -> Id {
        self.0
    }

    /// A second +1 handle to the same object (`objc_retain`).
    #[must_use]
    pub fn clone_retained(&self) -> Self {
        // SAFETY: `self.0` is non-null and live by construction (this holder owns a +1 reference); `objc_retain` is thread-safe.
        unsafe { Self::retain(self.0).expect("retaining a live non-null object") }
    }

    /// Give up ownership without releasing; the caller inherits the +1.
    #[must_use]
    pub(crate) const fn into_raw(self) -> Id {
        let id = self.0;
        std::mem::forget(self);
        id
    }

    /// Hand the +1 to the innermost pool and return the now-borrowed pointer.
    #[must_use]
    pub fn autorelease(self) -> Id {
        // SAFETY: `into_raw` hands over the one +1 without releasing and `objc_autorelease` takes it; the result is borrowed until the pool pops.
        unsafe { autorelease(self.into_raw()) }
    }
}

impl Drop for Obj {
    /// Releasing may run `dealloc`, which can autorelease into the current
    /// frame; holding an [`AutoreleasePool`] across the scope is the caller's
    /// obligation, not a hidden push/pop per release.
    fn drop(&mut self) {
        // SAFETY: `self.0` is non-null by construction and holds exactly one +1 reference, released once here.
        unsafe { objc_release(self.0) }
    }
}

/// Hand a +1 reference to the innermost autorelease pool on this thread.
/// # Safety
/// `obj` must be a +1 reference the caller is handing over, or null.
#[inline]
#[must_use]
pub unsafe fn autorelease(obj: Id) -> Id {
    // SAFETY: `obj` is +1 or null (returned unchanged); the runtime takes the reference and returns the same pointer at +0.
    unsafe { objc_autorelease(obj) }
}

/// An `@autoreleasepool` scope.
pub struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    /// Push a pool and hand back its token.
    /// # Safety
    /// The value must be dropped before any pool pushed after it on this thread, and on the thread that pushed it.
    #[must_use]
    pub unsafe fn new() -> Self {
        // SAFETY: push/pop are the runtime entry points, balanced by `Drop`; LIFO order is the caller's obligation, stated above.
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the matching push's token. The safe constructor
        // `autoreleasepool` makes this the innermost pop; the `unsafe` `new`
        // puts that obligation on its caller.
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// Run `f` inside an `@autoreleasepool`.
#[inline]
pub fn autoreleasepool<R>(f: impl FnOnce(&AutoreleasePool) -> R) -> R {
    // SAFETY: the token is a local of this frame, dropped below or during an unwind out of `f`; anything pushed after it was pushed inside `f` and has already returned or unwound.
    let pool = unsafe { AutoreleasePool::new() };
    let out = f(&pool);
    drop(pool);
    out
}

/// Build a +1 `NSString` holding exactly `s`.
#[must_use]
pub fn ns_string(s: &str) -> Option<Obj> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let cls = class(c"NSString");
    // SAFETY: `alloc` returns +1 uninitialised; `initWithCharacters:length:`
    // (`@32@0:8r^S16Q24`) consumes it and returns +1 or nil (`from_owned` maps
    // nil to `None`). It copies `units.len()` code units during the call only.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = crate::checked_msg();
        let raw = alloc(cls, crate::sel!(alloc));
        if raw.is_null() {
            return None;
        }
        let init: unsafe extern "C-unwind" fn(Id, Sel, *const u16, usize) -> Id =
            crate::checked_msg();
        let obj = init(
            raw,
            crate::sel!(initWithCharacters:length:),
            units.as_ptr(),
            units.len(),
        );
        Obj::from_owned(obj)
    }
}

/// Copy an `NSString`'s contents into an owned Rust `String`.
/// # Safety
/// `s` must be a live `NSString` (or nil, which yields an empty string).
#[must_use]
pub unsafe fn ns_string_to_rust(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    autoreleasepool(|_| {
        // SAFETY: `s` is a live NSString. `-length` is `Q16@0:8`, the count of
        // UTF-16 code units. `-getCharacters:range:` is `v40@0:8^S16{_NSRange=QQ}24`
        // and writes exactly `range.length` units into the buffer; the range is
        // the whole string, so it is in bounds, and the buffer holds that many.
        // Its length is set only after the call has written them.
        unsafe {
            let length: unsafe extern "C-unwind" fn(Id, Sel) -> usize = crate::checked_msg();
            let n = length(s, crate::sel!(length));
            if n == 0 {
                return String::new();
            }
            let mut units: Vec<u16> = Vec::with_capacity(n);
            let get: unsafe extern "C-unwind" fn(Id, Sel, *mut u16, crate::NSRange) =
                crate::checked_msg();
            get(
                s,
                crate::sel!(getCharacters:range:),
                units.as_mut_ptr(),
                crate::NSRange {
                    location: 0,
                    length: n,
                },
            );
            units.set_len(n);
            String::from_utf16_lossy(&units)
        }
    })
}

/// Read an `NSError`'s `localizedDescription` into an owned Rust `String`.
/// # Safety
/// `err` must be a live `NSError`, or null (which reports `"(nil NSError)"`).
#[must_use]
pub unsafe fn ns_error_string(err: Id) -> String {
    if err.is_null() {
        return "(nil NSError)".to_owned();
    }
    autoreleasepool(|_| {
        // SAFETY: `err` is a live `NSError`. `localizedDescription` is
        // autoreleased and copied before the pool pops.
        unsafe {
            let desc: unsafe extern "C-unwind" fn(Id, Sel) -> Id = crate::checked_msg();
            let d = desc(err, sel(c"localizedDescription"));
            if d.is_null() {
                return "(no localizedDescription)".to_owned();
            }
            ns_string_to_rust(d)
        }
    })
}
