// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The raw runtime surface: symbols, typed sends, ownership, pools.
//!
//! Most of this module was PROMOTED from `aterm-gpu/src/metal/ffi.rs`, which
//! established these conventions and states in its own header that it was the
//! first in the tree to talk to the ObjC runtime directly. Promoted, not
//! copied: three things that file gets wrong or does not need are DIFFERENT
//! here, and each is marked where it lives —
//!
//! * the autorelease pool is a SCOPE, because the RAII token that file uses
//!   lets safe code pop out of order (see [`AutoreleasePool`]);
//! * [`msg`] chooses between `objc_msgSend` and `objc_msgSend_stret` from the
//!   prototype's return type, because that file's single entry point is correct
//!   only for the selector set it happens to send (see [`returns_indirectly`]);
//! * the symbols that file deliberately never needed — class-pair creation,
//!   super sends, protocols, [`autorelease`] — are bound here.

use std::ffi::{CStr, c_char, c_void};

/// An Objective-C object pointer. Null is the ObjC `nil` and is always a valid
/// value to send a message to (it returns zero), so this is deliberately a raw
/// pointer rather than a `NonNull`.
///
/// In a METHOD SIGNATURE this type means `id`: [`crate::encode::Encode`] maps
/// it to `"@"`. This crate models exactly one object-pointer type, which is
/// what all 33 aterm-gui methods and all 71 winit methods actually need — none
/// of them takes a non-object `void *`.
pub type Id = *mut c_void;
/// An Objective-C selector — an interned, immortal string the runtime owns.
pub type Sel = *const c_void;
/// An Objective-C class object. A class is itself an object, so `Id` and this
/// are the same shape; the alias exists to make the message target obvious.
pub type ClassPtr = *mut c_void;
/// An Objective-C protocol object, as returned by `objc_getProtocol`.
pub type ProtocolPtr = *mut c_void;
/// An `Ivar` handle, as returned by `class_getInstanceVariable`.
pub type IvarPtr = *const c_void;

/// The `objc_super` struct `objc_msgSendSuper` takes in place of a receiver.
///
/// `receiver` is the instance; `super_class` is the class whose implementation
/// the send should START looking from — for `[super foo]` inside a method of
/// class `C`, that is `C`'s superclass, NOT `C`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ObjcSuper {
    /// The object the message is actually delivered to.
    pub receiver: Id,
    /// Where method lookup starts (the SUPERCLASS of the defining class).
    pub super_class: ClassPtr,
}

#[link(name = "objc")]
unsafe extern "C" {
    /// Declared with NO parameter list on purpose — see the crate docs. Every
    /// call goes through [`msg`], which casts this to the exact prototype of
    /// the selector being sent.
    fn objc_msgSend();
    /// Same discipline as [`objc_msgSend`], but the first argument is a
    /// `*const ObjcSuper` rather than the receiver. Cast through [`msg_super`].
    fn objc_msgSendSuper();
    fn objc_getClass(name: *const c_char) -> ClassPtr;
    fn objc_getProtocol(name: *const c_char) -> ProtocolPtr;
    fn object_getClass(obj: Id) -> ClassPtr;
    fn class_getSuperclass(cls: ClassPtr) -> ClassPtr;
    fn class_getName(cls: ClassPtr) -> *const c_char;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_retain(obj: Id) -> Id;
    fn objc_release(obj: Id);
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
    /// Hand a +1 reference to the innermost pool. Bound for [`autorelease`],
    /// which is what a declared method that RETURNS an object needs: a method
    /// not named `new*`/`alloc`/`copy*`/`mutableCopy*` owes its caller a +0
    /// reference, and `objc_autorelease` is the only way to produce one from a
    /// +1 without dropping it on the floor.
    fn objc_autorelease(obj: Id) -> Id;

    // --- class-pair creation. ZERO uses of any of these existed in the tree
    // before this crate; `metal/ffi.rs` never needed to DEFINE a class, only to
    // message existing ones. This is the capability gap the objc2 cluster was
    // being paid for. ---
    pub(crate) fn objc_allocateClassPair(
        superclass: ClassPtr,
        name: *const c_char,
        extra_bytes: usize,
    ) -> ClassPtr;
    pub(crate) fn objc_registerClassPair(cls: ClassPtr);
    // These three return the Objective-C `BOOL`, NOT Rust's `bool`: on
    // `x86_64-apple-darwin` — the compat slice this repo lipos into its
    // universal binary — `BOOL` is `signed char`, whose 256 values do not all
    // have a valid `bool` bit pattern. See [`crate::Bool`].
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
    pub(crate) fn class_getInstanceVariable(cls: ClassPtr, name: *const c_char) -> IvarPtr;
    pub(crate) fn ivar_getOffset(ivar: IvarPtr) -> isize;
}

