// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Objective-C block ABI, over `_NSConcreteStackBlock`.
//!
//! Escaping `Fn` blocks of arity 0, 1 and 2, for AppKit's event monitors and
//! completion handlers.
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
//! A block's `invoke` is an ABI boundary like a send, so `RcBlock::newN`
//! requires [`crate::Encode`] on each argument and the return type. An ObjC
//! `BOOL` out-parameter spelled as a Rust `bool` pointer is refused:
//!
//! ```compile_fail
//! # use aterm_objc::{Id, RcBlock};
//! // `-[NSString enumerateLinesUsingBlock:]` takes `void (^)(NSString *, BOOL *)`.
//! // On the x86_64 compat slice `BOOL` is `signed char`, so materialising the
//! // byte the framework writes as a Rust `bool` is undefined behaviour.
//! let _ = unsafe { RcBlock::new2(|_line: Id, _stop: *mut bool| {}) };
//! ```
//!
//! So is a bare array return, whose x86-64 classification is unstated:
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
//! block's arguments) needs `"@?"`, not `"^v"`: that is [`BlockPtr`]. A block a
//! framework hands a declared method (a delegate's completion handler) is
//! called through its header's `invoke` slot ([`BlockPtr::call0`],
//! [`BlockPtr::call1`]).
//!
//! # Not provided
//!
//! * `BLOCK_HAS_SIGNATURE`: blocks carry no type-encoding string, so an API
//!   that introspects one through `NSMethodSignature` would not work.
//! * `"^?"`, a bare C function pointer: no declared method takes one.

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
/// declared method takes a block today, but the alternative, `*mut c_void`,
/// would register `"^v"` and tell every reflective path the argument is an
/// opaque pointer.
///
/// Borrowed: [`RcBlock`] (or whatever the framework copied) owns the
/// reference, and this is valid while that lives.
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

    /// Call a `void (^)(void)` block a framework handed a declared method — a
    /// delegate's completion handler — through the `invoke` slot of its fixed
    /// ABI header, as clang's call site does. A nil block is not called.
    ///
    /// # Safety
    ///
    /// `self` must be nil or a live block whose prototype is exactly
    /// `void (^)(void)`. The block ABI carries no checkable signature.
    pub unsafe fn call0(self) {
        // SAFETY: forwarded; the prototype is the caller's.
        unsafe {
            if let Some(invoke) = self.invoke::<unsafe extern "C-unwind" fn(*mut c_void)>() {
                invoke(self.0);
            }
        }
    }

    /// Call a `void (^)(A)` block a framework handed a declared method (a
    /// completion handler taking one scalar, e.g. an options bitmask). A nil
    /// block is not called.
    ///
    /// # Safety
    ///
    /// `self` must be nil or a live block whose prototype is exactly
    /// `void (^)(A)`.
    pub unsafe fn call1<A: crate::Encode>(self, a: A) {
        // SAFETY: forwarded; the prototype is the caller's.
        unsafe {
            if let Some(invoke) = self.invoke::<unsafe extern "C-unwind" fn(*mut c_void, A)>() {
                invoke(self.0, a);
            }
        }
    }

    /// The block's `invoke` slot as `F`, or `None` for a nil block. `"C-unwind"`
    /// for the reason every send is: a raise inside the framework's block
    /// unwinds through this frame to the caller's containment.
    ///
    /// # Safety
    ///
    /// `self` must be nil or a live block, and `F` the exact function type of
    /// its `invoke`.
    unsafe fn invoke<F: Copy>(self) -> Option<F> {
        if self.0.is_null() {
            return None;
        }
        const {
            assert!(size_of::<F>() == size_of::<*const c_void>());
        };
        // SAFETY: every block begins with the fixed ABI header ([`BlockHeader`],
        // the layout `_Block_copy` and clang agree on); `invoke` is its fourth
        // field and is never null in a live block. `F` is a function pointer
        // of the same width (asserted above), which the caller names exactly.
        unsafe {
            let invoke = (*self.0.cast::<BlockHeader>()).invoke;
            (!invoke.is_null()).then(|| std::mem::transmute_copy::<*const c_void, F>(&invoke))
        }
    }
}

// SAFETY: `@encode` of any block type is `"@?"` (clang, two signatures, and
// Foundation's `v24@0:8@?16` read back in `tests/declared_abi.rs`). `BlockPtr` is
// `#[repr(transparent)]` over exactly the pointer a block is passed as.
unsafe impl crate::encode::Encode for BlockPtr {
    const ENCODING: &'static str = "@?";
}

/// A reference-counted heap block, ready to hand to an Objective-C API.
///
/// Dropping releases aterm's reference; an API that keeps the handler copies
/// the block, so dropping the `RcBlock` before it fires is fine.
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
            /// calls the block with: the block ABI carries no checkable
            /// signature, so a wrong prototype silently reinterprets registers.
            ///
            /// Arguments and return are [`crate::Encode`] (refusing a Rust
            /// `bool` for an ObjC `BOOL`); the return is [`crate::InertZero`],
            /// so an `NSException` raised in the closure is contained at
            /// `invoke` (reported as `"block invoke"`) and the block returns
            /// zero. A Rust panic aborts with that name.
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

                // As in `declare_class!`'s trampoline: `catch_unwind` outside,
                // the `@try` inside. No `Default` bound on `R`: the panic path
                // is `abort_on_unwind`, which returns `!`.
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

                /// Guarded like `invoke`, so a panicking `Drop` in a capture
                /// aborts with a name rather than Rust's anonymous message.
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

                // `_Block_copy`'s `malloc` aligns to 16 bytes; a closure needing
                // more would be misaligned in the heap block (`S4`).
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
    /// A one-argument block, e.g. an event monitor or a sheet completion handler.
    new1, invoke1, a0: A0
}
block_ctor! {
    /// A two-argument block, e.g. a LaunchServices completion handler.
    new2, invoke2, a0: A0, a1: A1
}

