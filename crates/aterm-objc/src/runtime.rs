// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The raw runtime surface: symbols, typed sends, ownership, pools.
//!
//! Conventions follow `aterm-gpu/src/metal/ffi.rs`, except that the
//! autorelease pool is a scope ([`AutoreleasePool`]), [`msg`] picks
//! `objc_msgSend` or `objc_msgSend_stret` from the prototype's return type
//! ([`returns_indirectly`]), and class-pair creation, super sends, protocols
//! and [`autorelease`] are bound here too.

use std::ffi::{CStr, c_char, c_uint, c_void};

/// An Objective-C object pointer. Null is `nil`, a valid receiver, so this
/// wraps a raw pointer rather than a `NonNull`.
///
/// A nil send returns zero for every register-class value (integers, pointers,
/// floats, register-sized structs), but on x86_64 libobjc leaves an indirect
/// struct return (`objc_msgSend_stret`: `CGRect`, anything over 16 bytes or
/// with a misaligned field) uninitialised. The `send_rect*` helpers nil-check
/// for that; a hand-cast `msg()` returning a struct must do the same.
///
/// A newtype so it can carry its own [`crate::Encode`] (`"@"`), leaving
/// `*mut c_void` to mean `"^v"`. KVO's `context:` is `void *`, which clang
/// encodes as `^v`:
///
/// ```text
/// observeValueForKeyPath:ofObject:change:context:   v48@0:8@16@24@32^v40
/// ```
///
/// Registering it as `"@"` would invite `NSInvocation`, forwarding and
/// accessibility to retain it (`tests/adversary_w2c.rs`). `#[repr(transparent)]`,
/// so it is FFI-safe and exactly the word the runtime passes.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(*mut c_void);

impl Id {
    /// The Objective-C `nil`.
    pub const NIL: Self = Self(std::ptr::null_mut());

    /// Wrap a raw object pointer. Safe: sending to an `id`, not holding one, is
    /// what is dangerous, and every send is `unsafe`.
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

    /// The address without exposing provenance, for identity comparisons and
    /// `-hash` (the instance pointer on `NSObject`).
    #[inline]
    #[must_use]
    pub fn addr(self) -> usize {
        self.0.addr()
    }

    /// The same address as a `*mut T`, for the zero-sized markers
    /// [`crate::declare_class!`] mints.
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
///
/// A newtype so it can carry its own [`crate::Encode`]: `SEL` is `":"` while
/// `const void *` is `"^v"`. `#[repr(transparent)]`, so it is FFI-safe and
/// exactly the word the runtime passes.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sel(*const c_void);

impl Sel {
    /// The nil selector. Legal as an argument (`-[NSMenuItem
    /// initWithTitle:action:keyEquivalent:]` for a label row,
    /// `-[NSControl setAction:]` to unwire a control); never a valid selector
    /// to send, since `objc_msgSend` with a nil selector crashes. The only
    /// selector not interned by `sel_registerName`.
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
    /// Lets an enumerated selector (from [`class_methods`]) be printed or
    /// matched. `"(null)"` for [`Sel::NULL`].
    #[must_use]
    pub fn name(self) -> &'static CStr {
        if self.0.is_null() {
            return c"(null)";
        }
        // SAFETY: non-null selector from the runtime; `sel_getName` returns a
        // NUL-terminated string in the immortal selector table, so `'static`.
        unsafe { CStr::from_ptr(sel_getName(self)) }
    }

    /// Wrap a pointer the runtime returned. Crate-private: every selector handed
    /// out comes from `sel_registerName` (or is [`Sel::NULL`]).
    #[inline]
    pub(crate) const fn from_ptr(ptr: *const c_void) -> Self {
        Self(ptr)
    }
}

// SAFETY: a selector points into libobjc's immortal selector table: never
// freed or written, interning is idempotent and synchronised, so the pointer
// is valid on every thread for the life of the process.
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
        // SAFETY: non-null selector from `sel_registerName`; `sel_getName`
        // returns a NUL-terminated string the runtime never frees.
        let name = unsafe { CStr::from_ptr(sel_getName(*self)) };
        write!(f, "Sel({})", name.to_string_lossy())
    }
}

/// An Objective-C class object.
///
/// Same machine shape as [`Id`], but `@encode(Class)` is `"#"`:
///
/// ```text
/// setDelegateClass:                              v24@0:8#16
/// ```
///
/// A newtype so its [`crate::Encode`] can say so; [`ProtocolPtr`] encodes `"@"`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClassPtr(*mut c_void);

impl ClassPtr {
    /// The null class — what a failed [`class`] lookup returns.
    pub const NULL: Self = Self(std::ptr::null_mut());

