// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Objective-C block ABI, over `_NSConcreteStackBlock`.
//!
//! The second capability `metal/ffi.rs` never needed: Metal's offscreen surface
//! is entirely synchronous, so nothing in it takes a completion handler. AppKit
//! is not, and aterm-gui has THREE block sites (not five — see the crate docs):
//!
//! | site | block signature | why |
//! |---|---|---|
//! | `alert_keys.rs:228` | `(NSEvent*) -> NSEvent*` | `addLocalMonitorForEventsMatchingMask:handler:` |
//! | `lib.rs:16069` | `(NSInteger) -> void` | `beginSheetModalForWindow:completionHandler:` |
//! | `app_launch_successor.rs:524` | `(NSRunningApplication*, NSError*) -> void` | `openApplicationAtURL:configuration:completionHandler:` |
//!
//! So the requirement is: escaping `Fn` blocks of arity 0, 1 and 2, with a
//! pointer-or-void return. That is what this module provides, and nothing more.
//!
//! # The layout, and what the runtime does with it
//!
//! A block is a struct whose first five fields are fixed ABI ([the Block ABI
//! spec][abi]) and whose tail is the captured state — here, the Rust closure:
//!
//! ```text
//! isa         -> &_NSConcreteStackBlock   "this is a stack block"
//! flags       -> BLOCK_HAS_COPY_DISPOSE   "the descriptor has helpers"
//! reserved    -> 0
//! invoke      -> extern "C" fn(block, args...) -> R
//! descriptor  -> { reserved, size, copy, dispose }
//! closure     -> the Rust Fn, by value
//! ```
//!
//! [`RcBlock::new1`] and friends build that on the stack and immediately hand it
//! to `_Block_copy`, which `malloc`s a copy, `memmove`s the whole struct
//! (closure included) and then calls the `copy` helper. Because the `memmove`
//! has ALREADY performed the Rust move, the copy helper is a no-op and the
//! stack original is `ManuallyDrop`ped — dropping it would run the closure's
//! destructor on state the heap block now owns. `dispose` drops the closure
//! when the heap block's last reference goes.
//!
//! A second `_Block_copy` on a HEAP block only bumps its refcount and does NOT
//! re-run the copy helper, which is what makes it correct for AppKit to retain
//! the handler for as long as it likes.
//!
//! [abi]: https://clang.llvm.org/docs/Block-ABI-Apple.html
//!
//! # Alignment: closed, not assumed
//!
//! `_Block_copy` `malloc`s, and `malloc` promises 16 bytes of alignment and no
//! more. A closure needing more would be misaligned in the heap block with no
//! diagnostic anywhere. There is no ABI knob for it, so each constructor
//! carries a `const` assertion that refuses such a closure at compile time —
//! see `S4` in the crate's soundness list. An over-aligned capture is a build
//! error naming the reason:
//!
//! ```compile_fail
//! #[repr(align(32))]
//! struct Overaligned([u8; 32]);
//! let capture = Overaligned([7; 32]);
//! // SAFETY: the block is never invoked; the `const` assertion inside the
//! // constructor rejects this before any of it is reachable.
//! let _ = unsafe { aterm_objc::RcBlock::new0(move || { let _ = &capture; }) };
//! ```
//!
//! A pointer-sized capture — which is what all three real sites hold — is
//! fine:
//!
//! ```
//! let capture = std::rc::Rc::new(7_i32);
//! // SAFETY: the closure takes no arguments and returns `()`, which is the
//! // prototype `new0` builds; it is never invoked here.
//! let block = unsafe { aterm_objc::RcBlock::new0(move || { let _ = &capture; }) };
//! assert!(block.is_some());
//! ```
//!
//! # Every argument and the return state their ABI
//!
//! `RcBlock::newN` carries an [`crate::Encode`] bound on each argument and on
//! the return type, for the reason [`crate::msg`] carries one on its return
//! type: `Encode` is the single place a type states the C ABI it crosses a
//! boundary with, and a block's `invoke` is a boundary exactly like a send. It
//! was the LAST place in the crate exempt from that rule.
//!
//! It costs the tree nothing — all three real sites already satisfy it — and it
//! buys the same two refusals `msg` gets. An ObjC `BOOL` out-parameter spelled
//! as a Rust `bool` pointer does not compile, which is `Bool`'s rule reaching
//! through a pointer for the first time (`tests/blocks.rs` had exactly this,
//! written `*mut bool`, until the bound refused it):
//!
//! ```compile_fail
//! # use aterm_objc::{Id, RcBlock};
//! // `-[NSString enumerateLinesUsingBlock:]` takes `void (^)(NSString *, BOOL *)`.
//! // On the x86_64 compat slice `BOOL` is `signed char`, so materialising the
//! // byte the framework writes as a Rust `bool` is undefined behaviour.
//! let _ = unsafe { RcBlock::new2(|_line: Id, _stop: *mut bool| {}) };
//! ```
//!
//! Nor does a bare array return, whose System V x86-64 classification nobody
//! has stated:
//!
//! ```compile_fail
//! # use aterm_objc::RcBlock;
//! let _ = unsafe { RcBlock::new0(|| [0_u8; 24]) };
//! ```
//!
//! The `BOOL *` spelling that DOES compile is `*mut Bool`, and its encoding is
//! one of this crate's stranger measurements — `^B` on arm64 but `*` on
//! x86_64, because `BOOL` is `signed char` there and so `BOOL *` IS `char *`:
//!
//! ```
//! # use aterm_objc::{Bool, Id, RcBlock};
//! let block = unsafe { RcBlock::new2(|_line: Id, _stop: *mut Bool| {}) };
//! assert!(block.is_some());
//! ```
//!
//! # Named gap: no `BLOCK_HAS_SIGNATURE`
//!
//! These blocks carry no type-encoding string. An API that introspects a block
//! through `NSMethodSignature` would need one; none of the three sites does,
//! and `block2` — the crate this replaces — does not emit one either, so the
//! behaviour is byte-for-byte what the tree ships today.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::os::raw::{c_int, c_ulong};
use std::ptr::NonNull;

