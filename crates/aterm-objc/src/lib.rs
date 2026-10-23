// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party Objective-C runtime layer over `libobjc`, `Foundation`, and `CoreFoundation`.
//!
//! Provides typed message sends ([`checked_msg`]), ownership ([`Obj`], [`Retained`]),
//! scoped [`autoreleasepool`]s, runtime class declaration ([`declare_class!`]),
//! blocks ([`RcBlock`], [`BlockPtr`]), pinned weak references ([`WeakSlot`]),
//! cached selectors ([`SelCache`]), method swizzling ([`Swizzle`]),
//! main-thread affinity ([`MainThread`], [`MainThreadBound`], [`run_on_main`]),
//! run-loop wake timers ([`WakeTimer`]), and [`HasNsView`].
//!
//! # Safety conventions
//!
//! * Every send casts [`checked_msg`] to the selector's exact `extern "C-unwind"` prototype
//!   (validated against the runtime in debug builds); `x86_64` indirect returns route
//!   through `objc_msgSend_stret` via [`returns_indirectly`].
//! * `+1` returns (`new*`/`alloc`/`copy*`) land in [`Obj`] or [`Retained`]; declared methods
//!   returning `+0` must call [`autorelease`], and callers must enclose borrows in [`autoreleasepool`].
//! * [`IvarSlot`] tracks initialization; ivar pointers use exposed provenance, and
//!   [`declare_class!`], [`ClassBuilder::add_rust_ivar`] and [`RcBlock`] reject alignments above
//!   `malloc`'s 16-byte guarantee at compile time.
//! * Trampolines wrap `@try` inside `catch_unwind` to contain `NSException`s and abort on Rust panics.

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

pub use block::{BlockFn, BlockPtr, IntoBlock, RcBlock};
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
    protocol_method_extended_types, protocol_method_types, protocol_or_register,
    protocols_registered_by_aterm, returns_indirectly, superclass_of,
};
pub use sel_cache::SelCache;
pub use send::{CheckedFn, Receiver, block_check, block_signatures, checked_msg, prototype_check};
pub use swizzle::{Imp, MethodFn, Swizzle, SwizzleError, SwizzleSite, owning_class};
pub use view::HasNsView;
pub use wake_timer::{WakeArm, WakePlan, WakeTarget, WakeTimer};
pub use weak::{WeakObj, WeakSlot};

/// Uncached selector lookup.
pub use runtime::sel as sel_uncached;