    /// Wrap a raw class pointer.
    #[inline]
    #[must_use]
    pub const fn from_ptr(ptr: *mut c_void) -> Self {
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

    /// The address with provenance exposed — what [`crate::ClassMeta`] stores
    /// so it can live in a `OnceLock`.
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

    /// This class as an object, for class messages (`+alloc`,
    /// `+sharedWorkspace`). Explicit because the two types differ in signatures.
    #[inline]
    #[must_use]
    pub const fn as_id(self) -> Id {
        Id::from_ptr(self.0)
    }
}

impl std::fmt::Debug for ClassPtr {
    /// Prints the class's name via `class_getName`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: `self` is null (reported as `"(nil)"` without a dereference)
        // or a class object, which is immortal.
        let name = unsafe { class_name(*self) };
        write!(f, "Class({})", name.to_string_lossy())
    }
}

/// An Objective-C protocol object, as returned by `objc_getProtocol`.
/// `@encode(Protocol *)` is `"@"` (measured, not `"#"`).
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

/// An `Ivar` handle, as returned by `class_getInstanceVariable`. A plain alias:
/// it never appears in a method signature, so it needs no [`crate::Encode`].
pub type IvarPtr = *const c_void;

/// The `objc_super` struct `objc_msgSendSuper` takes in place of a receiver.
/// For `[super foo]` in a method of class `C`, `super_class` is `C`'s
/// superclass, not `C`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ObjcSuper {
    /// The object the message is actually delivered to.
    pub receiver: Id,
    /// Where method lookup starts (the superclass of the defining class).
    pub super_class: ClassPtr,
}

// The send entry points are `"C-unwind"`. An `NSException` raised inside a
// send unwinds through the calling Rust frame as a foreign exception; under
// `extern "C"` (`nounwind`) that frame has no landing pad, so its `Drop`s are
// skipped, the process aborts with `failed to initiate panic`, or LLVM deletes
// a `catch_unwind` around nounwind calls. `"C-unwind"` runs every destructor
// on the way to [`crate::exception`]'s containment at no hot-path cost.
// [`MsgFn`] is implemented for `unsafe extern "C-unwind" fn(..)` only, since
// one `"C"` frame on the raise path loses the frames above it.
#[link(name = "objc")]
unsafe extern "C-unwind" {
    /// Declared with no parameter list on purpose. Every call goes through
    /// [`msg`], which casts it to the exact prototype of the selector.
    fn objc_msgSend();
    /// Same discipline as [`objc_msgSend`], but the first argument is a
    /// `*const ObjcSuper` rather than the receiver. Cast through [`msg_super`].
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

    // --- class-pair creation ---
    pub(crate) fn objc_allocateClassPair(
        superclass: ClassPtr,
        name: *const c_char,
        extra_bytes: usize,
    ) -> ClassPtr;
    pub(crate) fn objc_registerClassPair(cls: ClassPtr);
    // These return Objective-C `BOOL`, not `bool`: on `x86_64-apple-darwin`
    // `BOOL` is `signed char`, not every value of which is a valid `bool`. See
    // [`crate::Bool`].
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
    // --- protocol registration. A protocol exists in a process only if a
    // loaded image lists it in `__objc_protolist`; a header declaration alone
    // registers nothing, and AppKit does not carry `NSApplicationDelegate` on
    // every macOS. A Rust binary has no `__objc_protolist`, so aterm registers
    // missing protocols itself. See [`protocol_or_register`]. ---
    fn objc_allocateProtocol(name: *const c_char) -> ProtocolPtr;
    fn objc_registerProtocol(proto: ProtocolPtr);
    pub(crate) fn class_getInstanceVariable(cls: ClassPtr, name: *const c_char) -> IvarPtr;
    pub(crate) fn ivar_getOffset(ivar: IvarPtr) -> isize;

    // --- reading a registered method back: the read half of the encoding
    // contract, so tests can ask the runtime what was registered. ---
    pub(crate) fn class_getInstanceMethod(cls: ClassPtr, name: Sel) -> *const c_void;
    pub(crate) fn method_getTypeEncoding(method: *const c_void) -> *const c_char;

    // --- the IMP behind a method: the only evidence of whose code will run.
    // A swizzle leaves the encoding untouched, so encoding checks cannot see
    // one. ---
    pub(crate) fn method_getImplementation(method: *const c_void) -> *const c_void;

    // --- replacing an existing implementation, for [`crate::swizzle`].
    // `class_addMethod` returns `NO` for an existing row, so it cannot
    // substitute. `method_setImplementation` takes no `types` argument and
    // leaves the encoding unchanged; `class_replaceMethod` ignores a differing
    // `types` for a row the class already has; `class_addMethod` changes
    // nothing (`tests/swizzle.rs`). So no swizzle can corrupt an encoding, and
    // no encoding check can see one. ---
    pub(crate) fn method_setImplementation(
        method: *const c_void,
        imp: *const c_void,
    ) -> *const c_void;

    // --- the whole method/protocol table, so a caller can enumerate what the
    // runtime actually registered instead of checking its own copy of a list
    // (`crates/aterm-gui/examples/objc_live_class_audit.rs`). ---
    fn class_copyMethodList(cls: ClassPtr, out_count: *mut c_uint) -> *mut *const c_void;
    fn method_getName(method: *const c_void) -> Sel;
    fn class_copyProtocolList(cls: ClassPtr, out_count: *mut c_uint) -> *mut ProtocolPtr;
    fn protocol_getName(proto: ProtocolPtr) -> *const c_char;

    // --- a protocol's own method description: what the framework will call,
    // as opposed to what a class registered. ---
    fn protocol_getMethodDescription(
        proto: ProtocolPtr,
        sel: Sel,
        is_required_method: crate::encode::Bool,
        is_instance_method: crate::encode::Bool,
    ) -> ObjcMethodDescription;
}