/// A BLOCK'S C PROTOTYPE, NAMED: the `fn` type its `invoke` takes after the
/// block itself, so `fn(Id)` is `void (^)(id)` and `fn(Bool, Id)` is
/// `void (^)(BOOL, NSError *)`.
///
/// The block ABI carries no signature, so a wrong prototype is caught only
/// where something else records one: a protocol's extended encoding
/// (`@?<v@?@"NSError">`). A call site that names its block's prototype as an
/// alias builds the block with [`RcBlock::of`], or calls a framework's with
/// [`BlockPtr::call`], both held to the alias by type, and a test holds the
/// alias to that record ([`crate::send::block_check`]).
///
/// Implemented for `fn() -> R`, `fn(A0) -> R` and `fn(A0, A1) -> R`, the
/// arities [`RcBlock`] builds.
pub trait BlockFn: Copy {
    /// The arguments, as a tuple.
    type Args;
    /// The return type.
    type Ret;

    /// The block's signature as an extended encoding spells it, offsets
    /// aside: the return, `@?` for the block itself, then each argument
    /// (`v@?@` for `fn(Id)`).
    fn types() -> String;

    /// Call `block`'s `invoke` with `args`.
    ///
    /// # Safety
    /// `block` must be a live, non-nil block whose prototype is exactly this.
    #[doc(hidden)]
    unsafe fn call_block(block: BlockPtr, args: Self::Args) -> Self::Ret;
}

/// A closure [`RcBlock::of`] makes a block of prototype `P` from: an `Fn`
/// taking exactly `P`'s arguments and returning its return, so a closure
/// whose parameters disagree with the alias does not compile.
pub trait IntoBlock<P: BlockFn> {
    /// The heap block.
    ///
    /// # Safety
    /// As [`RcBlock::new1`]: `P` must be exactly what the framework calls the
    /// block with.
    #[doc(hidden)]
    unsafe fn into_block(self) -> Option<RcBlock>;
}

/// One [`BlockFn`] and [`IntoBlock`] pair per arity.
macro_rules! block_fn {
    ($ctor:ident $(, $a:ident : $A:ident)*) => {
        impl<R: crate::Encode, $($A: crate::Encode),*> BlockFn for fn($($A),*) -> R {
            type Args = ($($A,)*);
            type Ret = R;

            fn types() -> String {
                let mut types = String::from(R::ENCODING);
                types.push_str("@?");
                $(types.push_str($A::ENCODING);)*
                types
            }

            unsafe fn call_block(block: BlockPtr, ($($a,)*): Self::Args) -> R {
                // SAFETY: the caller pins `block` as live, non-nil and of
                // exactly this prototype, so `invoke` is this function type.
                unsafe {
                    let invoke = block
                        .invoke::<unsafe extern "C-unwind" fn(*mut c_void $(, $A)*) -> R>()
                        .expect("BlockFn::call_block on a nil block");
                    invoke(block.0 $(, $a)*)
                }
            }
        }

        impl<R, $($A,)* F> IntoBlock<fn($($A),*) -> R> for F
        where
            F: Fn($($A),*) -> R + 'static,
            R: crate::Encode + crate::InertZero,
            $($A: crate::Encode,)*
        {
            unsafe fn into_block(self) -> Option<RcBlock> {
                // SAFETY: forwarded; the prototype is the caller's.
                unsafe { RcBlock::$ctor(self) }
            }
        }
    };
}
block_fn!(new0);
block_fn!(new1, a0: A0);
block_fn!(new2, a0: A0, a1: A1);

impl RcBlock {
    /// A block of prototype `P` (a [`BlockFn`] alias) from `closure`, which
    /// must take exactly `P`'s arguments: the constructor that keeps a call
    /// site's block and the alias a test checks the same type.
    ///
    /// ```
    /// # use aterm_objc::{Id, RcBlock};
    /// /// `void (^)(NSError *)`.
    /// type Done = fn(Id);
    /// // SAFETY: never invoked.
    /// let block = unsafe { RcBlock::of::<Done, _>(|_error: Id| {}) };
    /// assert!(block.is_some());
    /// ```
    ///
    /// ```compile_fail
    /// # use aterm_objc::{Id, RcBlock};
    /// type Done = fn(Id);
    /// // A closure that disagrees with its alias is refused.
    /// let _ = unsafe { RcBlock::of::<Done, _>(|_status: isize| {}) };
    /// ```
    ///
    /// # Safety
    /// As [`RcBlock::new1`]: `P` must be exactly what the framework calls the
    /// block with.
    #[must_use]
    pub unsafe fn of<P: BlockFn, F: IntoBlock<P>>(closure: F) -> Option<Self> {
        // SAFETY: forwarded; the prototype is the caller's.
        unsafe { closure.into_block() }
    }
}

impl BlockPtr {
    /// Call a framework's block as prototype `P` (a [`BlockFn`] alias), as
    /// [`Self::call0`] and [`Self::call1`] do for theirs: the call that keeps
    /// a delegate's call site and the alias a test checks the same type. A nil
    /// block is not called.
    ///
    /// # Safety
    /// `self` must be nil or a live block whose prototype is exactly `P`.
    pub unsafe fn call<P: BlockFn<Ret = ()>>(self, args: P::Args) {
        if !self.0.is_null() {
            // SAFETY: non-nil, and the caller pins it live and of `P`.
            unsafe { P::call_block(self, args) }
        }
    }
}
