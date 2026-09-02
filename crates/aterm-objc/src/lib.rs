// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's first-party Objective-C runtime layer.
//!
//! # Why this crate exists
//!
//! `aterm-gpu/src/metal/ffi.rs` was the first module in the tree to talk to the
//! Objective-C runtime directly, and it established the conventions this crate
//! promotes: `objc_msgSend` declared once and cast per selector, one owning
//! wrapper that is the only caller of `objc_release`, explicit autorelease
//! pools, and selectors resolved from checked C-string literals. It is also
//! `pub(crate)` inside a PRIVATE `mod metal` (`aterm-gpu/src/lib.rs:62`), so
//! nothing outside `aterm-gpu` can use a line of it. The tree's demonstrated
//! answer to that has been to hand-roll again — `CfOwned` is duplicated
//! byte-for-byte between `aterm-gui/src/net_connections/keychain.rs:219` and
//! `aterm-http/src/verifier/apple.rs:193`.
//!
//! This crate is the shared version, plus the four things `metal/ffi.rs`
//! deliberately never needed and the `objc2` + `block2` cluster is being paid
//! for:
//!
//! 1. **[Runtime class creation](declare)** — `objc_allocateClassPair`,
//!    `class_addMethod`, `class_addIvar`, `class_addProtocol`,
//!    `objc_registerClassPair`. Zero uses of any of these existed anywhere in
//!    the workspace before this crate; they are what objc2's `declare_class!`
//!    does, and aterm-gui has seven such sites defining 33 methods.
//! 2. **[The block ABI](block)** — `_NSConcreteStackBlock`, for aterm-gui's
//!    three `RcBlock` sites, plus [`BlockPtr`] for the `"@?"` encoding a block
//!    needs when it crosses a boundary this crate DECLARES.
//! 3. **[Weak references](weak)** — `objc_initWeak`, `objc_loadWeak`,
//!    `objc_storeWeak`, `objc_destroyWeak`, `objc_copyWeak`. Zero uses of any
//!    of these existed in the workspace before W9, for the same reason as
//!    class creation: Metal owns its objects in a TREE and never has a cycle to
//!    break, while AppKit is a graph — `vendor/winit`'s `view.rs:169` holds a
//!    weak reference to the window that (transitively) retains it, and that one
//!    field is what makes `view.rs` the largest remaining port. A weak
//!    reference is the only handle in this crate whose STORAGE LOCATION is
//!    load-bearing rather than its value, which is why it is a capability and
//!    not a wrapper; see [`weak`] for what Rust's `memcpy`-move does to one.
//! 4. **[Cached selectors](sel_cache)** — `metal/ffi.rs` calls
//!    `sel_registerName` on every send, on per-frame paths.
//!
//! # Zero third-party dependencies
//!
//! By construction. The crate exists to REMOVE 255,335 lines of third-party
//! code from the mac-arm cell; adding a dependency to it would be
//! self-defeating. Everything here is std over `libobjc` and `Foundation`,
//! both already loaded in every aterm process.
//!
//! # Safety conventions
//!
//! The first two are `metal/ffi.rs`'s, unchanged. The rest are this crate's,
//! and the last three exist because an adversarial read of the first wave found
//! them missing:
//!
//! * One declaration of `objc_msgSend` cannot serve every selector. There is no
//!   single prototype that is right for all of them, and on AAPCS64 a send
//!   declared variadic passes its arguments somewhere else entirely from the
//!   non-variadic IMP that receives them. Every send therefore casts [`msg`] to
//!   the EXACT prototype of the selector; a wrong prototype corrupts registers
//!   silently, so a typed cast per selector is the only sound form.
//! * `new*`/`alloc`/`copy*` results are +1 and land in [`Obj`] or
//!   [`Retained`], the only places `objc_release` is called.
//! * A method that RETURNS an object and is not named `new*`/`alloc`/`copy*`
//!   owes its caller +0, which means [`autorelease`] — see [`Obj::autorelease`]
//!   and [`Retained::autorelease`]. Returning the +1 leaks one object per call;
//!   returning a borrowed pointer whose owner is about to drop returns a
//!   dangling one. There is no third option and the runtime enforces neither.
//! * Autoreleased returns are BORROWED and never released here; they live until
//!   the enclosing [`autoreleasepool`] scope ends, which is the CALLER's
//!   obligation — including on drop, because a dealloc path autoreleases into
//!   whatever frame the drop happens in.
//! * Every `unsafe` block names the runtime invariant it relies on, and a
//!   SAFETY comment that turns out to be false is a defect of the same rank as
//!   the bug it hides — worse, because it also tells the next reader not to
//!   look. Three were found by an adversarial read of W1 (the pool nesting
//!   claim, and `bool` as `BOOL` twice); re-reading the rest with the same
//!   suspicion found three more (what [`msg`]'s `size_of` assertion actually
//!   proves, when an ivar slot is guaranteed written, and which receivers a
//!   trampoline can be handed); a THIRD pass found four more, all of them about
//!   the word "aligned" or the word "always" — [`IvarSlot`]'s three alignment
//!   preconditions, which the SAFE `ivars()` accessor could not establish, and
//!   the claim that "in this crate a MUTABLE raw void pointer in a method
//!   position always means `id`", which the KVO `context:` argument refutes at
//!   every live site. All ten are corrected IN PLACE, each keeping a note of
//!   what it used to claim, because a silent correction teaches nothing.
//! * A GUARD ARMED AT THE WRONG SPELLING IS NOT A GUARD. Two of the second
//!   pass's fixes were green for a whole pass against shapes that occur nowhere
//!   in the code being ported: `Sel`'s newtype was armed with `*const c_void`
//!   where the SDK, `objc2-foundation` and vendored winit all write
//!   `*mut c_void`, and `HAS_UNALIGNED_FIELDS` was armed with a packed struct
//!   where the rule it states also has to answer for a WRAPPER around one. Both
//!   armings were moved to the live spelling.
//!
//! # Where this improves on `metal/ffi.rs`, and why
//!
//! * **A written initialised flag on the ivars** ([`IvarSlot`]) — `metal/ffi.rs`
//!   had no ivars at all, but the naive port of objc2's design would
//!   `drop_in_place` all-zero bytes if `dealloc` ran before the ivars were
//!   stored. The flag costs one byte per instance and makes that window sound.
//!   It is checked on READ in release builds too, not only in `dealloc`: the
//!   class is registered under a public name, so Objective-C code can mint an
//!   instance this crate never initialised, and the branch is free beside the
//!   `catch_unwind` every message already pays.
//! * **Exposed-provenance ivar access** — reaching an ivar means deriving a
//!   pointer at an offset from the instance base, which the instance's own
//!   (zero-sized) Rust type does not cover. This crate derives it through
//!   [`std::ptr::with_exposed_provenance`] from an address the runtime handed
//!   across FFI, rather than by `offset`-ing a reference out of its own
//!   allocation.
//! * **Panic guards on every trampoline** — unwinding out of an Objective-C
//!   frame is undefined behaviour. `metal/ffi.rs` never defines a method, so it
//!   never had the problem; every method this crate declares aborts instead.
//! * **Cached selectors** — see [`sel_cache`].
//! * **The indirect-return entry point is chosen by the TYPE SYSTEM.**
//!   `metal/ffi.rs` binds `objc_msgSend` alone and argues the case from the
//!   aarch64 ABI, but is gated `target_os = "macos"`, and aterm ships an
//!   `x86_64-apple-darwin` compat slice inside its universal binary. There a
//!   struct return larger than 16 bytes must go through `objc_msgSend_stret`,
//!   whose hidden result pointer occupies `RDI` — the register plain
//!   `objc_msgSend` reads `self` from — so every argument shifts and the send
//!   silently reads the wrong registers. `aterm-gui` already performs five
//!   `NSRect` (32-byte) sends. [`msg`] reads the return type off the prototype
//!   through [`MsgFn`] and picks the entry point in a `const` block, so no call
//!   site is asked to remember the rule. See [`returns_indirectly`].
//! * **`BOOL` is a type, not `bool`** — see [`Bool`], and the measurement in
//!   [`encode`].
//! * **Every pointer-shaped runtime type is its OWN type, so it can carry its
//!   own encoding.** `Id`, `ClassPtr`, `ProtocolPtr` and `Sel` were all aliases
//!   of `*mut c_void` / `*const c_void`, so two `Encode` impls had to serve four
//!   meanings and each impl picked one. `Sel` was newtyped first, which fixed
//!   `const void *`; the `*mut` side stayed broken and mapped an opaque `void *`
//!   — KVO's `context:`, spelled `*mut c_void` in the SDK, in
//!   `objc2-foundation`'s binding and in vendored winit — to `"@"`, an OBJECT.
//!   `Class` had the same problem in the other direction: `@encode(Class)` is
//!   `"#"`. All four are `#[repr(transparent)]` newtypes now and each carries
//!   the letter clang emits, measured on this box.
//! * **An autorelease pool is a SCOPE** — see [`autoreleasepool`]. The RAII
//!   token form let safe code pop out of order, which is a use-after-free.
//!
//! # Named soundness questions, unsettled
//!
//! * **Ivar provenance.** The exposed-provenance derivation above is the best
//!   available form, but Rust has no model in which a zero-sized marker type at
//!   an object's base address *legitimately* addresses bytes beyond itself.
//!   objc2 has the same shape and the same question. Miri cannot adjudicate it
//!   because it cannot run the Objective-C runtime at all.
//! * **Message sends are not `unwind`-safe in the other direction.** An
//!   Objective-C exception raised inside a send unwinds through the Rust frame
//!   as a foreign exception. `metal/ffi.rs` states this becomes an abort;
//!   nothing here changes it, and nothing here catches it.
//! * **`S3` — `dealloc` runs on whatever thread performs the last release.**
//!   [`ClassBuilder::register`] hands the object to the Objective-C runtime,
//!   and nothing in that runtime promises the final `release` comes from the
//!   thread that created the instance: an `-autorelease` inside a framework
//!   worker, a `performSelectorOnMainThread:` that outlives its sender, or a
//!   `NSNotificationCenter` posting from a background queue can all be the last
//!   holder. The generated `-dealloc` then runs the ivar type's Rust
//!   destructor there, with NO `Send` bound anywhere in the path. objc2 0.5 has
//!   exactly this hole — it is why 0.6 introduced `MainThreadOnly` — and every
//!   declared class in this tree is main-thread AppKit state whose ivars are
//!   `Cell`/`RefCell`/[`Retained`], so the hole is real and currently
//!   unexercised. STILL UNCLOSED at the RELEASE end, which is where it was
//!   named: nothing can make a framework release an object on the thread that
//!   made it.
//!
//!   Its BIRTH end is closed, and it had quietly opened. objc2 0.5 gave every
//!   class in this port `mutability::MainThreadOnly`, which made `alloc`
//!   reachable only through a `MainThreadMarker`; this crate's first
//!   [`declare_class!`] dropped the parameter, reasoning that [`Retained`] is
//!   `!Send` so it had one useful value. That reasoning is about where an
//!   instance may TRAVEL. A judge built the counterexample about where one may
//!   be BORN: a declared class registered, instantiated and deallocated on a
//!   `std::thread::spawn`ed thread, no witness, NOT ONE `unsafe` token at the
//!   call site, no diagnostic — the ivar destructor running on a foreign thread
//!   through the front door rather than through a framework's release. W3 was
//!   about to multiply that by five, including `NSView` and `NSWindow`
//!   subclasses. [`MainThread`] is the parameter restored: `alloc_init` and
//!   `alloc_ivars` take one, `new()` asks `+[NSThread isMainThread]` exactly as
//!   objc2's marker does, and the escape hatch is `unsafe`. Class REGISTRATION
//!   is deliberately still ungated — see [`MainThread`] for why.
//! * **`S4` — nothing `malloc` allocates can out-align 16 bytes. CLOSED, by
//!   refusal, on BOTH of its paths.** `_Block_copy` allocates with `malloc`,
//!   which guarantees 16-byte alignment; a closure needing more would land
//!   misaligned in the heap block. There is no ABI knob to ask for more, so
//!   each [`RcBlock`] constructor carries a `const` assertion that rejects such
//!   a closure at compile time rather than producing a misaligned one at run
//!   time.
//!
//!   That was recorded as CLOSED while its exact twin stood open one module
//!   over. `class_createInstance` — what `+alloc` calls — uses the same
//!   allocator, `class_addIvar` can align the ivar's OFFSET but not the
//!   instance BASE, and so a `type Ivars = T` with `align_of::<T>() > 16` lands
//!   misaligned in some fraction of instances: MEASURED through `ivar_getOffset`
//!   on raw `+alloc` addresses, 9/4096 at `align(64)` and 19/4096 at
//!   `align(32)`, with 0/4096 at 16. It was reachable with NOT ONE `unsafe`
//!   token at the call site (`declare_class!{ type Ivars = Align64 }` then
//!   `X::alloc_init(v)`), aborted in debug at `IvarSlot::init`'s `ptr::write`
//!   alignment precondition and again on the `dealloc` path, and in release
//!   compiles the check out and lets LLVM propagate the alignment it was
//!   promised. [`ClassBuilder::add_rust_ivar`] and [`declare_class!`] now carry
//!   the same `const` assertion the block constructors do — which is also what
//!   makes [`IvarSlot`]'s three "aligned" preconditions dischargeable, since
//!   its `get` is reached from the SAFE `ivars()` accessor.
//! * **`x86_64` is proved by CODEGEN, not by execution.** Every claim this
//!   crate makes about `x86_64-apple-darwin` — `@encode(BOOL) == "c"`, the
//!   `objc_msgSend_stret` threshold at 16 bytes, and the SECOND `_stret` rule
//!   that has no threshold at all (a struct with a misaligned field goes
//!   indirect at any size, measured down to three bytes) — was measured by
//!   compiling the equivalent Objective-C with `clang -arch x86_64 -S` and
//!   reading which symbol the call lands on, because the development box cannot
//!   execute that slice. The Rust side's `x86_64` arms are type-checked by a
//!   cross `cargo check --target x86_64-apple-darwin`, never run. The aarch64
//!   half of each pair IS execution-proved, by the tests here.

