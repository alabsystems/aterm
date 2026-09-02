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

use std::ffi::{CStr, c_char, c_uint, c_void};

/// An Objective-C object pointer. Null is the ObjC `nil` and is always a valid
/// value to send a message to (it returns zero), so this wraps a raw pointer
/// rather than a `NonNull`.
///
/// # Why this is a NEWTYPE and not `*mut c_void`
///
/// For the same reason [`Sel`] is, and the second half of the same defect. This
/// was `pub type Id = *mut c_void;`, and the [`crate::Encode`] impl that gave
/// `Id` its `"@"` was therefore written on `*mut c_void` itself, justified by
/// "in this crate a MUTABLE raw void pointer in a method position always means
/// `id` … nothing in aterm's 33 methods or winit's 71 passes a non-object
/// `void *` in that position".
///
/// THAT CLAIM WAS FALSE, and false at the very site the crate named as W2's
/// next piece of work. KVO's callback is
/// `- (void)observeValueForKeyPath:(NSString *)k ofObject:(id)o
/// change:(NSDictionary *)c context:(void *)ctx` — the SDK's
/// `NSKeyValueObserving.h` spells that last parameter `void *`, NOT
/// `const void *`; `objc2-foundation`'s generated binding writes it
/// `context: *mut c_void`; and `vendor/winit/src/platform_impl/macos/
/// window_delegate.rs:456` — the file this crate points at for W2 — writes
/// `_context: *mut c_void`. MEASURED with clang on this box, the compiler emits
///
/// ```text
/// observeValueForKeyPath:ofObject:change:context:   v48@0:8@16@24@32^v40
/// ```
///
/// i.e. `^v` in the fourth argument, while the old mapping would have
/// registered `@`. Declaring an opaque caller-owned pointer as an OBJECT is
/// strictly worse than the `":"` that motivated `Sel`'s newtype: `"@"` invites
/// `NSInvocation`, forwarding and accessibility to RETAIN it.
///
/// The [`crate::Encode`] impl for `"@"` now lives on this type, which frees
/// `*mut c_void` to mean what the runtime says it means (`"^v"`), and
/// `tests/adversary_w2c.rs` declares the KVO method AT THE `*mut` SPELLING and
/// reads the encoding back out of the runtime.
///
/// `#[repr(transparent)]` over the pointer, so it is still exactly the word the
/// runtime passes and is still FFI-safe in an `extern "C"` prototype.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(*mut c_void);

impl Id {
    /// The Objective-C `nil`.
    pub const NIL: Self = Self(std::ptr::null_mut());

    /// Wrap a raw object pointer.
    ///
    /// Safe, because holding an `id` is not what is dangerous — SENDING to one
    /// is, and every send in this crate is already `unsafe`.
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

    /// The address, WITHOUT exposing provenance — for identity comparisons and
    /// for `-hash`, which on `NSObject` is the instance pointer.
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
/// A NEWTYPE rather than the `*const c_void` alias it used to be, and the
/// reason is an ENCODING, not a type-safety preference. [`crate::Encode`] maps
/// a Rust type to the letter `class_addMethod` is told, and while `SEL` and
/// `const void *` are the same machine word they are `":"` and `"^v"` to the
/// runtime. As one alias they could not be told apart, so the impl had to pick:
/// it picked `":"`, which made the RIGHT choice for `doCommandBySelector:` and
/// the SILENT WRONG one for every `const void *context` — the shape
/// `observeValueForKeyPath:ofObject:change:context:` has, which is KVO, which
/// is what W2 needs for winit. Two types, two encodings, nothing to guess.
///
/// `#[repr(transparent)]` over the pointer, so it is still exactly the word the
/// runtime passes and is still FFI-safe in an `extern "C"` prototype.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sel(*const c_void);

impl Sel {
    /// The NIL selector.
    ///
    /// Not a defensive constant: `nil` is a legal, load-bearing value in a SEL
    /// position, and W2 could not port `aterm-gui` without it.
    /// `-[NSMenuItem initWithTitle:action:keyEquivalent:]` takes `nil` for
    /// "this row is a label, not a command" — the status item's information
    /// rows and the menu bar's disabled headers are exactly that — and
    /// `-[NSControl setAction:]` takes it to unwire a control.
    ///
    /// It is a CONSTANT rather than a public `from_ptr`, which stays
    /// `pub(crate)`: a selector this crate hands out is either interned by
    /// `sel_registerName` or is this null, and there is no third source. A
    /// nil SEL is inert on every path the runtime has — sending it is the one
    /// thing that is not allowed, and `objc_msgSend` with a nil selector is a
    /// crash rather than a no-op, which is why this is only ever an ARGUMENT.
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