// `class_copyMethodList` and `class_copyProtocolList` return a malloc'd array
// the caller owns and must `free`.
unsafe extern "C" {
    fn free(ptr: *mut c_void);
}

/// `struct objc_method_description` — a selector and its type string.
///
/// Two pointers, so it comes back in registers on both Apple ABIs (`x0`/`x1`,
/// `RAX`/`RDX`) and needs no `_stret` consideration.
#[repr(C)]
#[derive(Clone, Copy)]
struct ObjcMethodDescription {
    name: Sel,
    types: *const c_char,
}

// The indirect-return entry points exist only on x86_64; arm64 libobjc exports
// no `_objc_msgSend_stret`, so a reference reaching an arm64 link fails.
// System V x86-64 returns a MEMORY-class value through a hidden pointer in
// `RDI`, where plain `objc_msgSend` expects `self`, so every argument would
// shift by one register. AAPCS64 uses `x8` for that pointer, so arm64 needs
// one entry point.
//
// `objc_msgSend_fpret` / `_fp2ret` (x87 `long double`) are not bound: no Rust
// type has that ABI. `tests/abi.rs` records the per-return-type codegen.
#[cfg(target_arch = "x86_64")]
#[link(name = "objc")]
unsafe extern "C-unwind" {
    /// Same discipline and `"C-unwind"` ABI as [`objc_msgSend`].
    fn objc_msgSend_stret();
    /// Same, against the super entry point.
    fn objc_msgSendSuper_stret();
}

// Declares no symbol: it only links Foundation, where `NSString` and the other
// classes this crate looks up live. Without it `class(c"NSString")` is nil in
// a binary that links nothing else from Foundation, such as this crate's tests.
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

/// A message-send prototype, decomposed far enough that [`msg`] can read its
/// return type and pick the right entry point.
///
/// Implemented for `unsafe extern "C-unwind" fn(..) -> R` with up to sixteen
/// parameters including the implicit `self` and `_cmd`, i.e. up to fourteen
/// arguments. Sixteen is headroom, not an ABI boundary (register passing ends
/// at eight on AAPCS64 and six on System V; the rest spill to the stack).
/// AppKit's widest initializer takes ten arguments.
/// [`crate::declare_class!`] asserts the same ceiling, so a method Rust cannot
/// send fails to compile rather than registering.
///
/// The bound is what makes the `_stret` choice automatic — see
/// [`returns_indirectly`].
///
/// Fourteen arguments are sendable (`tests/adversary_w2c.rs`); fifteen do not
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
/// An implementor must be a bare `unsafe extern "C-unwind"` function pointer
/// whose return type is `Ret`: [`msg`] transmutes a symbol address into `Self`
/// and dispatches on `size_of::<Self::Ret>()`, so a lying `Ret` picks the wrong
/// entry point.
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
        // SAFETY: the implementing type is an `unsafe extern "C-unwind"`
        // function pointer and `Ret` is its own return type.
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

/// Whether `R` comes back through a hidden pointer on this target's C ABI,
/// which decides whether a send must go through `objc_msgSend_stret`.
///
/// * `aarch64-apple-darwin`: always `false`. The indirect-result pointer goes
///   in `x8`, which no argument occupies, and libobjc has no `_stret` variant.
/// * `x86_64-apple-darwin`: `size_of::<R>() > 16` (a 16-byte struct of two
///   `double`s returns in `xmm0`/`xmm1`, 17 bytes goes to memory; see
///   `tests/abi.rs`), or `R` has unaligned fields.
///
/// System V classifies a struct MEMORY if it exceeds eight eightbytes or
/// contains unaligned fields, and the second clause has no size threshold.
/// clang `-arch x86_64 -O1 -S` on real ObjC sends:
///
/// ```text
/// size align  type                                             x86_64 entry
///    3     1  struct __attribute__((packed)) { char; short; }   _objc_msgSend_stret
///    5     1  struct __attribute__((packed)) { char; int; }     _objc_msgSend_stret
///    6     1  struct __attribute__((packed)) { short; int; }    _objc_msgSend_stret
///    9     1  struct __attribute__((packed)) { char; long; }    _objc_msgSend_stret
///   16     1  struct __attribute__((packed)) { int a,b,c,d; }   _objc_msgSend
///    2     1  struct __attribute__((packed)) { char; char; }    _objc_msgSend
///    9     1  struct { char c[9]; }                             _objc_msgSend
///   16     8  struct { double a, b; }                           _objc_msgSend
///   24     8  struct { double a, b, c; }                        _objc_msgSend_stret
/// ```
///
/// Types with identical `size_of` and `align_of` land on different entry
/// points, so the answer comes from the type's author as
/// [`crate::Encode::HAS_UNALIGNED_FIELDS`] (default `false`; a packed type
/// sets it). [`msg`] requires an `Encode` return type, so the question is
/// always answered.
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
/// the call folds away. A diverging function rather than a `cfg` in [`msg`]
/// keeps one send logic for both architectures.
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