#![cfg(target_os = "macos")]

pub mod block;
pub mod class_macro;
pub mod declare;
pub mod dispatch;
pub mod encode;
pub mod retained;
pub mod runtime;
pub mod sel_cache;
pub mod send;
pub mod weak;

pub use block::{BlockPtr, RcBlock};
pub use declare::{
    ClassBuilder, ClassMeta, IVAR_NAME, IvarSlot, MainThread, abort_on_unwind, begin,
    send_super_dealloc, super_of,
};
pub use dispatch::run_on_main;
pub use encode::{Bool, CGPoint, CGRect, CGSize, Encode, NSRange, strip_method_offsets};
pub use retained::{ClassType, Retained};
pub use runtime::{
    AutoreleasePool, ClassPtr, Id, IvarPtr, MsgFn, Obj, ObjcSuper, ProtocolPtr, Sel, autorelease,
    autoreleasepool, class, class_methods, class_name, class_of, class_protocols, method_imp,
    method_types, msg, msg_super, ns_error_string, ns_string, ns_string_to_rust, protocol,
    protocol_method_types, returns_indirectly, superclass_of,
};
pub use sel_cache::SelCache;
pub use weak::{Weak, WeakObj, WeakSlot};

/// The uncached selector lookup, under its full name.
///
/// [`sel!`] is what call sites should use; this is exported for the rare case
/// where the name is not known at compile time, and for tests that compare the
/// two forms.
pub use runtime::sel as sel_uncached;