unsafe extern "C" {
    /// The `isa` every stack block points at. Lives in libSystem's libclosure,
    /// which every Rust macOS binary already links.
    static _NSConcreteStackBlock: [*const c_void; 32];
    /// Move a stack block to the heap, or retain a heap block. Returns nil on
    /// allocation failure.
    fn _Block_copy(block: *const c_void) -> *mut c_void;
    /// Release a heap block; runs `dispose` and frees on the last reference.
    fn _Block_release(block: *const c_void);
}

/// The descriptor has `copy` and `dispose` helpers.
const BLOCK_HAS_COPY_DISPOSE: c_int = 1 << 25;

/// The fixed ABI header at the front of every block.
#[repr(C)]
struct BlockHeader {
    isa: *const c_void,
    flags: c_int,
    reserved: c_int,
    invoke: *const c_void,
    descriptor: *const BlockDescriptor,
}

/// `Block_descriptor_2` — the form implied by `BLOCK_HAS_COPY_DISPOSE`.
#[repr(C)]
#[derive(Clone, Copy)]
struct BlockDescriptor {
    reserved: c_ulong,
    size: c_ulong,
    copy: Option<unsafe extern "C" fn(dst: *mut c_void, src: *const c_void)>,
    dispose: Option<unsafe extern "C" fn(block: *mut c_void)>,
}

/// A reference-counted heap block, ready to hand to an Objective-C API.
///
/// Dropping releases aterm's reference; the block itself survives for as long
/// as the framework holds its own (`addLocalMonitorForEventsMatchingMask:` and
/// `beginSheetModalForWindow:completionHandler:` both copy the block, so the
/// three real sites are correct even though two of them drop the `RcBlock`
/// before the handler ever fires).
pub struct RcBlock {
    ptr: NonNull<c_void>,
}