/// Cast the untyped `objc_msgSend` to a concrete prototype, choosing the
/// `_stret` entry point when `F`'s return type needs it.
///
/// [`returns_indirectly`] decides in a `const` block, so the choice is free at
/// run time and no call site can get it wrong. The `Encode` bound on the return
/// type answers the packed-struct question `size_of` cannot, and refuses
/// `-> bool` (an ObjC `BOOL` return is spelled [`crate::Bool`]).
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
/// `F` must be the exact C prototype of the selector about to be sent,
/// including the implicit `(self, _cmd)` pair. Anything else is undefined
/// behaviour on every Apple ABI.
#[inline]
#[must_use]
pub unsafe fn msg<F: MsgFn>() -> F
where
    <F as MsgFn>::Ret: crate::encode::Encode,
{
    // Rejects an `F` that is not pointer-wide. It does not prove `F` is a
    // function type; `MsgFn` (unsafe to implement) is the real guard.
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    let entry = if const { returns_indirectly::<<F as MsgFn>::Ret>() } {
        stret_entry()
    } else {
        objc_msgSend as *const c_void
    };
    // SAFETY: `entry` is a function symbol's address and `F` is pointer-wide.
    // `F` supplies the prototype; the entry point matches its return class.
    unsafe { std::mem::transmute_copy(&entry) }
}

/// Cast the untyped `objc_msgSendSuper` to a concrete prototype, with the same
/// automatic `_stret` selection as [`msg`].
///
/// # Safety
/// `F` must be the exact C prototype of the selector, with `*const ObjcSuper`
/// in the receiver position and `Sel` second; otherwise as [`msg`].
#[inline]
#[must_use]
pub unsafe fn msg_super<F: MsgFn>() -> F
where
    <F as MsgFn>::Ret: crate::encode::Encode,
{
    // See `msg`.
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    let entry = if const { returns_indirectly::<<F as MsgFn>::Ret>() } {
        super_stret_entry()
    } else {
        objc_msgSendSuper as *const c_void
    };
    // SAFETY: identical to `msg`, against the super entry points.
    unsafe { std::mem::transmute_copy(&entry) }
}

