// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's first-party Objective-C runtime layer.

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

/// The uncached selector lookup.
pub use runtime::sel as sel_uncached;