    /// The selector's NAME, borrowed from the runtime's immortal selector
    /// table.
    ///
    /// The same `sel_getName` call [`Sel`]'s `Debug` makes, given a name of its
    /// own because an ENUMERATED selector — one that came back from
    /// [`class_methods`] rather than from a literal a caller typed — has no
    /// other way to be printed, matched or reported. `"(null)"` for
    /// [`Sel::NULL`], so a report can name the null selector instead of
    /// crashing while describing it.
    #[must_use]
    pub fn name(self) -> &'static CStr {
        if self.0.is_null() {
            return c"(null)";
        }
        // SAFETY: `self` is a non-null selector obtained from the runtime, so
        // `sel_getName` returns a NUL-terminated string in libobjc's immortal
        // selector table — never freed, so `'static` is honest.
        unsafe { CStr::from_ptr(sel_getName(self)) }
    }

    /// Wrap a pointer the runtime returned. Not public: every selector this
    /// crate hands out comes from `sel_registerName`, and one that did not
    /// would be a dangling pointer the moment it was sent.
    #[inline]
    pub(crate) const fn from_ptr(ptr: *const c_void) -> Self {
        Self(ptr)
    }
}

// SAFETY: a selector is a pointer into `libobjc`'s IMMORTAL selector table.
// It is never freed, never written through, and interning is idempotent and
// internally synchronised, so the pointer is valid on every thread for the
// life of the process. This is the same fact `SelCache`'s `Relaxed` ordering
// rests on, stated once here rather than re-argued at each use.
unsafe impl Send for Sel {}
// SAFETY: as above — the pointee is immutable and immortal.
unsafe impl Sync for Sel {}

impl std::fmt::Debug for Sel {
    /// Prints the selector's NAME, through `sel_getName` rather than by casting
    /// the pointer to a `char *`. On Apple's runtime a `SEL` happens to BE a
    /// pointer to its own name, and reading it that way works — but that is an
    /// implementation detail of `objc4`, not part of the ABI this crate is
    /// entitled to rely on, and `sel_getName` is one call that costs nothing on
    /// a debug path.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_null() {
            return f.write_str("Sel(null)");
        }
        // SAFETY: `self` is a non-null selector from `sel_registerName`, so
        // `sel_getName` returns a NUL-terminated string the runtime owns and
        // never frees.
        let name = unsafe { CStr::from_ptr(sel_getName(*self)) };
        write!(f, "Sel({})", name.to_string_lossy())
    }
}

/// An Objective-C class object.
///
/// A class is itself an object and has the same machine shape as an [`Id`], but
/// it is NOT the same thing to the runtime and it does not have the same
/// encoding: `@encode(Class)` is `"#"`, not `"@"`. MEASURED with clang on this
/// box, `- (void)setDelegateClass:(Class)c` registers
///
/// ```text
/// setDelegateClass:                              v24@0:8#16
/// ```
///
/// This used to be `pub type ClassPtr = *mut c_void;`, the SAME alias as `Id`
/// and `ProtocolPtr`, so no [`crate::Encode`] impl could tell the three apart
/// and the one impl that existed said `"@"` for all of them. That is the defect
/// [`Sel`] was newtyped for, one type over — fixed on the `const` side of the
/// void pointer while three `*mut` spellings stood behind a single impl. All
/// three are newtypes now, and each carries the letter clang emits: `"@"` for
/// [`Id`], `"#"` here, `"@"` for [`ProtocolPtr`] (`@encode(Protocol *)` is `@`
/// — measured, not assumed, because it is the one of the three that does NOT
/// follow from its own name).
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

    /// The address, with provenance exposed — what [`crate::ClassMeta`] stores
    /// so it can live in a `OnceLock`.
    #[inline]
    #[must_use]
    pub fn expose_provenance(self) -> usize {
        self.0.expose_provenance()
    }

    /// The address, WITHOUT exposing provenance — for the identity assertions
    /// that prove a `OnceLock`-registered class is registered exactly once.
    #[inline]
    #[must_use]
    pub fn addr(self) -> usize {
        self.0.addr()
    }

    /// This class object AS an object, for the sends that message a class
    /// (`+alloc`, `+sharedWorkspace`). The conversion is explicit because the
    /// two types are deliberately distinct in a method signature.
    #[inline]
    #[must_use]
    pub const fn as_id(self) -> Id {
        Id::from_ptr(self.0)
    }
}