/// The type encoding the runtime holds for `cls`'s instance method `name`, or
/// `None` if neither it nor a superclass implements it.
///
/// Reads back what [`crate::declare_class!`] registered, so a port can be
/// checked against the runtime: `method_types(cls, sel!(validateMenuItem:))`
/// is `"B@:@"` on arm64 and `"c@:@"` on x86_64.
///
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn method_types(cls: ClassPtr, name: Sel) -> Option<String> {
    // SAFETY: `cls` is live; `name` is interned; an unimplemented selector
    // yields NULL.
    let method = unsafe { class_getInstanceMethod(cls, name) };
    if method.is_null() {
        return None;
    }
    // SAFETY: `method` is a live `Method`; the encoding string is owned by the
    // runtime for the class's life, and may be null (checked).
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

/// The IMP the runtime will call for `name` on `cls`, or `None` if the class
/// does not respond to it.
///
/// Where [`method_types`] gives the call's shape, this says whose code runs;
/// the two differ after a swizzle, which replaces the IMP and keeps the types.
/// The pointer is never called or dereferenced here: it is for identity
/// comparison and for asking the dynamic loader which image it came from.
///
/// # Safety
/// `cls` must be a live class object or null.
#[must_use]
pub unsafe fn method_imp(cls: ClassPtr, name: Sel) -> Option<*const c_void> {
    // SAFETY: `cls` is live or null; `class_getInstanceMethod` answers NULL for
    // null or an unimplemented selector and walks the superclass chain.
    let method = unsafe { class_getInstanceMethod(cls, name) };
    if method.is_null() {
        return None;
    }
    // SAFETY: `method` is a live `Method` handle from the call above.
    let imp = unsafe { method_getImplementation(method) };
    (!imp.is_null()).then_some(imp)
}

/// The type encoding a protocol declares for `name`, or `None` if it does not.
///
/// The authority side of the encoding contract: [`method_types`] says what was
/// registered, this says what the framework will call. For example
/// `NSDraggingDestination` declares `-draggingEntered:` as `Q24@0:8@16`, and a
/// port that returns `bool` registers a one-byte return where AppKit reads
/// eight.
///
/// Searches the required table, then the optional one. The string is the
/// compiler-emitted form with byte offsets (`"v24@0:8@16"`); compare through
/// [`crate::strip_method_offsets`], since this crate registers offset-free
/// strings (see [`crate::encode`]).
///
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
    // SAFETY: `proto` is live; `name` is interned. An undeclared selector
    // yields a zeroed description, so `types` is null-checked.
    let mut desc = unsafe {
        protocol_getMethodDescription(proto, name, crate::encode::Bool::YES, is_instance)
    };
    if desc.types.is_null() {
        // SAFETY: as above, against the optional table, where delegate rows
        // usually live.
        desc = unsafe {
            protocol_getMethodDescription(proto, name, crate::encode::Bool::NO, is_instance)
        };
    }
    if desc.types.is_null() {
        return None;
    }
    // SAFETY: a non-null, runtime-owned, NUL-terminated string, immortal for
    // the life of the protocol object.
    Some(
        unsafe { CStr::from_ptr(desc.types) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// Resolve a selector from a checked C-string literal, e.g. `sel(c"length")`.
///
/// Uncached: one `sel_registerName` per call. Per-frame call sites use the
/// cached [`crate::sel!`].
#[inline]
#[must_use]
pub fn sel(name: &'static CStr) -> Sel {
    // SAFETY: `CStr` guarantees one trailing NUL and no interior NUL;
    // `sel_registerName` copies it into the runtime's immortal selector table.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// Look up a class from a checked C-string literal, e.g. `class(c"NSString")`.
///
/// Not cached: every call pays `objc_getClass`'s string hash, roughly ten times
/// the cost of a send and most of what [`crate::MainThread::new`] (and so
/// [`crate::run_on_main`]) costs. Classes are immortal, so a cache mirroring
/// [`crate::SelCache`] would be sound.
#[inline]
#[must_use]
pub fn class(name: &'static CStr) -> ClassPtr {
    // SAFETY: `CStr` gives a valid C string. Classes are immortal; the pointer
    // is never released.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// Look up a protocol by name, e.g. `protocol(c"NSMenuDelegate")`. Null when
/// no loaded image registers it. A pure lookup that never registers anything.
///
/// Presence depends on the loaded images: the runtime registers a protocol only
/// from an image whose `__objc_protolist` carries it, which a framework that
/// merely declares it in a header may not, and this varies between macOS
/// releases (some ship AppKit without `NSApplicationDelegate`). So null does
/// not mean the framework is unlinked (`class(c"NSApplication")` answers that).
/// A class that must claim a protocol uses [`protocol_or_register`].
#[inline]
#[must_use]
pub fn protocol(name: &'static CStr) -> ProtocolPtr {
    // SAFETY: `CStr` gives a valid C string. Protocol objects are immortal and
    // never released.
    unsafe { objc_getProtocol(name.as_ptr()) }
}

/// The names of every protocol this crate registered because no loaded image
/// had, in registration order, each once.
///
/// Tells an auditor "the host framework does not register this protocol" from
/// "it does". A protocol registered here is name-only, so it is never an
/// encoding authority: [`protocol_method_types`] answers `None` for it.
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

/// The protocol object for `name`, registered by this crate if no loaded image
/// registers it.
///
/// `class_addProtocol` needs a protocol object, and whether the host framework
/// registers one varies by macOS release (see [`protocol`]). Skipping the claim
/// leaves `-conformsToProtocol:` answering NO; panicking kills the app on hosts
/// that lack the protocol.
///
/// When the lookup is null this allocates and registers a name-only protocol
/// (no method descriptions, no parents) and records it for
/// [`protocols_registered_by_aterm`]. A claim needs only identity:
/// `conformsToProtocol:` compares names, and AppKit reaches a claimant's rows
/// by selector (`-respondsToSelector:` for the application delegate, direct
/// sends of required rows for a text input client; see [`class_protocols`]).
/// Copying method descriptions from a Rust table would make this crate the
/// authority the seam census checks the port against.
///
/// Registration is serialized under one lock and idempotent; the returned
/// object is always `objc_getProtocol`'s answer, since two unserialized
/// allocations of one name both succeed and the loser is an orphan. An image
/// loaded later that carries the real protocol has its references remapped to
/// the object already registered, so every `@protocol(…)` names one object.
///
/// # Panics
/// If the runtime refuses to register (allocation nil although the lookup
/// found nothing, or the lookup still nil after registration).
#[must_use]
pub fn protocol_or_register(name: &'static CStr) -> ProtocolPtr {
    let found = protocol(name);
    if !found.is_null() {
        return found;
    }
    let mut registered = registered()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Re-asked under the lock: another thread may have registered it between
    // the lookup above and here.
    let found = protocol(name);
    if !found.is_null() {
        return found;
    }
    // SAFETY: `name` is a valid C string; `objc_allocateProtocol` returns a
    // fresh, unregistered protocol object, or nil if the name is taken.
    let proto = unsafe { objc_allocateProtocol(name.as_ptr()) };
    assert!(
        !proto.is_null(),
        "aterm-objc: objc_allocateProtocol({name:?}) answered nil although \
         objc_getProtocol found nothing under that name"
    );
    // SAFETY: `proto` is a live, allocated, unregistered protocol object, and
    // registering it is the documented terminal operation on one.
    unsafe { objc_registerProtocol(proto) };
    let now = protocol(name);
    assert!(
        !now.is_null(),
        "aterm-objc: protocol {name:?} is still absent after objc_registerProtocol"
    );
    registered.push(name);
    now
}

/// The class of a live object (`object_getClass`). Null for `nil`.
///
/// # Safety
/// `obj` must be a live object pointer, or null.
#[inline]
#[must_use]
pub unsafe fn class_of(obj: Id) -> ClassPtr {
    // SAFETY: the caller pins `obj` as live-or-null; `object_getClass` reads
    // only the isa field and returns an immortal class pointer.
    unsafe { object_getClass(obj) }
}

/// A class's superclass. Null for a root class (or for `nil`).
///
/// # Safety
/// `cls` must be a live class object, or null.
#[must_use]
pub unsafe fn superclass_of(cls: ClassPtr) -> ClassPtr {
    // SAFETY: class pointers are immortal once registered; the accessor has no
    // side effects and tolerates nil.
    unsafe { class_getSuperclass(cls) }
}

/// A class's name, borrowed from the runtime's immortal storage.
///
/// # Safety
/// `cls` must be a live class object, or null (which reports `"(nil)"`).
#[must_use]
pub unsafe fn class_name(cls: ClassPtr) -> &'static CStr {
    if cls.is_null() {
        return c"(nil)";
    }
    // SAFETY: `class_getName` returns a NUL-terminated string owned by the
    // runtime for a registered class; it outlives the process.
    unsafe { CStr::from_ptr(class_getName(cls)) }
}

/// Every instance method a class registered itself, with the type encoding the
/// runtime holds for each.
///
/// Enumerates rather than looks up, so a check starts from what the runtime has
/// and finds each row's authority ([`protocol_method_types`] /
/// [`method_types`]) instead of re-checking its own list of selectors.
///
/// Inherited methods are not included (`class_copyMethodList` reports only
/// this class's own rows, which is what a [`crate::declare_class!`] site is
/// responsible for). The encoding is `None` for a method registered without a
/// type string (possible through raw `class_addMethod`). Order is the
/// runtime's, not declaration order.
///
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn class_methods(cls: ClassPtr) -> Vec<(Sel, Option<String>)> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut count: c_uint = 0;
    // SAFETY: `cls` is live. `class_copyMethodList` writes the count and
    // returns a malloc'd array (freed below), or null with a zero count.
    let list = unsafe { class_copyMethodList(cls, &raw mut count) };
    if list.is_null() {
        return Vec::new();
    }
    let mut rows = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: `i < count` live `Method` handles at `list`; a `Method` is
        // owned by its class.
        let method = unsafe { *list.add(i) };
        // SAFETY: `method` is a live handle from the array above.
        let sel = unsafe { method_getName(method) };
        // SAFETY: as above. Null for a method registered without types
        // (checked).
        let types = unsafe { method_getTypeEncoding(method) };
        let encoding = if types.is_null() {
            None
        } else {
            // SAFETY: a non-null, NUL-terminated string the runtime owns for
            // the life of the class.
            Some(
                unsafe { CStr::from_ptr(types) }
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        rows.push((sel, encoding));
    }
    // SAFETY: the array from `class_copyMethodList` is the caller's to `free`;
    // the `Method` handles stay owned by the runtime.
    unsafe { free(list.cast()) };
    rows
}

/// The protocols a class object claims — its own `class_addProtocol` list, not
/// its superclass's.
///
/// A claim is what makes `conformsToProtocol:` answer YES, and no encoding
/// check can see it. Whether AppKit asks depends on the protocol: application
/// delegate rows are dispatched by `-respondsToSelector:` alone, while
/// `-[NSView _inputContext]` creates an `NSTextInputContext` only for a view
/// whose class conforms to `NSTextInputClient`, then sends its required rows
/// (a conforming view that lacks them aborts with `unrecognized selector`).
///
/// A name here may be one this crate registered itself (see
/// [`protocol_or_register`], [`protocols_registered_by_aterm`]). Names, not
/// [`ProtocolPtr`]s, since callers compare them with the claimed list.
///
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn class_protocols(cls: ClassPtr) -> Vec<String> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut count: c_uint = 0;
    // SAFETY: `cls` is live; `class_copyProtocolList` writes the count and
    // returns a malloc'd array this function frees, or null with a zero count.
    let list = unsafe { class_copyProtocolList(cls, &raw mut count) };
    if list.is_null() {
        return Vec::new();
    }
    let mut names = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: `i < count`, and protocol objects are immortal.
        let proto = unsafe { *list.add(i) };
        // SAFETY: `proto` is a live protocol object; `protocol_getName` returns
        // a NUL-terminated string the runtime owns forever.
        let name = unsafe { CStr::from_ptr(protocol_getName(proto)) };
        names.push(name.to_string_lossy().into_owned());
    }
    // SAFETY: the array is the caller's to `free`; the protocol objects it held
    // are immortal and are not touched.
    unsafe { free(list.cast()) };
    names
}

/// An owned (+1) Objective-C object. The only place `objc_release` is called
/// for untyped objects ([`crate::Retained`] is the typed twin).
#[derive(Debug)]
pub struct Obj(Id);

impl Obj {
    /// Adopt a +1 reference (an `alloc`/`new*`/`copy*` return).
    ///
    /// # Safety
    /// `id` must be a +1 reference the caller is handing over, or null.
    #[must_use]
    pub const unsafe fn from_owned(id: Id) -> Option<Self> {
        if id.is_null() { None } else { Some(Self(id)) }
    }

    /// Retain a borrowed reference (an autoreleased or +0 return) to +1.
    ///
    /// # Safety
    /// `id` must be a live object pointer, or null.
    #[must_use]
    pub unsafe fn retain(id: Id) -> Option<Self> {
        if id.is_null() {
            return None;
        }
        // SAFETY: caller pins `id` as live; `objc_retain` returns the same
        // pointer at +1, which `Drop` balances exactly once.
        Some(Self(unsafe { objc_retain(id) }))
    }

    #[inline]
    #[must_use]
    pub const fn id(&self) -> Id {
        self.0
    }

    /// A second +1 handle to the same object (`objc_retain`). Not `Clone`, so
    /// every retain is visible at the call site.
    #[must_use]
    pub fn clone_retained(&self) -> Self {
        // SAFETY: `self.0` is non-null and live by construction (this holder
        // owns a +1 reference); `objc_retain` is thread-safe.
        unsafe { Self::retain(self.0).expect("retaining a live non-null object") }
    }

    /// Give up ownership without releasing; the caller inherits the +1.
    #[must_use]
    pub(crate) fn into_raw(self) -> Id {
        let id = self.0;
        std::mem::forget(self);
        id
    }

    /// Hand the +1 to the innermost pool and return the now-borrowed pointer.
    ///
    /// What a declared method returning an object owes its caller when its name
    /// does not begin `new`/`alloc`/`copy`/`mutableCopy`: +0. See
    /// [`autorelease`].
    #[must_use]
    pub fn autorelease(self) -> Id {
        // SAFETY: `into_raw` hands over the one +1 without releasing and
        // `objc_autorelease` takes it; the result is borrowed until the pool
        // pops.
        unsafe { autorelease(self.into_raw()) }
    }
}

impl Drop for Obj {
    /// Releasing may run `dealloc`, which can autorelease into the current
    /// frame; holding an [`AutoreleasePool`] across the scope is the caller's
    /// obligation, not a hidden push/pop per release.
    fn drop(&mut self) {
        // SAFETY: `self.0` is non-null by construction and holds exactly one +1
        // reference, released once here.
        unsafe { objc_release(self.0) }
    }
}

/// Hand a +1 reference to the innermost autorelease pool on this thread.
///
/// The returned pointer is the same object, now borrowed: it stays alive until
/// that pool pops.
///
/// With no pool on the thread the object leaks and nothing else goes wrong, so
/// a missing pool is a leak, not undefined behaviour — which is why the safe
/// [`Obj::autorelease`] and [`crate::Retained::autorelease`] may call this.
/// libobjc reports it only under `OBJC_DEBUG_MISSING_POOLS=YES`:
///
/// ```text
/// $ CHILD=1 ./adversary_w2c                              (no output)
/// $ CHILD=1 OBJC_DEBUG_MISSING_POOLS=YES ./adversary_w2c
/// objc[75911]: MISSING POOLS: (0x16fb43000) Object 0x100f33610 of class
/// __NSCFString autoreleased with no pool in place - just leaking - break on
/// objc_autoreleaseNoPool() to debug
/// ```
///
/// (`tests/adversary_w2c.rs` runs both.) A Cocoa main thread always has a pool
/// in the event loop; elsewhere use [`autoreleasepool`].
///
/// # Safety
/// `obj` must be a +1 reference the caller is handing over, or null.
#[inline]
#[must_use]
pub unsafe fn autorelease(obj: Id) -> Id {
    // SAFETY: `obj` is +1 or null (returned unchanged); the runtime takes the
    // reference and returns the same pointer at +0.
    unsafe { objc_autorelease(obj) }
}

/// An `@autoreleasepool` scope.
///
/// Not a bare RAII guard: `!Send` and `!Clone` do not imply LIFO drop order,
/// and popping an outer pool before an inner one frees what the inner pool
/// holds and then aborts in the runtime:
///
/// ```text
/// Invalid or prematurely-freed autorelease pool … Invalid autorelease pools
/// are a fatal error
/// ```
///
/// So the token constructor is `unsafe` and [`autoreleasepool`]'s nested scopes
/// are the safe API; an inner scope cannot outlive the outer one.
///
/// This does not compile:
///
/// ```compile_fail
/// # use aterm_objc::AutoreleasePool;
/// let outer = AutoreleasePool::new();
/// let inner = AutoreleasePool::new();
/// drop(outer);
/// drop(inner);
/// ```
///
/// The same intent, as scopes:
///
/// ```
/// # use aterm_objc::autoreleasepool;
/// let n = autoreleasepool(|_outer| autoreleasepool(|_inner| 7));
/// assert_eq!(n, 7);
/// ```
///
/// `tests/pools.rs` runs the first snippet unsafely in a child and asserts the
/// `SIGABRT`.
pub struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    /// Push a pool and hand back its token.
    ///
    /// # Safety
    /// The value must be dropped before any pool pushed after it on this
    /// thread, and on the thread that pushed it. [`autoreleasepool`] is the
    /// safe form; use this only where a scope cannot express the lifetime.
    #[must_use]
    pub unsafe fn new() -> Self {
        // SAFETY: push/pop are the runtime entry points, balanced by `Drop`;
        // LIFO order is the caller's obligation, stated above.
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
///
/// The pool pops when `f` returns or unwinds. Autoreleased objects reached
/// inside are valid for the body and not after it. Nesting these calls is the
/// only way to open a second pool (see [`AutoreleasePool`]).
///
/// `&AutoreleasePool` proves that a pool is open on this thread, not that it is
/// the one the runtime will use: every +0 return autoreleases into the
/// innermost pool at the moment of the call, and inside a nested scope the
/// caller can still name the outer one. So a pool reference may gate an
/// operation but must never appear in a returned lifetime (see the
/// counterexample in [`crate::weak`]'s module docs; `tests/weak.rs`'s
/// `a_pool_parameter_never_mints_a_lifetime` enforces it). No bound on `R`
/// fixes that: a `Cell` in the outer scope can carry such a borrow out of a
/// closure that returns `()`.
#[inline]
pub fn autoreleasepool<R>(f: impl FnOnce(&AutoreleasePool) -> R) -> R {
    // SAFETY: the token is a local of this frame, dropped below or during an
    // unwind out of `f`; anything pushed after it was pushed inside `f` and has
    // already returned or unwound.
    let pool = unsafe { AutoreleasePool::new() };
    let out = f(&pool);
    drop(pool);
    out
}

/// Build a +1 `NSString` holding exactly `s`.
///
/// Uses `alloc` + `initWithCharacters:length:` so the result is owned, not
/// autoreleased, and needs no pool. The string is built from its UTF-16 code
/// units, the unit an `NSString` stores and [`ns_string_to_rust`] reads back:
/// `initWithBytes:length:encoding:` decodes UTF-8 as an external
/// representation and drops a leading U+FEFF as a byte-order mark.
#[must_use]
pub fn ns_string(s: &str) -> Option<Obj> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let cls = class(c"NSString");
    // SAFETY: `alloc` returns +1 uninitialised; `initWithCharacters:length:`
    // (`@32@0:8r^S16Q24`) consumes it and returns +1 or nil (`from_owned` maps
    // nil to `None`). It copies `units.len()` code units during the call only.
    unsafe {
        let alloc: unsafe extern "C-unwind" fn(ClassPtr, Sel) -> Id = msg();
        let raw = alloc(cls, crate::sel!(alloc));
        if raw.is_null() {
            return None;
        }
        let init: unsafe extern "C-unwind" fn(Id, Sel, *const u16, usize) -> Id = msg();
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
///
/// Reads the string's counted UTF-16 code units (`-length`, then
/// `-getCharacters:range:`), never a C string: `-UTF8String` stops at the
/// first U+0000, and answers NULL for a string holding an unpaired surrogate,
/// so a paste of `a\0b` read as `a` and a paste cut mid-surrogate-pair read as
/// nothing at all. An unpaired surrogate, which an `NSString` can hold and a
/// Rust `String` cannot, becomes U+FFFD; everything else is exact.
///
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
            let length: unsafe extern "C-unwind" fn(Id, Sel) -> usize = msg();
            let n = length(s, crate::sel!(length));
            if n == 0 {
                return String::new();
            }
            let mut units: Vec<u16> = Vec::with_capacity(n);
            let get: unsafe extern "C-unwind" fn(Id, Sel, *mut u16, crate::NSRange) = msg();
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
///
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
            let desc: unsafe extern "C-unwind" fn(Id, Sel) -> Id = msg();
            let d = desc(err, sel(c"localizedDescription"));
            if d.is_null() {
                return "(no localizedDescription)".to_owned();
            }
            ns_string_to_rust(d)
        }
    })
}