// The INDIRECT-RETURN entry points, which exist ONLY on x86_64. `libobjc` on
// arm64 exports no such symbol — MEASURED, by linking a C file that references
// it: `-arch x86_64` links, `-arch arm64` fails with `Undefined symbols for
// architecture arm64: "_objc_msgSend_stret"`. So the `cfg` is not tidiness; a
// reference reaching an arm64 link line stops the build, and one reaching it
// only under some feature combination would stop it in someone else's lane.
// `objc_msgSend_stret` is what a send whose return value is
// classified MEMORY by the System V x86-64 ABI must go through: the caller
// passes a hidden pointer to the result in `RDI`, which is where plain
// `objc_msgSend` expects `self`, so every argument shifts by one register and
// the send silently reads garbage. On AAPCS64 the indirect-return pointer has
// its own register (`x8`) and never displaces an argument, which is why arm64
// has one entry point and needs no variant.
//
// `objc_msgSend_fpret` (x86_64 `long double`) and `objc_msgSend_fp2ret`
// (`_Complex long double`) are deliberately NOT bound: Rust has no type with
// the x87 80-bit ABI, so no prototype this crate can express reaches them.
// Measured with clang, not assumed — see `tests/abi.rs`, which records the
// per-return-type codegen and the boundary at 16 bytes.
#[cfg(target_arch = "x86_64")]
#[link(name = "objc")]
unsafe extern "C" {
    /// Same discipline as [`objc_msgSend`]: no parameter list, cast per send.
    fn objc_msgSend_stret();
    /// Same, against the super entry point.
    fn objc_msgSendSuper_stret();
}

// Foundation supplies `NSString`, which [`ns_string`] and [`ns_error_string`]
// need. This block declares no symbol: its ONLY job is the `-framework
// Foundation` link directive, because every class this crate reaches for lives
// there rather than in `libobjc`. (`metal/ffi.rs` removed an empty
// `#[link(name = "QuartzCore")]` block because the claim beside it was FALSE —
// nothing in that file touched QuartzCore. Here the claim is true and load-
// bearing: drop this block and `class(c"NSString")` returns nil in a binary
// that links nothing else from Foundation, which `aterm-objc`'s own test
// binary does not.)
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

/// A message-send prototype, decomposed far enough that [`msg`] can read its
/// RETURN TYPE and pick the right entry point.
///
/// Implemented for `unsafe extern "C" fn(..) -> R` at every arity this crate
/// can be asked for (up to twelve parameters; the widest real selector in the
/// tree, `addObserver:selector:name:object:`, is six). The bound is what makes
/// the `_stret` choice AUTOMATIC rather than a rule a call site is asked to
/// remember — see [`returns_indirectly`].
///
/// # Safety
/// An implementor must be a bare `unsafe extern "C"` function pointer whose
/// return type is `Ret`. Nothing else may implement it: [`msg`] transmutes a
/// raw symbol address into `Self` and dispatches on `size_of::<Self::Ret>()`,
/// so a lying `Ret` picks the wrong `objc_msgSend` variant.
pub unsafe trait MsgFn: Copy {
    /// The prototype's return type.
    type Ret;
}

