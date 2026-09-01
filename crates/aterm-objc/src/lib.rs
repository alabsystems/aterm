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
//! This crate is the shared version, plus the three things `metal/ffi.rs`
//! deliberately never needed and the `objc2` + `block2` cluster is being paid
//! for:
//!
//! 1. **[Runtime class creation](declare)** — `objc_allocateClassPair`,
//!    `class_addMethod`, `class_addIvar`, `class_addProtocol`,
//!    `objc_registerClassPair`. Zero uses of any of these existed anywhere in
//!    the workspace before this crate; they are what objc2's `declare_class!`
//!    does, and aterm-gui has seven such sites defining 33 methods.
//! 2. **[The block ABI](block)** — `_NSConcreteStackBlock`, for aterm-gui's
//!    three `RcBlock` sites.
//! 3. **[Cached selectors](sel_cache)** — `metal/ffi.rs` calls
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
//!   trampoline can be handed). All six are corrected IN PLACE, each keeping a
//!   note of what it used to claim, because a silent correction teaches
//!   nothing.
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
//!   unexercised. UNCLOSED. Closing it means deciding what aterm's own
//!   main-thread proof is, which is the same open question the ported site
//!   names as W2's.
//! * **`S4` — block captures cannot out-align `malloc`. CLOSED, by refusal.**
//!   `_Block_copy` allocates with `malloc`, which guarantees 16-byte alignment;
//!   a closure needing more would land misaligned in the heap block. There is
//!   no ABI knob to ask for more, so each [`RcBlock`] constructor carries a
//!   `const` assertion that rejects such a closure at compile time rather than
//!   producing a misaligned one at run time.
//! * **`x86_64` is proved by CODEGEN, not by execution.** Every claim this
//!   crate makes about `x86_64-apple-darwin` — `@encode(BOOL) == "c"`, the
//!   `objc_msgSend_stret` threshold at 16 bytes — was measured by compiling the
//!   equivalent Objective-C with `clang -arch x86_64 -S` and reading the
//!   emitted assembly, because the development box cannot execute that slice.
//!   The aarch64 half of each pair IS execution-proved, by the tests here.

#![cfg(target_os = "macos")]

pub mod block;
pub mod class_macro;
pub mod declare;
pub mod encode;
pub mod retained;
pub mod runtime;
pub mod sel_cache;

pub use block::RcBlock;
pub use declare::{
    ClassBuilder, ClassMeta, IVAR_NAME, IvarSlot, abort_on_unwind, begin, send_super_dealloc,
    super_of,
};
pub use encode::{Bool, CGPoint, CGRect, CGSize, Encode};
pub use retained::{ClassType, Retained};
pub use runtime::{
    AutoreleasePool, ClassPtr, Id, IvarPtr, MsgFn, Obj, ObjcSuper, ProtocolPtr, Sel, autorelease,
    autoreleasepool, class, class_name, class_of, msg, msg_super, ns_error_string, ns_string,
    ns_string_to_rust, protocol, returns_indirectly, superclass_of,
};
pub use sel_cache::SelCache;

/// The uncached selector lookup, under its full name.
///
/// [`sel!`] is what call sites should use; this is exported for the rare case
/// where the name is not known at compile time, and for tests that compare the
/// two forms.
pub use runtime::sel as sel_uncached;
