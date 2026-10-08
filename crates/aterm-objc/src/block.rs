// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Objective-C block ABI, over `_NSConcreteStackBlock`.
//!
//! AppKit takes blocks for event monitors and completion handlers
//! (`addLocalMonitorForEventsMatchingMask:handler:`,
//! `beginSheetModalForWindow:completionHandler:`,
//! `openApplicationAtURL:configuration:completionHandler:`), so this provides
//! escaping `Fn` blocks of arity 0, 1 and 2.
//!
//! # Layout
//!
//! The first five fields are fixed ABI ([the Block ABI spec][abi]); the tail
//! is the captured state, here the Rust closure:
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
//! [`RcBlock::new1`] and friends build that on the stack and hand it to
//! `_Block_copy`, which `malloc`s a copy, `memmove`s the whole struct, then
//! calls the `copy` helper. The `memmove` is the Rust move, so the copy helper
//! is a no-op and the stack original is `ManuallyDrop`. `dispose` drops the
//! closure when the heap block's last reference goes. `_Block_copy` on a heap
//! block only bumps its refcount, so AppKit may retain the handler freely.
//!
//! [abi]: https://clang.llvm.org/docs/Block-ABI-Apple.html
//!
//! # Alignment
//!
//! `malloc` promises only 16-byte alignment, so each constructor refuses an
//! over-aligned closure at compile time (`S4` in the crate docs):
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
//! A pointer-sized capture is fine:
//!
//! ```
//! let capture = std::rc::Rc::new(7_i32);
//! // SAFETY: the closure takes no arguments and returns `()`, which is the
//! // prototype `new0` builds; it is never invoked here.
//! let block = unsafe { aterm_objc::RcBlock::new0(move || { let _ = &capture; }) };
//! assert!(block.is_some());
//! ```
//!
//! # Arguments and return state their ABI
//!
//! `RcBlock::newN` requires [`crate::Encode`] on each argument and the return
//! type, as [`crate::msg`] does on its return: a block's `invoke` is an ABI
//! boundary like a send. An ObjC `BOOL` out-parameter spelled as a Rust `bool`
//! pointer is refused:
//!
//! ```compile_fail
//! # use aterm_objc::{Id, RcBlock};
//! // `-[NSString enumerateLinesUsingBlock:]` takes `void (^)(NSString *, BOOL *)`.
//! // On the x86_64 compat slice `BOOL` is `signed char`, so materialising the
//! // byte the framework writes as a Rust `bool` is undefined behaviour.
//! let _ = unsafe { RcBlock::new2(|_line: Id, _stop: *mut bool| {}) };
//! ```
//!
//! So is a bare array return, whose x86-64 classification nobody has stated:
//!
//! ```compile_fail
//! # use aterm_objc::RcBlock;
//! let _ = unsafe { RcBlock::new0(|| [0_u8; 24]) };
//! ```
//!
//! The `BOOL *` spelling that compiles is `*mut Bool` (encoded `^B` on arm64,
//! `^c` on x86_64; see [`crate::encode`]):
//!
//! ```
//! # use aterm_objc::{Bool, Id, RcBlock};
//! let block = unsafe { RcBlock::new2(|_line: Id, _stop: *mut Bool| {}) };
//! assert!(block.is_some());
//! ```
//!
//! # A block as an argument: `"@?"`
//!
//! Passing a block to a framework method needs no encoding, so
//! `RcBlock::as_ptr`'s `*mut c_void` suffices. A block crossing a boundary this
//! crate declares (a method registered through `class_addMethod`, or another
//! block's arguments) needs `"@?"`, not `"^v"`: that is [`BlockPtr`].
//!
//! # Not provided
//!
//! * `BLOCK_HAS_SIGNATURE`: blocks carry no type-encoding string, so an API
//!   that introspects one through `NSMethodSignature` would not work. None of
//!   the call sites does.
//! * `"^?"` (a bare C function pointer, e.g.
//!   `-[NSView sortSubviewsUsingFunction:context:]`'s `v32@0:8^?16^v24`):
//!   no declared method takes one.

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

/// A block pointer in a position where its type encoding matters — `"@?"`.
///
/// ```text
/// @encode(void (^)(void))                    @?
/// @encode(id (^)(id))                        @?
/// -[NSString enumerateLinesUsingBlock:]      v24@0:8@?16
/// -[NSView sortSubviewsUsingFunction:context:] v32@0:8^?16^v24
/// ```
///
/// `@?` is a block and `^?` a function pointer; only the first is bound. No
/// declared method takes a block today (`tests/winit_seam.rs` checks), but
/// the alternative, `*mut c_void`, would register `"^v"` and tell every
/// reflective path the argument is an opaque pointer.
///
/// Borrowed, not an owner: [`RcBlock`] (or whatever the framework copied) owns
/// the reference, and this is valid while that lives.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BlockPtr(*mut c_void);

impl BlockPtr {
    /// The raw pointer, for handing to an ObjC API.
    #[inline]
    #[must_use]
    pub const fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Wrap a raw block pointer, for the receiving side of a declared method.
    /// Safe: holding the pointer is harmless, invoking it is not.
    #[inline]
    #[must_use]
    pub const fn from_ptr(ptr: *mut c_void) -> Self {
        Self(ptr)
    }
}

