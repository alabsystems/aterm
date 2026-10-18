// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's first-party Objective-C runtime layer.
//!
//! Typed message sends ([`checked_msg`], one exact prototype per selector,
//! checked against the runtime at every send of a debug build), ownership
//! ([`Obj`], [`Retained`]), scoped autorelease pools, and:
//!
//! 1. **[Runtime class creation](declare)**, driven by [`declare_class!`].
//! 2. **[The block ABI](block)** — stack and heap blocks ([`RcBlock`]), plus
//!    [`BlockPtr`] for the `"@?"` encoding of a block in a declared method.
//! 3. **[Weak references](weak)**. The runtime tracks a weak reference's
//!    storage location, not its value, so it is a pinned capability rather
//!    than a movable wrapper.
//! 4. **[Cached selectors](sel_cache)** — one `sel_registerName` per call site.
//! 5. **[Method swizzling](swizzle)** — `method_setImplementation`, for patching
//!    an existing framework method, which `class_addMethod` cannot do. No
//!    encoding check can see its effect.
//! 6. **[A thread-affinity container](main_thread_bound)** —
//!    [`MainThreadBound<T>`][MainThreadBound], built from the [`MainThread`]
//!    witness and [`run_on_main`].
//! 7. **[The run-loop wake timer](wake_timer)** — the event-loop waker. Its
//!    repeat interval is a year because CoreFoundation walks a late repeating
//!    timer forward one interval at a time with the run-loop lock held.
//! 8. **[CoreFoundation](cf)** — the flat C the windowing backend calls beside
//!    its sends: run-loop sources and observers, retain/release. Each
//!    declaration is held to the SDK headers by clang (`tests/cf_sdk.rs`).
//!    CoreGraphics (display IDs, bounds and modes) is its own crate,
//!    `aterm-cg`, so only the windowing backend's binaries load it.
//! 9. **[A window's content view](view)** — [`HasNsView`], the `NSView` the
//!    windowing crate hands the Metal presenter and the GUI's chrome.
//!
//! The crate has no third-party dependencies: everything is std over `libobjc`,
//! `Foundation` and `CoreFoundation`, all loaded in every aterm process.
//!
//! # Safety conventions
//!
//! * No single `objc_msgSend` prototype is right for every selector, and on
//!   AAPCS64 a variadic declaration passes arguments elsewhere than the
//!   non-variadic IMP reads them. Every send casts [`checked_msg`] to the
//!   exact prototype of the selector; a wrong one corrupts registers silently
//!   in a release build and panics, naming both encodings, in a debug build
//!   ([`send`]). The unchecked [`msg`] is this crate's tests' alone: the
//!   workspace `clippy.toml` refuses it everywhere else.
//! * `new*`/`alloc`/`copy*` results are +1 and land in [`Obj`] or
//!   [`Retained`], the only places `objc_release` is called.
//! * A declared method that returns an object and is not named
//!   `new*`/`alloc`/`copy*` owes its caller +0, i.e. [`autorelease`] (see
//!   [`Obj::autorelease`], [`Retained::autorelease`]). Returning +1 leaks;
//!   returning a borrowed pointer whose owner is about to drop dangles.
//! * Autoreleased returns are borrowed and never released here; they live until
//!   the enclosing [`autoreleasepool`] scope ends, which is the caller's
//!   obligation — including on drop, since dealloc paths autorelease into the
//!   current frame.
//! * Every `unsafe` block names the runtime invariant it relies on.
//! * A test guard is armed with the spelling the ported code uses (e.g.
//!   `*mut c_void`, a wrapper around a packed struct), not a stand-in.
//!
//! # Design points
//!
//! * **Ivars carry a written-initialised flag** ([`IvarSlot`]), so a `dealloc`
//!   before the ivars are stored never drops zeroed bytes. It is checked on
//!   read in release builds too: the class is registered under a public name,
//!   so Objective-C code can mint an instance this crate never initialised.
//! * **Ivar pointers use exposed provenance** — derived through
//!   [`std::ptr::with_exposed_provenance`] from the address the runtime handed
//!   across FFI, not by offsetting a reference to the zero-sized Rust type.
//! * **Every trampoline has a panic guard and exception containment.**
//!   Unwinding out of an Objective-C frame is undefined behaviour, so a Rust
//!   panic aborts with the method's name. An `NSException` raised inside the
//!   method is caught by the `@try` wrapper in [`exception`] after every Rust
//!   `Drop` on the unwound frames has run, reported once, counted, and the
//!   method returns its inert zero. `-dealloc` and methods declared
//!   `@abort_on_exception` abort instead. Order: `catch_unwind` outside,
//!   `@try` inside.
//! * **The indirect-return entry point is chosen by the type system.** On
//!   `x86_64-apple-darwin` a struct return over 16 bytes must use
//!   `objc_msgSend_stret`, whose hidden result pointer occupies `RDI`,
//!   shifting every argument. [`msg`] reads the return type off the prototype
//!   through [`MsgFn`] and picks the entry point in a `const` block. See
//!   [`returns_indirectly`].
//! * **`BOOL` is a type, not `bool`** — see [`Bool`] and [`encode`].
//! * **Every pointer-shaped runtime type is its own `#[repr(transparent)]`
//!   type** (`Id`, `ClassPtr`, `ProtocolPtr`, `Sel`), so each carries the
//!   encoding clang emits (`"@"`, `"#"`, …) and an opaque `void *` such as
//!   KVO's `context:` is not mistaken for an object.
//! * **An autorelease pool is a scope** — see [`autoreleasepool`]; an RAII
//!   token would let safe code pop out of order.
//!
//! # Open soundness questions
//!
//! * **Ivar provenance.** Rust has no model in which a zero-sized marker at an
//!   object's base address legitimately addresses bytes beyond itself. Miri
//!   cannot run the Objective-C runtime.
//! * **Sends are unwind-capable, and must be.** An Objective-C exception raised
//!   inside a send unwinds through the Rust frame as a foreign exception, so
//!   this crate's entry points are `extern "C-unwind"` and [`MsgFn`] refuses
//!   `"C"` (see [`exception`]). `aterm-gpu`'s private `metal/ffi.rs` declares
//!   its sends `"C"`, so a Metal raise skips that frame's destructors on its
//!   way to the containment.
//! * **`S3` — `dealloc` runs on whatever thread performs the last release.**
//!   The generated `-dealloc` runs the ivars' Rust destructor there with no
//!   `Send` bound in the path, and every declared class holds main-thread
//!   state (`Cell`/`RefCell`/[`Retained`]). Nothing can make a framework
//!   release an object on its creating thread, so this stays open in general.
//!   A value held in a [`MainThreadBound`] is released on the main thread: its
//!   `Drop` reschedules through [`run_on_main`]. The birth end is closed:
//!   `alloc_init` and `alloc_ivars` take a [`MainThread`] witness, so a
//!   declared class cannot be instantiated off the main thread without
//!   `unsafe`. Class registration is deliberately ungated — see [`MainThread`].
//! * **`S4` — `malloc` cannot out-align 16 bytes. Closed by refusal.**
//!   `_Block_copy` and `class_createInstance` both allocate with `malloc`, and
//!   `class_addIvar` aligns an ivar's offset but not the instance base. Each
//!   [`RcBlock`] constructor, [`ClassBuilder::add_rust_ivar`] and
//!   [`declare_class!`] carry a `const` assertion rejecting alignment above
//!   16, which is also what makes [`IvarSlot`]'s alignment preconditions
//!   dischargeable from the safe `ivars()` accessor.
//! * **`x86_64` is checked by codegen, not execution.** `@encode(BOOL) ==
//!   "c"`, the 16-byte `objc_msgSend_stret` threshold, and the second `_stret`
//!   rule (a struct with a misaligned field goes indirect at any size) come
//!   from reading `clang -arch x86_64 -S` output; the Rust `x86_64` arms are
//!   only type-checked by a cross `check`. The aarch64 half of each is
//!   execution-tested here.