impl std::fmt::Debug for ClassPtr {
    /// Prints the class's NAME, through `class_getName`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: `self` is either null — which `class_name` reports as
        // `"(nil)"` without dereferencing — or a class object, which is
        // immortal once it has been handed out.
        let name = unsafe { class_name(*self) };
        write!(f, "Class({})", name.to_string_lossy())
    }
}

/// An Objective-C protocol object, as returned by `objc_getProtocol`.
///
/// A newtype for [`ClassPtr`]'s reason; `@encode(Protocol *)` is `"@"`, which
/// is what the [`crate::Encode`] impl says — measured, because "it is called
/// `Protocol` so it must encode like `Class`" is exactly the guess that would
/// be wrong.
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
///
/// Left as a bare `*const c_void` alias, deliberately: an `Ivar` is an opaque
/// runtime handle that NEVER appears in a method signature, so it never reaches
/// [`crate::Encode`] and there is nothing for a newtype to disambiguate.
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
    /// The selector's name, for [`Sel`]'s `Debug`.
    fn sel_getName(sel: Sel) -> *const c_char;
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

    // --- reading a registered method back out. The WRITE half of the encoding
    // contract (`class_addMethod`'s `types`) has been here since W1; this is
    // the READ half, and W2 is what needed it: a port whose only evidence is
    // "it compiles" is precisely the trap this campaign has already been caught
    // by, and the runtime's own answer to "what did you register?" is the
    // cheapest evidence there is. ---
    fn class_getInstanceMethod(cls: ClassPtr, name: Sel) -> *const c_void;
    fn method_getTypeEncoding(method: *const c_void) -> *const c_char;

    // --- and the IMP behind a row, which is a different question from its
    // ENCODING and the only one that can answer "whose code will run?".
    // `vendor/winit/.../app.rs` does not DECLARE a class in product code at
    // all: it SWIZZLES `-[NSApplication sendEvent:]` with
    // `method_setImplementation`, which leaves the type encoding untouched by
    // construction. So every encoding check in the tree passes whether that
    // swizzle happened or not, and the only live evidence that it did is the
    // address of the function the runtime will call. ---
    fn method_getImplementation(method: *const c_void) -> *const c_void;

    // --- and the WHOLE table, which is the only way to ask a question about a
    // class that does not begin by naming what you expect to find. Every
    // encoding check this crate had before W3 started from a selector somebody
    // wrote down; two compile-verified plants (an argument retyped `Id` ->
    // `Bool`, and a protocol dropped from the `protocols:` list) proved that a
    // checker holding its own copy of the list checks its own copy. These three
    // are what let a caller enumerate what the runtime ACTUALLY registered and
    // then go looking for each row's authority. See
    // `crates/aterm-gui/examples/objc_live_class_audit.rs`. ---
    fn class_copyMethodList(cls: ClassPtr, out_count: *mut c_uint) -> *mut *const c_void;
    fn method_getName(method: *const c_void) -> Sel;
    fn class_copyProtocolList(cls: ClassPtr, out_count: *mut c_uint) -> *mut ProtocolPtr;
    fn protocol_getName(proto: ProtocolPtr) -> *const c_char;

    // --- and the PROTOCOL's own answer, which is the authority a delegate
    // method has to match. `class_getInstanceMethod` reports what SOMEBODY
    // registered; a protocol reports what the framework will CALL. For a
    // declared class the two are only the same if the port got it right, and
    // W3's census found one winit row where they are not
    // (`draggingEntered:`: `B@:@` registered against `Q@:@` declared). ---
    fn protocol_getMethodDescription(
        proto: ProtocolPtr,
        sel: Sel,
        is_required_method: crate::encode::Bool,
        is_instance_method: crate::encode::Bool,
    ) -> ObjcMethodDescription;
}