macro_rules! impl_msg_fn {
    ($($arg:ident),*) => {
        // SAFETY: the implementing type IS an `unsafe extern "C"` function
        // pointer and `Ret` is written from its own return position, so it
        // cannot disagree with itself.
        unsafe impl<Ret, $($arg),*> MsgFn for unsafe extern "C" fn($($arg),*) -> Ret {
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

/// Whether a `R` comes back through a HIDDEN POINTER on this target's C ABI —
/// which decides whether a send must go through `objc_msgSend_stret`.
///
/// * `aarch64-apple-darwin`: always `false`. AAPCS64 passes the indirect-result
///   pointer in `x8`, a register no argument ever occupies, so one entry point
///   serves every return type and `libobjc` exports no `_stret` variant at all.
/// * `x86_64-apple-darwin`: `size_of::<R>() > 16`. MEASURED against clang, not
///   read off the System V document — a 16-byte struct of two `double`s comes
///   back in `xmm0`/`xmm1` (plain `objc_msgSend`), a 17-byte one goes to memory
///   (`objc_msgSend_stret`). See `tests/abi.rs`.
///
/// The one gap, named because a silent gap is the thing this function exists to
/// prevent: on x86_64 a `#[repr(C, packed)]` type of 16 bytes or fewer is also
/// classified MEMORY, and `size_of` cannot see the packing. This crate declares
/// no packed type and neither does any Objective-C framework struct.
#[inline]
#[must_use]
pub const fn returns_indirectly<R>() -> bool {
    cfg!(target_arch = "x86_64") && size_of::<R>() > 16
}

/// `objc_msgSend_stret`, on the one architecture that has it.
#[cfg(target_arch = "x86_64")]
#[inline]
fn stret_entry() -> *const c_void {
    objc_msgSend_stret as *const c_void
}

/// Unreachable off x86_64: [`returns_indirectly`] is a `const false` there, so
/// the branch that calls this folds away before codegen. It is a diverging
/// function rather than a `cfg` around the body of [`msg`] so that both
/// architectures compile the SAME send logic.
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
/// The choice is made by [`returns_indirectly`] from `F`'s own return type in a
/// `const` block, so it costs nothing at run time and — this is the point — a
/// call site CANNOT get it wrong. `metal/ffi.rs` had the same hazard and was
/// safe only by accident of its selector set; here the type system picks.
///
/// # Safety
/// `F` must be the EXACT C prototype of the selector about to be sent,
/// including the implicit `(self, _cmd)` leading pair. Getting this wrong is
/// undefined behaviour on every Apple ABI.
#[inline]
#[must_use]
pub unsafe fn msg<F: MsgFn>() -> F {
    // A function pointer is exactly one pointer wide, so this rejects an `F`
    // that could not be one. It does NOT prove `F` is a function type — a
    // pointer-sized `F` would pass — which is why `MsgFn` is the real guard and
    // is `unsafe` to implement.
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    let entry = if const { returns_indirectly::<<F as MsgFn>::Ret>() } {
        stret_entry()
    } else {
        objc_msgSend as *const c_void
    };
    // SAFETY: `entry` is the address of a function symbol, so it is a valid
    // function pointer, and the assertion above pins `F` to pointer width. The
    // caller's `F` supplies the prototype; the entry point matches `F`'s return
    // class by construction.
    unsafe { std::mem::transmute_copy(&entry) }
}

/// Cast the untyped `objc_msgSendSuper` to a concrete prototype, with the same
/// automatic `_stret` selection as [`msg`].
///
/// # Safety
/// `F` must be the EXACT C prototype of the selector about to be sent, with
/// `*const ObjcSuper` in the receiver position and `Sel` second. The same
/// register-corruption rule as [`msg`] applies.
#[inline]
#[must_use]
pub unsafe fn msg_super<F: MsgFn>() -> F {
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

/// Resolve a selector from a checked C-string literal, e.g. `sel(c"length")`.
///
/// This is the UNCACHED form `metal/ffi.rs` uses — one `sel_registerName` per
/// send. [`crate::sel!`] is the cached form and is what the per-frame call
/// sites should use; see that macro for the measurement.
#[inline]
#[must_use]
pub fn sel(name: &'static CStr) -> Sel {
    // SAFETY: `CStr` guarantees one trailing NUL and no interior NUL;
    // `sel_registerName` copies it into the runtime's immortal selector table.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// Look up a class from a checked C-string literal, e.g. `class(c"NSString")`.
#[inline]
#[must_use]
pub fn class(name: &'static CStr) -> ClassPtr {
    // SAFETY: `CStr` supplies the exact C-string contract. Classes are
    // immortal; the returned pointer is never released.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// Look up a protocol by name, e.g. `protocol(c"NSMenuDelegate")`. Null when
/// the protocol is not present in the process — which, for an AppKit protocol,
/// means AppKit is not linked into THIS binary rather than that the name is
/// wrong.
#[inline]
#[must_use]
pub fn protocol(name: &'static CStr) -> ProtocolPtr {
    // SAFETY: `CStr` supplies the exact C-string contract. Protocol objects are
    // immortal and are never released.
    unsafe { objc_getProtocol(name.as_ptr()) }
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
/// `cls` must be a live class object, or null. `metal/ffi.rs` left the
/// equivalent accessors safe; this crate does not, because a `ClassPtr` here
/// can come from a caller rather than only from [`class`] two lines above.
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

/// An owned (+1) Objective-C object. The ONLY place `objc_release` is called
/// for untyped objects (see [`crate::Retained`] for the typed twin).
///
/// Wrapping every `new*` result is what keeps a renderer that rebuilds
/// pipelines on every resize from leaking one object per event — the same
/// `CfOwned` argument `aterm-http/src/verifier/apple.rs` makes for CoreFoundation.
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

    /// Retain a BORROWED reference (an autoreleased or +0 return) to +1.
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

    /// A second +1 handle to the SAME object (an `objc_retain`). Deliberately
    /// not a `Clone` impl: every clone of a raw-pointer holder should be
    /// visibly a retain at the call site, not something a derive can smuggle in.
    #[must_use]
    pub fn clone_retained(&self) -> Self {
        // SAFETY: `self.0` is non-null and live by construction (this holder
        // owns a +1 reference); `objc_retain` is thread-safe.
        unsafe { Self::retain(self.0).expect("retaining a live non-null object") }
    }

    /// Give up ownership WITHOUT releasing — the caller inherits the +1.
    #[must_use]
    pub fn into_raw(self) -> Id {
        let id = self.0;
        std::mem::forget(self);
        id
    }

    /// Hand the +1 to the innermost pool and return the now-BORROWED pointer.
    ///
    /// This is what a declared method that returns an object owes its caller.
    /// Objective-C's naming rule is not a convention the runtime enforces — it
    /// is what every caller, ARC included, compiles against: a method whose
    /// name does not begin `new`/`alloc`/`copy`/`mutableCopy` returns +0, so
    /// returning a +1 leaks one object per call and returning a pointer whose
    /// owner is about to drop returns a dangling one. See [`autorelease`].
    #[must_use]
    pub fn autorelease(self) -> Id {
        // SAFETY: `self` owns exactly one +1 reference, which `into_raw` hands
        // over without releasing and `objc_autorelease` then takes ownership
        // of; the pointer that comes back is borrowed until the pool pops.
        unsafe { autorelease(self.into_raw()) }
    }
}

impl Drop for Obj {
    /// Releasing an ObjC object runs its `dealloc`, and many `dealloc` paths
    /// autorelease into whatever frame the drop happens in. That obligation is
    /// the CALLER's — hold an [`AutoreleasePool`] across the scope — exactly as
    /// `metal/ffi.rs` measured and documented it; a hidden push/pop per release
    /// would buy nothing and cost two runtime calls on a per-frame path.
    fn drop(&mut self) {
        // SAFETY: `self.0` is non-null by construction (both constructors
        // reject null) and holds exactly one +1 reference, released once here.
        unsafe { objc_release(self.0) }
    }
}

/// Hand a +1 reference to the innermost autorelease pool on this thread.
///
/// The returned pointer is the SAME object, now BORROWED: it stays alive until
/// that pool pops, and the caller's obligation to release it is discharged.
///
/// # Safety
/// `obj` must be a +1 reference the caller is handing over, or null. There must
/// be a pool on this thread's stack — [`autoreleasepool`] is how this crate
/// puts one there. With no pool the runtime logs
/// `object … autoreleased with no pool in place - just leaking` and the object
/// is never freed; on the main thread of a Cocoa app the event loop always has
/// one, which is why a declared method may autorelease unconditionally.
#[inline]
#[must_use]
pub unsafe fn autorelease(obj: Id) -> Id {
    // SAFETY: the caller pins `obj` as a +1 reference (or null, which
    // `objc_autorelease` returns unchanged); the runtime takes ownership of
    // that reference and hands back the same pointer at +0.
    unsafe { objc_autorelease(obj) }
}

/// An `@autoreleasepool` scope.
///
/// # Why this is not a bare RAII guard
///
/// It used to be, and the comment beside its `Drop` said pools were "strictly
/// nested because this type is neither `Send` nor cloneable". THAT WAS FALSE,
/// and it was false in the file this one was promoted from
/// (`aterm-gpu/src/metal/ffi.rs`). Neither `!Send` nor `!Clone` implies LIFO
/// drop order — `drop(outer); drop(inner);` is ordinary safe Rust — and popping
/// the outer pool first releases everything the inner pool was holding, so any
/// autoreleased pointer borrowed from it dangles, and the inner pop then hits
/// the runtime's own check:
///
/// ```text
/// Invalid or prematurely-freed autorelease pool … Invalid autorelease pools
/// are a fatal error
/// ```
///
/// A use-after-free reachable from safe code is a soundness hole, not a
/// footgun, so the scope form is the sanctioned API and the token constructor
/// is `unsafe`. Nesting through [`autoreleasepool`] is structural: the inner
/// scope cannot outlive the outer one, so out-of-order pop is unconstructible.
///
/// The four lines that used to abort the process no longer compile:
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
/// `tests/pools.rs` runs the first snippet in a child process and asserts the
/// `SIGABRT`, so the hazard is measured rather than asserted.
pub struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    /// Push a pool and hand back its token.
    ///
    /// # Safety
    /// The returned value must be dropped BEFORE any pool pushed after it on
    /// this thread, and on the same thread that pushed it. [`autoreleasepool`]
    /// is the safe form and satisfies this by construction; call this directly
    /// only where a scope genuinely cannot express the lifetime, and say why.
    #[must_use]
    pub unsafe fn new() -> Self {
        // SAFETY: push/pop are the documented runtime entry points and are
        // balanced by `Drop`; the token is opaque and only handed back to pop.
        // LIFO order is the CALLER's obligation, stated above.
        Self(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the token from the matching push, and this type's
        // only safe constructor is `autoreleasepool`, whose scope makes this
        // pop the innermost one. A token minted by the `unsafe` `new` carries
        // that same obligation on its caller.
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

/// Run `f` inside an `@autoreleasepool`.
///
/// The pool pops when `f` returns, including when it returns by unwinding.
/// Autoreleased objects reached inside are valid for the body and NOT after it,
/// which is exactly the C scope, and nesting these calls is the only way this
/// crate lets a second pool exist — see [`AutoreleasePool`] for why that is a
/// soundness property rather than a style.
#[inline]
pub fn autoreleasepool<R>(f: impl FnOnce(&AutoreleasePool) -> R) -> R {
    // SAFETY: the token is a local of THIS frame and is dropped when the frame
    // ends — normally at the `drop` below, or during an unwind out of `f`.
    // Either way nothing pushed after it can still be live, because anything
    // that pushed did so inside `f` and has already returned or unwound.
    let pool = unsafe { AutoreleasePool::new() };
    let out = f(&pool);
    drop(pool);
    out
}

/// Build a +1 `NSString` from a Rust `&str`.
///
/// Uses `alloc` + `initWithBytes:length:encoding:` rather than the familiar
/// `+stringWithUTF8String:` so the result is OWNED rather than autoreleased —
/// no pool is required and the lifetime is explicit.
#[must_use]
pub fn ns_string(s: &str) -> Option<Obj> {
    const NS_UTF8: usize = 4;
    let cls = class(c"NSString");
    // SAFETY: `alloc` returns a +1 uninitialized instance; the follow-up
    // `initWithBytes:length:encoding:` consumes it and returns the initialized
    // +1 object (or nil, which `from_owned` maps to `None`). The byte pointer
    // is only read for `len` bytes during the call and is not retained.
    unsafe {
        let alloc: unsafe extern "C" fn(ClassPtr, Sel) -> Id = msg();
        let raw = alloc(cls, sel(c"alloc"));
        if raw.is_null() {
            return None;
        }
        let init: unsafe extern "C" fn(Id, Sel, *const u8, usize, usize) -> Id = msg();
        let obj = init(
            raw,
            sel(c"initWithBytes:length:encoding:"),
            s.as_ptr(),
            s.len(),
            NS_UTF8,
        );
        Obj::from_owned(obj)
    }
}

/// Copy an `NSString`'s UTF-8 bytes into an owned Rust `String`.
///
/// # Safety
/// `s` must be a live `NSString` (or nil, which yields an empty string).
#[must_use]
pub unsafe fn ns_string_to_rust(s: Id) -> String {
    if s.is_null() {
        return String::new();
    }
    autoreleasepool(|_| {
        // SAFETY: the caller pins `s` as a live NSString. `UTF8String` returns
        // an interior pointer valid until the enclosing pool pops; the bytes
        // are copied into an owned `String` before that happens.
        unsafe {
            let utf8: unsafe extern "C" fn(Id, Sel) -> *const c_char = msg();
            let p = utf8(s, sel(c"UTF8String"));
            if p.is_null() {
                return String::new();
            }
            CStr::from_ptr(p).to_string_lossy().into_owned()
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
        // SAFETY: `err` is a live `NSError`. `localizedDescription` returns an
        // autoreleased object valid until the pool pops, and its bytes are
        // copied into an owned `String` before that happens.
        unsafe {
            let desc: unsafe extern "C" fn(Id, Sel) -> Id = msg();
            let d = desc(err, sel(c"localizedDescription"));
            if d.is_null() {
                return "(no localizedDescription)".to_owned();
            }
            ns_string_to_rust(d)
        }
    })
}