impl RcBlock {
    /// The block pointer, borrowed. Pass this where an ObjC API wants a block.
    #[inline]
    #[must_use]
    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr.as_ptr()
    }

    /// A second reference to the SAME block (`_Block_copy` on a heap block is a
    /// retain). Deliberately not `Clone`, for [`crate::Obj`]'s reason: a
    /// refcount bump should be visible at the call site.
    #[must_use]
    pub fn clone_retained(&self) -> Self {
        // SAFETY: `self.ptr` is a live heap block, so `_Block_copy` increments
        // its refcount and returns the same pointer; the extra reference is
        // balanced by this new holder's `Drop`.
        let copied = unsafe { _Block_copy(self.ptr.as_ptr()) };
        Self {
            ptr: NonNull::new(copied).expect("retaining a live heap block"),
        }
    }
}

impl Drop for RcBlock {
    fn drop(&mut self) {
        // SAFETY: this holder owns exactly one reference to a live heap block,
        // released exactly once here.
        unsafe { _Block_release(self.ptr.as_ptr()) }
    }
}

impl std::fmt::Debug for RcBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RcBlock({:p})", self.ptr.as_ptr())
    }
}

/// Generate one `RcBlock::newN` constructor per arity.
macro_rules! block_ctor {
    (
        $(#[$m:meta])*
        $ctor:ident, $invoke:ident $(, $arg:ident : $A:ident)*
    ) => {
        impl RcBlock {
            $(#[$m])*
            ///
            /// # Safety
            ///
            /// The argument and return types must be EXACTLY what the framework
            /// calls the block with. This is [`crate::msg`]'s obligation in the
            /// other direction: the block ABI carries no checkable signature, so
            /// a wrong prototype silently reinterprets registers.
            ///
            /// The closure must not unwind. It is wrapped in a panic guard that
            /// aborts, because unwinding out of a block into framework code is
            /// undefined behaviour.
            ///
            /// # The `Encode` bounds
            ///
            /// Every argument and the return type must implement
            /// [`crate::Encode`], for the reason [`crate::msg`] carries the same
            /// bound on ITS return type: `Encode` is the one place a type states
            /// the C ABI it crosses a boundary with, and a block's `invoke` is a
            /// boundary. It cost nothing to add — all three real sites in the
            /// tree already satisfy it (`(Id) -> Id`, `(i64) -> ()`,
            /// `(Id, Id) -> ()`) — and it buys the same two refusals `msg` gets:
            /// a Rust `bool` standing in for an ObjC `BOOL` (which has no
            /// `Encode` impl, deliberately) and a packed struct whose
            /// System V x86-64 classification nobody has stated. This is the
            /// crate's rule applied at the LAST place that was exempt from it.
            #[must_use]
            pub unsafe fn $ctor<$($A,)* R, F>(closure: F) -> Option<Self>
            where
                F: Fn($($A),*) -> R + 'static,
                $($A: $crate::Encode,)*
                R: $crate::Encode,
            {
                /// The stack block: the fixed header, then the closure.
                #[repr(C)]
                struct Blk<F> {
                    header: BlockHeader,
                    closure: F,
                }

                // No bound on `R` beyond being the closure's return type. It
                // used to be `R: Default`, left over from a version that
                // returned `R::default()` on a caught panic; the guard now
                // lands on `abort_on_unwind`, which is `!` and coerces to any
                // `R`, so the bound bought nothing and cost every block whose
                // return type has no `Default` — an `Option<T>` for a `T` that
                // is not `Default`, a `Retained<T>`, a bare `Id` newtype.
                unsafe extern "C" fn $invoke<$($A,)* R, F: Fn($($A),*) -> R>(
                    block: *mut c_void,
                    $($arg: $A),*
                ) -> R {
                    // SAFETY: the runtime always passes the block itself as the
                    // first argument, and this `invoke` is only ever installed
                    // in a `Blk<F>` of exactly this `F`.
                    let closure = unsafe { &(*block.cast::<Blk<F>>()).closure };
                    let guard = ::std::panic::catch_unwind(
                        ::std::panic::AssertUnwindSafe(move || closure($($arg),*)),
                    );
                    match guard {
                        Ok(v) => v,
                        Err(_) => crate::abort_on_unwind("block invoke"),
                    }
                }

                /// `_Block_copy` has already `memmove`d the closure, which IS
                /// the Rust move; there is nothing left to copy.
                unsafe extern "C" fn copy_helper(_dst: *mut c_void, _src: *const c_void) {}

                /// The LAST unguarded `extern "C"` callback in the crate, and
                /// the reason it needed one is subtle: a panicking `Drop` in a
                /// captured value does abort here either way, because Rust's
                /// own `extern "C"` shim turns an escaping unwind into
                /// `thread caused non-unwinding panic. aborting.` — defined
                /// behaviour, not UB. But that message names neither the crate
                /// nor the callback, and `invoke`, `__tramp` and `__dealloc`
                /// all abort through [`crate::abort_on_unwind`] with a name.
                /// A convention with one silent exception is not a convention.
                unsafe extern "C" fn dispose_helper<F>(block: *mut c_void) {
                    let blk = block.cast::<Blk<F>>();
                    let guard = ::std::panic::catch_unwind(
                        ::std::panic::AssertUnwindSafe(|| {
                            // SAFETY: `dispose` runs exactly once, on the last
                            // release of a heap block this module built, so the
                            // closure is live and owned by that block.
                            // `&raw mut` avoids forming a reference to the
                            // partially-torn-down block.
                            unsafe { ::std::ptr::drop_in_place(&raw mut (*blk).closure) };
                        }),
                    );
                    if guard.is_err() {
                        crate::abort_on_unwind("block dispose");
                    }
                }

                struct Desc<F>(PhantomData<F>);
                impl<F> Desc<F> {
                    const D: BlockDescriptor = BlockDescriptor {
                        reserved: 0,
                        size: size_of::<Blk<F>>() as c_ulong,
                        copy: Some(copy_helper),
                        dispose: Some(dispose_helper::<F>),
                    };
                }
                // The explicit `'static` is load-bearing: it makes const
                // promotion a COMPILE-TIME requirement rather than a hope. If
                // the descriptor were a stack temporary the block would carry a
                // dangling pointer the moment this function returned, and this
                // annotation turns that into a borrow-check error instead.
                let descriptor: &'static BlockDescriptor = &Desc::<F>::D;

                // `_Block_copy` allocates with `malloc`, which guarantees
                // 16-byte alignment and nothing more. A closure whose capture
                // set needs more than that (`#[repr(align(32))]`, an AVX
                // vector, a hand-aligned buffer) would land MISALIGNED in the
                // heap block and every field access through `&(*block).closure`
                // would be undefined behaviour. There is no way to ask
                // `_Block_copy` for more alignment, so the honest fix is to
                // refuse the closure at compile time; every real block site in
                // the tree captures pointers and `Retained` handles, which are
                // 8-aligned.
                const {
                    assert!(
                        align_of::<Blk<F>>() <= 16,
                        "aterm-objc: this block's captures need more than the \
                         16-byte alignment `_Block_copy`'s `malloc` guarantees"
                    )
                };

                let block = ManuallyDrop::new(Blk {
                    header: BlockHeader {
                        isa: (&raw const _NSConcreteStackBlock).cast(),
                        flags: BLOCK_HAS_COPY_DISPOSE,
                        reserved: 0,
                        invoke: $invoke::<$($A,)* R, F> as *const c_void,
                        descriptor,
                    },
                    closure,
                });

                // SAFETY: `block` is a well-formed stack block whose closure is
                // owned by it and NOT dropped (`ManuallyDrop`). `_Block_copy`
                // is called on it exactly once, which is the precondition for a
                // no-op copy helper: it `memmove`s the whole struct to the heap
                // and the heap block becomes the sole owner of the closure.
                let heap = unsafe { _Block_copy((&raw const *block).cast()) };
                NonNull::new(heap).map(|ptr| Self { ptr })
            }
        }
    };
}

block_ctor! {
    /// A no-argument block, e.g. a `dispatch_block_t`.
    new0, invoke0
}
block_ctor! {
    /// A one-argument block — `alert_keys.rs`'s event monitor and `lib.rs`'s
    /// sheet completion handler.
    new1, invoke1, a0: A0
}
block_ctor! {
    /// A two-argument block — `app_launch_successor.rs`'s LaunchServices
    /// completion handler.
    new2, invoke2, a0: A0, a1: A1
}