// `class_copyMethodList` and `class_copyProtocolList` hand back a
// malloc'd array the CALLER owns — the only two runtime accessors this crate
// binds that allocate. `free` arrives with `std`'s libc; binding it here is
// cheaper and more honest than leaking one array per audited class.
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
/// Implemented for `unsafe extern "C" fn(..) -> R` up to SIXTEEN parameters,
/// INCLUDING the implicit `self` and `_cmd` — so up to a fourteen-colon
/// selector. Counting the implicit pair is the whole of the correction: the
/// ceiling used to be twelve parameters and was described as "every arity this
/// crate can be asked for", which was false by demonstration, because
/// `declare_class!` has no ceiling at all. An eleven-ARGUMENT method declared,
/// registered and dispatched from Objective-C but could not be SENT from Rust
/// (`E0277`), which is the arity trap D1 closed, one level up and one register
/// further out.
///
/// Sixteen is a ROUND NUMBER that clears every real signature by a wide margin.
/// It used to be justified as stopping "where the ABI stops being uniform", and
/// THAT IS FALSE: AAPCS64 has eight integer argument registers and System V
/// x86-64 has six, so uniformity ends at eight and at six respectively — far
/// below sixteen, and at two different places. Every arity past those spills to
/// the stack, which the C ABI handles perfectly well and which `objc_msgSend`
/// tail-calls through unchanged; there is no ABI edge at sixteen to stop at.
/// The honest statement is the one this paragraph now makes: sixteen is where
/// the macro repetition stops, chosen for headroom, not for a machine reason.
/// AppKit's widest initializer,
/// `initWithBitmapDataPlanes:pixelsWide:pixelsHigh:bitsPerSample:samplesPerPixel:hasAlpha:isPlanar:colorSpaceName:bytesPerRow:bitsPerPixel:`,
/// is exactly ten arguments — twelve parameters — and the widest real selector
/// otherwise in the tree, `addObserver:selector:name:object:`, is four.
///
/// # The ceiling is loud at BOTH ends
///
/// It used to be loud only here. [`crate::declare_class!`] had no arity limit
/// at all, so a fifteen-argument method REGISTERED, answered
/// `respondsToSelector:` with `YES`, dispatched from Objective-C — and was
/// unreachable from Rust, because no `MsgFn` impl could build its prototype.
/// That is D1's trap and F1's trap a third time. The macro now carries a
/// `const` assertion of its own, so the class fails to compile with a message
/// naming this ceiling instead of registering a method Rust cannot call.
///
/// The bound is what makes the `_stret` choice AUTOMATIC rather than a rule a
/// call site is asked to remember — see [`returns_indirectly`].
///
/// Fourteen arguments — sixteen parameters — is sendable; `tests/adversary_w2c.rs`
/// declares such a method, registers it and sends it. Fifteen is not, and the
/// wall is loud rather than silent:
///
/// ```compile_fail
/// # use aterm_objc::{Id, Sel, msg};
/// // 17 parameters: `self`, `_cmd`, and fifteen arguments.
/// let _: unsafe extern "C" fn(
///     Id, Sel,
///     i64, i64, i64, i64, i64, i64, i64, i64,
///     i64, i64, i64, i64, i64, i64, i64,
/// ) -> i64 = unsafe { msg() };
/// ```
///
/// # Safety
/// An implementor must be a bare `unsafe extern "C"` function pointer whose
/// return type is `Ret`. Nothing else may implement it: [`msg`] transmutes a
/// raw symbol address into `Self` and dispatches on `size_of::<Self::Ret>()`,
/// so a lying `Ret` picks the wrong `objc_msgSend` variant.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a message-send prototype `aterm-objc` can build",
    label = "not an `unsafe extern \"C\" fn(..) -> R` of at most 16 parameters",
    note = "a prototype must be a bare `unsafe extern \"C\"` function pointer whose             first two parameters are the implicit `self` and `_cmd` — e.g.             `unsafe extern \"C\" fn(Id, Sel, i64) -> Bool`",
    note = "the ceiling is SIXTEEN parameters INCLUDING `self` and `_cmd`, i.e. a             fourteen-colon selector; a wider one has no `MsgFn` impl and             `declare_class!` refuses to register it for the same reason"
)]
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
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_msg_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_msg_fn!(
    A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14
);
impl_msg_fn!(
    A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15
);

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
/// # The term `size_of` cannot see
///
/// System V x86-64 classifies an object MEMORY if it is larger than eight
/// eightbytes **or it contains unaligned fields**, and the second clause has no
/// size threshold at all. MEASURED with clang on this box, real ObjC sends,
/// `-arch x86_64 -O1 -S`, reading which symbol the call lands on:
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
/// Two rows decide the design. A THREE-byte packed struct goes indirect, so
/// there is no size below which the check can be skipped. And `packed { int
/// a,b,c,d }` (16 bytes, align 1) goes DIRECT while `packed { char; short; }`
/// (3 bytes, align 1) goes INDIRECT — identical `align_of`, opposite ABI —
/// while `packed { char; short; }` and `struct { char c[3]; }` have the SAME
/// size AND the SAME alignment and still differ. So no predicate over
/// `size_of` and `align_of` can decide this, and the answer has to come from
/// the type's author. It does, as [`crate::Encode::HAS_UNALIGNED_FIELDS`],
/// which every impl in this crate takes the `false` default for and a packed
/// type must set by hand — and [`msg`] will not send a return type that has no
/// `Encode` impl at all, so there is no way to reach this function without
/// having answered the question.
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
/// The `Encode` bound on the return type is not decoration: it is what closes
/// the packed-struct gap described on [`returns_indirectly`], because the ABI
/// question it answers is one no amount of `size_of` can. It also refuses a
/// `-> bool` prototype outright — `bool` deliberately has no [`crate::Encode`]
/// impl, so an ObjC `BOOL` return can only be spelled [`crate::Bool`], which is
/// D3's rule enforced on the RETURN side instead of merely documented there.
/// It caught two live `-> bool` sends when it landed, one of them in
/// `aterm-gui`.
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
/// let _: unsafe extern "C" fn(Id, Sel) -> Packed = unsafe { msg() };
/// ```
///
/// Nor does a `BOOL` return spelled as a Rust `bool`:
///
/// ```compile_fail
/// # use aterm_objc::{Id, Sel, msg};
/// let _: unsafe extern "C" fn(Id, Sel) -> bool = unsafe { msg() };
/// ```
///
/// # Safety
/// `F` must be the EXACT C prototype of the selector about to be sent,
/// including the implicit `(self, _cmd)` leading pair. Getting this wrong is
/// undefined behaviour on every Apple ABI.
#[inline]
#[must_use]
pub unsafe fn msg<F: MsgFn>() -> F
where
    <F as MsgFn>::Ret: crate::encode::Encode,
{
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
/// register-corruption rule as [`msg`] applies, and so does its `Encode` bound.
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

/// The type encoding the runtime holds for `cls`'s INSTANCE method `name`, or
/// `None` if the class does not implement it.
///
/// This is the mirror of what [`crate::declare_class!`] wrote through
/// `class_addMethod`, read back from the runtime's own method table. It exists
/// so a port can be checked against the encoding table rather than asserted to
/// match it: `method_types(cls, sel!(validateMenuItem:))` returning
/// `"B@:@"` on arm64 (`"c@:@"` on the x86_64 compat slice) is the difference
/// between a `BOOL` AppKit can read and one it cannot.
///
/// Inherited methods count — `class_getInstanceMethod` walks the superclass
/// chain — so a `None` means "no implementation anywhere above this class",
/// which for a selector this crate declared would mean registration failed.
///
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn method_types(cls: ClassPtr, name: Sel) -> Option<String> {
    // SAFETY: the caller guarantees `cls` is live; `name` is an interned
    // selector, and `class_getInstanceMethod` tolerates a selector the class
    // does not implement by returning NULL.
    let method = unsafe { class_getInstanceMethod(cls, name) };
    if method.is_null() {
        return None;
    }
    // SAFETY: `method` is a live `Method` handle from the call above, and
    // `method_getTypeEncoding` returns the NUL-terminated string the runtime
    // copied at `class_addMethod` time and owns for the life of the class.
    // It CAN be null for a method with no recorded types, which is why this is
    // checked rather than assumed.
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
/// The twin of [`method_types`], and the question that one cannot answer: an
/// encoding says what SHAPE the call has, an IMP says WHOSE CODE runs. They
/// come apart exactly where a swizzle lives — `method_setImplementation`
/// replaces the function and keeps the types — which is the case
/// `vendor/winit/src/platform_impl/macos/app.rs` is, and the reason this exists.
///
/// The returned pointer is a code address and is NEVER dereferenced or called
/// through by this crate; it is for identity comparison and for asking the
/// dynamic loader which image it came from.
///
/// # Safety
/// `cls` must be a live class object or null.
#[must_use]
pub unsafe fn method_imp(cls: ClassPtr, name: Sel) -> Option<*const c_void> {
    // SAFETY: the caller pins `cls` as live-or-null; `class_getInstanceMethod`
    // tolerates null and a selector the class does not implement, answering
    // NULL for both, and it walks the superclass chain exactly as
    // `method_types` does.
    let method = unsafe { class_getInstanceMethod(cls, name) };
    if method.is_null() {
        return None;
    }
    // SAFETY: `method` is a live `Method` handle from the call above.
    let imp = unsafe { method_getImplementation(method) };
    (!imp.is_null()).then_some(imp)
}

/// The type encoding a PROTOCOL declares for `name`, or `None` if the protocol
/// does not declare it.
///
/// This is the authority side of the encoding contract, and it is a different
/// question from [`method_types`]. `method_types` answers "what did somebody
/// register?"; this answers "what will the framework CALL?" — and for a
/// declared class the two agree only if the port got it right. It is what makes
/// a delegate row CHECKABLE rather than asserted: `NSDraggingDestination`
/// declares `-draggingEntered:` as `Q24@0:8@16` (an eight-byte
/// `NSDragOperation`), and a port that writes the Rust signature as `-> bool`
/// registers `B@:@` — a ONE-byte return where the framework reads eight. That
/// row exists in `vendor/winit`, and this function is how W3 found it in all
/// seventy-two rather than in the one it was told about.
///
/// Both the required and the optional table are searched, required first,
/// because a delegate protocol declares almost everything optional and the
/// caller almost never cares which table a row came from.
///
/// The string is the COMPILER-EMITTED form, with byte offsets
/// (`"v24@0:8@16"`). [`crate::strip_method_offsets`] is the other half of any
/// comparison against what this crate registers, which is offset-free by
/// design — see the [`crate::encode`] module note.
///
/// # Safety
/// `proto` must be a live protocol object (`nil` is tolerated and answers
/// `None`, since `objc_getProtocol` returns nil for a framework that is not
/// loaded).
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
    // SAFETY: the caller pins `proto` as live; `name` is an interned selector.
    // `protocol_getMethodDescription` tolerates a selector the protocol does
    // not declare by returning a ZEROED description, which is why `types` is
    // null-checked rather than assumed.
    let mut desc = unsafe {
        protocol_getMethodDescription(proto, name, crate::encode::Bool::YES, is_instance)
    };
    if desc.types.is_null() {
        // SAFETY: as above, against the OPTIONAL table — where every
        // `NSWindowDelegate` and `NSDraggingDestination` row this port cares
        // about actually lives.
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
///
/// # THIS IS NOT CACHED, AND IT COSTS MORE THAN THE SEND IT PRECEDES
///
/// [`sel!`](crate::sel) caches per call site because `metal/ffi.rs` resolving a
/// selector on every send was worth fixing. `objc_getClass` is the same shape —
/// a `libobjc` call that hashes a string — and it is NOT cached, so a call site
/// spelled `class(c"Foo")` pays that hash on every pass.
///
/// MEASURED on this box, release, 2x10^6 iterations per arm, four alternating
/// rounds (spread across rounds shown):
///
/// ```text
/// pthread_main_np()                          0.87 ns/op
/// objc_msgSend(+isMainThread), cached class   1.67-1.97 ns/op
/// objc_getClass("NSThread")  ALONE           16.94-17.46 ns/op
/// MainThread::new()  (getClass + the send)   18.24-18.68 ns/op
/// ```
///
/// So the lookup is ~10x the send it exists to address, and it is 91% of what
/// [`crate::MainThread::new`] costs — which is what [`crate::run_on_main`]
/// pays on EVERY call, including its direct path. A real drive
/// (`aterm-gui`'s `objc_ime_drive`, instrumented) performs **908** of these in
/// one short window+IME session, and there are 115 `class(c"…")` call sites in
/// the tree, several on per-frame Metal paths.
///
/// Nothing here is a correctness problem: classes are immortal and the pointer
/// is stable, which is exactly why a `ClassCache` mirroring [`crate::SelCache`]
/// would be sound and would take the number to ~2 ns. It is recorded rather
/// than fixed because the fix is an API addition, not a bug fix, and because a
/// cost this module states must be the cost it has.
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

/// Every instance method a class registered ITSELF, with the type encoding the
/// runtime holds for each.
///
/// # Why enumeration and not lookup
///
/// [`method_types`] answers "what is registered for the selector I named", and
/// every encoding check in this repository was built on it until W3. That shape
/// cannot see a row nobody named, and it cannot see that the list it was handed
/// has gone stale — which is not a hypothetical: two compile-verified plants in
/// `vendor/winit`'s ported delegate (an argument retyped `Id` -> [`Bool`], and
/// `NSWindowDelegate` deleted from the `protocols:` list) both passed a checker
/// that re-declared the selectors it intended to check. This function is the
/// other direction: ask the CLASS what it has, then go and find each row's
/// authority ([`protocol_method_types`] / [`method_types`]).
///
/// INHERITED METHODS ARE NOT INCLUDED. `class_copyMethodList` reports only what
/// this class object itself registered, which is exactly the set a
/// [`crate::declare_class!`] site is responsible for; the superclass chain is
/// [`method_types`]'s business.
///
/// The encoding is `None` for a method registered with no type string —
/// impossible through [`crate::declare_class!`], possible through raw
/// `class_addMethod`, and reported rather than hidden so an auditor can call it
/// a finding.
///
/// The order is the runtime's and is NOT declaration order; sort by name if a
/// stable report matters.
///
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn class_methods(cls: ClassPtr) -> Vec<(Sel, Option<String>)> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut count: c_uint = 0;
    // SAFETY: the caller pins `cls` as live. `class_copyMethodList` writes the
    // row count through `&mut count` and returns a malloc'd array of `Method`
    // handles that this function owns and frees below. It answers null with a
    // zero count for a class with no methods of its own.
    let list = unsafe { class_copyMethodList(cls, &raw mut count) };
    if list.is_null() {
        return Vec::new();
    }
    let mut rows = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // SAFETY: `i < count`, and the runtime guarantees `count` live `Method`
        // handles at `list`. A `Method` is owned by its class and outlives it.
        let method = unsafe { *list.add(i) };
        // SAFETY: `method` is a live handle from the array above.
        let sel = unsafe { method_getName(method) };
        // SAFETY: as above. The result CAN be null (a method registered with no
        // types), which is why it is checked rather than assumed.
        let types = unsafe { method_getTypeEncoding(method) };
        let encoding = if types.is_null() {
            None
        } else {
            // SAFETY: a non-null, NUL-terminated string the runtime copied at
            // registration time and owns for the life of the class.
            Some(
                unsafe { CStr::from_ptr(types) }
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        rows.push((sel, encoding));
    }
    // SAFETY: `list` came from `class_copyMethodList`, which documents the array
    // as the caller's to `free`. The `Method` handles it held are owned by the
    // runtime and stay valid; only the array is released.
    unsafe { free(list.cast()) };
    rows
}

/// The protocols a class object CLAIMS — its own `class_addProtocol` list, not
/// its superclass's.
///
/// This is the half of a declared class that no encoding check can see. A
/// `protocols:` list is not decoration: it is what makes `conformsToProtocol:`
/// answer YES, and AppKit asks that question before it will treat an object as
/// a delegate at all. Deleting `NSWindowDelegate` from `vendor/winit`'s
/// delegate left every registered encoding correct, the build green and the
/// driven event log byte-identical — the class simply stopped saying it was a
/// window delegate. Nothing in this crate could report that until this
/// function; now an auditor can ask.
///
/// Names, not [`ProtocolPtr`]s, because the only thing a caller does with the
/// answer is compare it to the list the source claims.
///
/// # Safety
/// `cls` must be a live class object.
#[must_use]
pub unsafe fn class_protocols(cls: ClassPtr) -> Vec<String> {
    if cls.is_null() {
        return Vec::new();
    }
    let mut count: c_uint = 0;
    // SAFETY: the caller pins `cls` as live; `class_copyProtocolList` writes
    // the count through the pointer and returns a malloc'd array this function
    // owns, or null with a zero count.
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
/// # A pool is a LEAK obligation, not a safety one
///
/// This used to read "There must be a pool on this thread's stack" under
/// `# Safety`, and that was a false contract of exactly the shape D2 was about:
/// two SAFE methods in this crate — [`Obj::autorelease`] and
/// [`crate::Retained::autorelease`] — call this function and neither can
/// establish it, because neither can see the caller's stack. A safety
/// precondition that safe code in the same crate violates by construction is
/// not a precondition; it is either a bug in the wrappers or a bug in the
/// contract.
///
/// MEASURED, in a child process with no pool anywhere on the thread: the
/// object leaks and the process runs to a clean exit. Nothing is freed early,
/// nothing dangles, nothing aborts, and a message still lands on the returned
/// pointer afterwards — so the wrappers are right and the contract was wrong,
/// and the requirement is demoted to what it actually is: a leak you own, not
/// undefined behaviour.
///
/// The old text also promised a diagnostic, and that half was measured too and
/// is likewise wrong by default. `libobjc` is SILENT here unless asked:
///
/// ```text
/// $ CHILD=1 ./adversary_w2c                              (no output)
/// $ CHILD=1 OBJC_DEBUG_MISSING_POOLS=YES ./adversary_w2c
/// objc[75911]: MISSING POOLS: (0x16fb43000) Object 0x100f33610 of class
/// __NSCFString autoreleased with no pool in place - just leaking - break on
/// objc_autoreleaseNoPool() to debug
/// ```
///
/// Both exit 0. That matters for anyone hunting one of these leaks: the string
/// to grep for does not appear until the environment variable is set.
/// `tests/adversary_w2c.rs` runs the child both ways and asserts both.
///
/// On the main thread of a Cocoa app the event loop always has a pool, which is
/// why a declared method may autorelease unconditionally; off it,
/// [`autoreleasepool`] is how this crate puts one there.
///
/// # Safety
/// `obj` must be a +1 reference the caller is handing over, or null.
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
///
/// # WHAT `&AutoreleasePool` IS NOT, and the rule that follows
///
/// It is a proof that **A** pool is open on this thread. It is NOT a proof that
/// **THIS** pool is the one the runtime will use. Every `+0` return in
/// Objective-C — `objc_loadWeak`, every framework method not named
/// `new`/`alloc`/`copy` — autoreleases into the INNERMOST pool at the instant
/// of the call, which is a fact about the thread's stack; a `&AutoreleasePool`
/// parameter is a fact about a name the caller chose. Nesting is where they
/// come apart, because inside `autoreleasepool(|inner| …)` the caller can still
/// name `outer`.
///
/// **So a pool reference may GATE an operation and must NEVER appear in the
/// returned lifetime.** `WeakObj::load_borrowed` broke this and was withdrawn;
/// `crates/aterm-objc/src/weak.rs`'s module docs carry the counterexample, the
/// SIGSEGV it produces from 100% safe code, and the measurement showing the
/// form was not even faster. `tests/weak.rs`'s
/// `a_pool_parameter_never_mints_a_lifetime` enforces the rule against this
/// crate's source, `aterm-gui`'s and the winit fork's.
///
/// **AND NO BOUND ON `R` FIXES IT — measured, not reasoned.** The obvious
/// repair is to stop the inner closure returning such a borrow. The escape does
/// not need the return type: with every closure returning `()`, a `Cell`
/// declared in the outer scope carries the borrow out just as well. `R: 'static`
/// would forbid honest returns and still not close it. That is why the answer
/// was to delete the API rather than to constrain this function, and it is
/// worth knowing BEFORE the next capability mints a lifetime from a pool.
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