#![cfg(target_os = "macos")]
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

pub(crate) mod block;
pub mod cf;
pub(crate) mod class_macro;
pub(crate) mod declare;
pub(crate) mod dispatch;
pub(crate) mod encode;
pub mod exception;
pub(crate) mod main_thread_bound;
pub(crate) mod retained;
pub(crate) mod runtime;
pub(crate) mod sel_cache;
pub mod send;
pub mod swizzle;
pub mod view;
pub mod wake_timer;
pub(crate) mod weak;

pub use block::{BlockPtr, RcBlock};
pub use declare::{
    ClassBuilder, ClassMeta, IvarSlot, MainThread, abort_on_unwind, begin, send_super_dealloc,
    super_of,
};
pub use dispatch::{ContainedInRunOnMain, run_on_main};
pub use encode::{Bool, CGPoint, CGRect, CGSize, Encode, NSRange, strip_method_offsets};
pub use exception::{Contained, ContainedException, InertZero, contain, contained_count, objc_try};
pub use main_thread_bound::MainThreadBound;
pub use retained::{ClassType, Retained};
pub use runtime::{
    AutoreleasePool, ClassPtr, Id, IvarPtr, MsgFn, Obj, ObjcSuper, ProtocolPtr, Sel, autorelease,
    autoreleasepool, class, class_methods, class_name, class_of, class_protocols, method_imp,
    method_types, msg, msg_super, ns_error_string, ns_string, ns_string_to_rust, protocol,
    protocol_method_types, protocol_or_register, protocols_registered_by_aterm, returns_indirectly,
    superclass_of,
};
pub use sel_cache::SelCache;
pub use send::{CheckedFn, Receiver, checked_msg};
pub use swizzle::{Imp, MethodFn, Swizzle, SwizzleError, SwizzleSite, owning_class};
pub use view::HasNsView;
pub use wake_timer::{WakeArm, WakePlan, WakeTarget, WakeTimer};
pub use weak::{WeakObj, WeakSlot};

/// The uncached selector lookup. Call sites use [`sel!`]; this serves names not
/// known at compile time and tests comparing the two forms.
pub use runtime::sel as sel_uncached;