// SAFETY: `@encode` of any block type is `"@?"` (clang, two signatures, and
// Foundation's `v24@0:8@?16` read back in `tests/winit_seam.rs`). `BlockPtr` is
// `#[repr(transparent)]` over exactly the pointer a block is passed as.
unsafe impl crate::encode::Encode for BlockPtr {
    const ENCODING: &'static str = "@?";
}

/// A reference-counted heap block, ready to hand to an Objective-C API.
///
/// Dropping releases aterm's reference; APIs that keep the handler
/// (`addLocalMonitorForEventsMatchingMask:`,
/// `beginSheetModalForWindow:completionHandler:`) copy the block, so dropping
/// the `RcBlock` before it fires is fine.
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

    /// The same pointer typed as [`BlockPtr`] (`"@?"`), for a block that is an
    /// argument of a declared method or another block. `as_ptr`'s `*mut c_void`
    /// would encode as `"^v"`.
    #[inline]
    #[must_use]
    pub fn as_block_ptr(&self) -> BlockPtr {
        BlockPtr(self.ptr.as_ptr())
    }

    /// A second reference to the same block (`_Block_copy` on a heap block is a
    /// retain). Not `Clone`, so the refcount bump is visible at the call site.
    #[must_use]
    pub fn clone_retained(&self) -> Self {
        // SAFETY: `self.ptr` is a live heap block, so `_Block_copy` increments
        // its refcount and returns the same pointer, balanced by the new
        // holder's `Drop`.
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
            /// The argument and return types must be exactly what the framework
            /// calls the block with. The block ABI carries no checkable
            /// signature, so a wrong prototype silently reinterprets registers
            /// (the mirror of [`crate::msg`]'s obligation).
            ///
            /// Every argument and the return type must be [`crate::Encode`],
            /// refusing a Rust `bool` for an ObjC `BOOL` and unstated packed
            /// layouts. The return type must be [`crate::InertZero`]: an
            /// `NSException` raised inside the closure is contained at `invoke`
            /// (reported against `"block invoke"` through [`crate::exception`])
            /// and the block returns the all-zero value. A Rust panic aborts
            /// with a name, since unwinding into framework code is undefined
            /// behaviour.
            #[must_use]
            pub unsafe fn $ctor<$($A,)* R, F>(closure: F) -> Option<Self>
            where
                F: Fn($($A),*) -> R + 'static,
                $($A: $crate::Encode,)*
                R: $crate::Encode + $crate::InertZero,
            {
                /// The stack block: the fixed header, then the closure.
                #[repr(C)]
                struct Blk<F> {
                    header: BlockHeader,
                    closure: F,
                }

                // Same order as `declare_class!`'s trampoline: `catch_unwind`
                // outside (a Rust panic aborts with a name), the Objective-C
                // `@try` inside (an `NSException` is contained and answered
                // with the inert zero). No `Default` bound on `R`: the panic
                // path is `abort_on_unwind`, which returns `!`.
                unsafe extern "C" fn $invoke<$($A,)* R: crate::InertZero, F: Fn($($A),*) -> R>(
                    block: *mut c_void,
                    $($arg: $A),*
                ) -> R {
                    // SAFETY: the runtime passes the block itself as the first
                    // argument, and this `invoke` is only installed in a
                    // `Blk<F>` of exactly this `F`.
                    let closure = unsafe { &(*block.cast::<Blk<F>>()).closure };
                    let guard = ::std::panic::catch_unwind(
                        ::std::panic::AssertUnwindSafe(move || {
                            match crate::exception::contain(
                                "block invoke",
                                move || closure($($arg),*),
                            ) {
                                Ok(v) => v,
                                Err(_) => crate::exception::inert::<R>(),
                            }
                        }),
                    );
                    match guard {
                        Ok(v) => v,
                        Err(_) => crate::abort_on_unwind("block invoke"),
                    }
                }

                /// `_Block_copy` has already `memmove`d the closure, which is
                /// the Rust move; there is nothing left to copy.
                unsafe extern "C" fn copy_helper(_dst: *mut c_void, _src: *const c_void) {}

                /// Guarded like `invoke`, `__tramp` and `__dealloc`, so a
                /// panicking `Drop` in a capture aborts through
                /// [`crate::abort_on_unwind`] with a name rather than Rust's
                /// anonymous non-unwinding-panic message.
                unsafe extern "C" fn dispose_helper<F>(block: *mut c_void) {
                    let blk = block.cast::<Blk<F>>();
                    let guard = ::std::panic::catch_unwind(
                        ::std::panic::AssertUnwindSafe(|| {
                            // SAFETY: `dispose` runs once, on the last release
                            // of a heap block built here, so the closure is live
                            // and owned by it. `&raw mut` avoids a reference to
                            // the partially torn-down block.
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
                // The explicit `'static` forces const promotion: a stack
                // temporary descriptor would dangle, and this makes that a
                // borrow-check error.
                let descriptor: &'static BlockDescriptor = &Desc::<F>::D;

                // `_Block_copy` allocates with `malloc` (16-byte alignment), so
                // a closure needing more would be misaligned in the heap block
                // and every access through `&(*block).closure` would be UB.
                // There is no way to ask for more; refuse at compile time.
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

                // SAFETY: `block` is a well-formed stack block that owns its
                // closure, not dropped (`ManuallyDrop`). One `_Block_copy`
                // `memmove`s it to the heap, making the heap block the sole
                // owner, which is what the no-op copy helper assumes.
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
