// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime class creation — the capability `metal/ffi.rs` deliberately never
//! needed, and the one the objc2 cluster was being paid for.
//!
//! `objc_allocateClassPair` / `class_addMethod` / `class_addIvar` /
//! `class_addProtocol` / `objc_registerClassPair` had ZERO uses anywhere in the
//! tree before this module. Everything aterm-gui's seven `declare_class!` sites
//! and winit's five actually do decomposes into exactly those five calls plus
//! `objc_msgSendSuper`.
//!
//! # What the real sites need, measured rather than guessed
//!
//! Read from the seven sites in `aterm-gui` (33 methods) and the five in
//! winit's macOS backend (71 methods):
//!
//! * **Superclasses**: only `NSObject` (5 sites) and `NSView` (2 sites) in
//!   aterm-gui; winit adds `NSWindow`, `NSApplication` and `NSResponder`.
//!   [`ClassBuilder`] takes any class, so this is not a constraint.
//! * **Ivars**: every one of the seven has exactly ONE `type Ivars = T`, where
//!   `T` is an ordinary Rust type — `EventLoopProxy<Wake>` for four of them, a
//!   plain struct of `Cell`/`RefCell`/`Retained`/`String` for the other three.
//!   So the requirement is "store one arbitrary Rust value per instance and
//!   drop it correctly", NOT objc2's per-field ivar machinery.
//! * **`dealloc`**: NOT ONE of the seven writes a `dealloc`. What they rely on
//!   is objc2's *generated* ivar drop. So this module owes an automatic
//!   `dealloc` that drops the Rust ivars and calls `[super dealloc]`, and owes
//!   no user-facing `dealloc` hook at all. (See [`crate::declare_class!`] for
//!   the flag that makes that drop sound even if `dealloc` runs before the
//!   ivars were written.)
//! * **Protocol conformance**: `NSObjectProtocol` (a marker objc2 needs for its
//!   own trait bounds and which the runtime already gives every `NSObject`
//!   subclass), `NSMenuDelegate` (1 site) and `NSToolbarDelegate` (1 site).
//!   That is the whole list — `class_addProtocol` twice.
//! * **Ivars are dropped on whatever thread performs the LAST RELEASE** — see
//!   the crate's soundness list (`S3`). `register` hands the object to
//!   Objective-C, and nothing in the runtime promises the final `release` comes
//!   from the thread that created it, so a Rust destructor can run on a foreign
//!   thread with no `Send` bound in sight. objc2 0.5 has the identical hole; it
//!   is why 0.6 introduced `MainThreadOnly`. Unclosed here, and NAMED.
//! * **Superclass sends**: four sites, three selectors — `init`,
//!   `initWithFrame:` and `updateTrackingAreas`. [`crate::msg_super`] plus
//!   [`ObjcSuper`](crate::ObjcSuper) covers all of them.

use std::ffi::{CStr, CString, c_void};
use std::mem::MaybeUninit;

use crate::runtime::{
    ClassPtr, Id, Sel, class, class_addIvar, class_addMethod, class_addProtocol,
    class_getInstanceVariable, ivar_getOffset, objc_allocateClassPair, objc_registerClassPair,
    protocol, superclass_of,
};

/// The name of the single ivar every declared class carries.
pub const IVAR_NAME: &CStr = c"_atermIvars";

/// Per-class facts resolved once at registration and read on every ivar access.
///
/// The class pointer is kept as a `usize` so this type is `Send + Sync` and can
/// live in a `OnceLock` — an ObjC class object is immortal, so the address is
/// stable for the life of the process and carries no aliasing meaning.
#[derive(Clone, Copy, Debug)]
pub struct ClassMeta {
    class_addr: usize,
    ivar_offset: isize,
}

impl ClassMeta {
    /// The registered class object.
    #[inline]
    #[must_use]
    pub fn class(&self) -> ClassPtr {
        // The address came from `objc_allocateClassPair`, i.e. from FFI, so it
        // already has exposed provenance; a class pointer is never dereferenced
        // by Rust anyway, only handed back to the runtime.
        ClassPtr::from_ptr(std::ptr::with_exposed_provenance_mut(self.class_addr))
    }

    /// Byte offset of [`IVAR_NAME`] within an instance.
    #[inline]
    #[must_use]
    pub const fn ivar_offset(&self) -> isize {
        self.ivar_offset
    }
}

/// The per-instance ivar payload: an initialised flag plus the Rust value.
///
/// # Why the flag
///
/// `class_createInstance` (what `+alloc` calls) returns ZEROED memory, so
/// `initialized` reads `false` on a freshly allocated instance. That makes
/// `dealloc` sound in the window between `+alloc` and the ivars being written:
/// if a superclass `init` fails and releases `self`, `dealloc` runs against a
/// slot that was never filled and correctly drops NOTHING, instead of calling
/// `drop_in_place` on all-zero bytes — which is undefined behaviour for almost
/// every real ivar type (`EventLoopProxy<Wake>` and `Retained<NSTrackingArea>`
/// among them). This is a deliberate improvement on objc2 0.5, where
/// `set_ivars` must be called before anything can send a message and nothing in
/// the type system enforces it.
#[repr(C)]
pub struct IvarSlot<T> {
    initialized: bool,
    value: MaybeUninit<T>,
}

impl<T> IvarSlot<T> {
    /// Fill an all-zero slot.
    ///
    /// # Safety
    /// `slot` must point at a properly aligned, writable `IvarSlot<T>` whose
    /// `initialized` flag is currently `false` (i.e. freshly `+alloc`ed memory,
    /// never a slot that was already filled — that would leak the old value).
    ///
    /// "Properly aligned" is the half that used to be UNDISCHARGED. The only
    /// caller is [`crate::declare_class!`]'s `alloc_init`, which derives `slot`
    /// from the instance base plus the registered ivar offset — and the offset
    /// is aligned within the instance while the BASE is only 16-aligned, so for
    /// `align_of::<Self>() > 16` this precondition was violated by construction
    /// from SAFE code. [`crate::ClassBuilder::add_rust_ivar`] now refuses such a
    /// `T` at compile time, which is what makes this contract dischargeable —
    /// see `S4` in the crate's soundness list.
    pub unsafe fn init(slot: *mut Self, value: T) {
        // SAFETY: the caller pins `slot` as an aligned, writable, not-yet-filled
        // slot. Writing the whole struct (rather than the fields) is a single
        // initialising store with no read of the old bytes.
        unsafe {
            slot.write(Self {
                initialized: true,
                value: MaybeUninit::new(value),
            });
        }
    }

    /// Borrow the value.
    ///
    /// The initialised flag is checked in RELEASE builds too, and that is a
    /// deliberate change from `debug_assert!`. The window it guards is not
    /// hypothetical: `alloc_init` is this crate's only constructor, but the
    /// class is registered with the Objective-C runtime under a public name, so
    /// Objective-C code — a nib, a `+alloc`/`-init` from a framework, an
    /// `NSClassFromString` — can mint an instance whose slot was never written.
    /// The old comment here claimed the write "was performed by `alloc_init`
    /// before any message could reach the instance", which is true of every
    /// instance THIS crate creates and of no other. A panic inside a
    /// trampoline is already caught and turned into an abort, so the failure is
    /// loud rather than an `assume_init_ref` over zeroed bytes.
    ///
    /// The cost is one load and one predictable branch, on a path that already
    /// pays a `catch_unwind` per message.
    ///
    /// # Safety
    /// `slot` must point at an aligned, readable `IvarSlot<T>` (initialised or
    /// zeroed) that outlives `'a`.
    ///
    /// The word "aligned" here was the SECOND undischargeable precondition of
    /// this type, and the more serious one: `get`'s only caller is the SAFE
    /// `ivars()` accessor the macro generates, which cannot establish it at all
    /// — the same shape as the pool contract F2 was raised for, one pass later.
    /// It is dischargeable now because `add_rust_ivar` refuses any `T` whose
    /// slot needs more than the 16 bytes `class_createInstance` guarantees, so
    /// every registered class HAS an aligned slot and the accessor's obligation
    /// is met by the class's own existence.
    #[must_use]
    pub unsafe fn get<'a>(slot: *const Self) -> &'a T {
        // SAFETY: the caller pins `slot` as aligned, readable and live for
        // `'a`; reading the flag is valid either way because `+alloc` zeroed
        // the whole instance.
        let this = unsafe { &*slot };
        assert!(
            this.initialized,
            "aterm-objc: ivars read before they were written — this instance was \
             not created by `alloc_init`, or a message reached it between \
             `+alloc` and the ivar store"
        );
        // SAFETY: `initialized` is only ever set by `init`, which wrote a valid
        // `T` into `value` in the same store, and `dispose` clears it before
        // dropping. The assertion above is what makes that a fact rather than a
        // convention.
        unsafe { this.value.assume_init_ref() }
    }

    /// Drop the value in place if it was ever written; idempotent.
    ///
    /// # Safety
    /// `slot` must point at an aligned, writable `IvarSlot<T>` (initialised or
    /// zeroed) that nothing else is borrowing. Alignment is discharged the way
    /// [`Self::init`] and [`Self::get`] discharge it — by `add_rust_ivar`
    /// refusing an over-aligned `T` — which is also the second of the two sites
    /// an over-aligned ivar aborted at (`(*slot).initialized` on the dealloc
    /// path, below).
    pub unsafe fn dispose(slot: *mut Self) {
        // SAFETY: the caller pins `slot` as writable; reading the flag is
        // always valid because `+alloc` zeroed it.
        let initialized = unsafe { (*slot).initialized };
        if !initialized {
            return;
        }
        // Clear the flag FIRST, so a re-entrant `dealloc` (which the runtime
        // does not do, but a superclass `dealloc` that resurrected `self` could
        // provoke) cannot drop the same value twice.
        // SAFETY: as above.
        unsafe {
            (*slot).initialized = false;
            std::ptr::drop_in_place((*slot).value.as_mut_ptr());
        }
    }
}

/// A class pair under construction. Consumed by [`ClassBuilder::register`].
///
/// Every mutator must run BEFORE registration: `class_addIvar` in particular is
/// only legal on an unregistered pair, because adding an ivar moves the
/// instance size and a registered class may already have instances.
pub struct ClassBuilder {
    cls: ClassPtr,
    name: CString,
}

impl ClassBuilder {
    /// Start a new class pair. `None` if a class of that name already exists —
    /// which, inside one process, means two [`crate::declare_class!`] sites
    /// picked the same `NAME`.
    ///
    /// # Safety
    /// `superclass` must be a live class object (it is dereferenced by the
    /// runtime to compute the new class's instance layout).
    #[must_use]
    pub unsafe fn new(superclass: ClassPtr, name: &CStr) -> Option<Self> {
        assert!(
            !superclass.is_null(),
            "aterm-objc: superclass not found for class {name:?} — the framework \
             that defines it is not linked into this binary"
        );
        // SAFETY: `superclass` is a live class object (asserted non-null above,
        // and class objects are immortal); `name` is a valid C string. The
        // runtime copies the name. `extra_bytes` is 0 because this crate stores
        // its payload in a real ivar rather than in indexed extra storage — an
        // ivar has a name the runtime can report and survives subclassing.
        let cls = unsafe { objc_allocateClassPair(superclass, name.as_ptr(), 0) };
        if cls.is_null() {
            return None;
        }
        Some(Self {
            cls,
            name: name.to_owned(),
        })
    }

    /// The class under construction (unregistered).
    #[inline]
    #[must_use]
    pub fn as_ptr(&self) -> ClassPtr {
        self.cls
    }

    /// Add an ivar big enough for `T`, named [`IVAR_NAME`].
    ///
    /// The type encoding written is `[Nc]` — "an array of N opaque bytes". That
    /// is the honest encoding for a Rust value, and it is deliberately NOT a
    /// pointer encoding: the runtime must not treat these bytes as an object
    /// reference to retain, release or scan.
    ///
    /// # An ivar cannot out-align the instance allocator: `S4`'s SECOND half
    ///
    /// `_Block_copy` allocates with `malloc` and a block capture needing more
    /// than 16-byte alignment is refused at compile time — that is `S4`, and it
    /// was recorded as CLOSED while its exact twin stood open one module over.
    /// `class_createInstance` (what `+alloc` calls) uses the same allocator, so
    /// the ivar SLOT inherits the instance base's alignment and nothing more:
    /// `class_addIvar` aligns the OFFSET within the instance, which is all it
    /// can do, and the base is the part `malloc` decides.
    ///
    /// MEASURED through `ivar_getOffset` and the raw `+alloc` addresses — no
    /// Rust reference anywhere in the measurement, so nothing is folded away —
    /// 4,096 instances per class on this box:
    ///
    /// ```text
    /// Ivars align  ivar offset  slot misaligned    base not 32-aligned
    ///        16         16          0 / 4096          2042 / 4096
    ///        32         32         19 / 4096            19 / 4096
    ///        64         64          9 / 4096             9 / 4096
    /// ```
    ///
    /// Sixteen is exact, and it is exact for the same reason and at the same
    /// number as the block case. The `align 16` row also shows the base really
    /// is only 16-aligned rather than accidentally more.
    ///
    /// It was reachable from safe code with NOT ONE `unsafe` token at the call
    /// site — `declare_class!{ type Ivars = Align64 }` then `X::alloc_init(v)`
    /// — and in a debug build it aborted at `IvarSlot::init`'s `ptr::write`
    /// ("unsafe precondition(s) violated: ptr::write requires that the pointer
    /// argument is aligned and non-null"), with a second site on the `dealloc`
    /// path. In RELEASE the precondition compiles out and LLVM is entitled to
    /// fold the alignment check to `true` off the reference's declared
    /// alignment, which makes it assumption-propagating undefined behaviour
    /// rather than a tolerated unaligned access.
    ///
    /// So the fix is the one the crate already wrote for blocks: refuse the
    /// type at compile time, since there is no knob to ask the runtime for more.
    ///
    /// ```compile_fail
    /// #[repr(align(32))]
    /// struct WideIvars(u64);
    /// aterm_objc::declare_class! {
    ///     struct DocOverAligned: NSObject {
    ///         const NAME: &str = "ATermDocOverAligned";
    ///         type Ivars = WideIvars;
    ///
    ///         @sel(ping)
    ///         fn ping(&self) {}
    ///     }
    /// }
    /// let _ = DocOverAligned::alloc_init(WideIvars(1));
    /// ```
    pub fn add_rust_ivar<T>(&mut self) {
        // `class_createInstance` allocates the instance with the same 16-byte
        // `malloc` guarantee `_Block_copy` has, and the slot sits at a fixed
        // offset from that base — so a `T` wanting more lands misaligned in
        // some fraction of instances and there is no way to ask for more. See
        // this method's docs for the measurement.
        const {
            assert!(
                align_of::<IvarSlot<T>>() <= 16,
                "aterm-objc: this class's `Ivars` need more than the 16-byte \
                 alignment `class_createInstance`'s allocator guarantees"
            )
        };
        let size = size_of::<IvarSlot<T>>();
        let align = align_of::<IvarSlot<T>>();
        debug_assert!(align.is_power_of_two());
        let align_log2 = u8::try_from(align.trailing_zeros()).expect("ivar alignment fits in u8");
        let encoding = CString::new(format!("[{size}c]")).expect("encoding has no interior NUL");
        // SAFETY: `self.cls` is an allocated, NOT-yet-registered class pair,
        // which is the only state in which `class_addIvar` is legal; the name
        // and encoding are valid C strings; `size`/`align_log2` describe a real
        // Rust type. The runtime copies both strings.
        let ok = unsafe {
            class_addIvar(
                self.cls,
                IVAR_NAME.as_ptr(),
                size,
                align_log2,
                encoding.as_ptr(),
            )
        };
        assert!(
            ok.as_bool(),
            "aterm-objc: class_addIvar failed on {:?}",
            self.name
        );
    }

    /// Add a method implementation.
    ///
    /// # Safety
    /// `imp` must be an `extern "C"` function whose prototype is exactly
    /// `(Id, Sel, ..args)` matching `types`, and it must not unwind. The
    /// [`crate::declare_class!`] macro is what normally guarantees both.
    pub unsafe fn add_method(&mut self, name: Sel, imp: *const c_void, types: &str) {
        let types = CString::new(types).expect("type encoding has no interior NUL");
        // SAFETY: the caller pins `imp`'s prototype; `self.cls` is a live class
        // pair and `name` a live selector. The runtime copies `types`.
        let ok = unsafe { class_addMethod(self.cls, name, imp, types.as_ptr()) };
        assert!(
            ok.as_bool(),
            "aterm-objc: class_addMethod failed on {:?} — a method with that \
             selector is already defined directly on this class",
            self.name
        );
    }

    /// Declare conformance to a protocol, by name.
    ///
    /// Panics if the protocol is absent from the process: for an AppKit
    /// protocol that means AppKit is not linked into THIS binary, which is a
    /// build defect rather than a runtime condition to tolerate.
    pub fn add_protocol(&mut self, name: &'static CStr) {
        let proto = protocol(name);
        assert!(
            !proto.is_null(),
            "aterm-objc: protocol {name:?} not present in this process (its \
             framework is not linked)"
        );
        // SAFETY: `self.cls` is a live class pair and `proto` a live, immortal
        // protocol object.
        let ok = unsafe { class_addProtocol(self.cls, proto) };
        assert!(
            ok.as_bool(),
            "aterm-objc: class_addProtocol({name:?}) failed on {:?}",
            self.name
        );
    }

    /// Register the pair and resolve the ivar offset.
    ///
    /// After this the class is live and immortal; the pair is never disposed
    /// (`objc_disposeClassPair` is deliberately not bound — every class this
    /// crate creates is a process-lifetime singleton).
    #[must_use]
    pub fn register(self) -> ClassMeta {
        // SAFETY: `self.cls` is an allocated, unregistered pair; registering it
        // is the documented terminal operation and makes the class usable.
        unsafe { objc_registerClassPair(self.cls) };
        // SAFETY: the class is registered, so its ivar table is final; the name
        // is the one `add_rust_ivar` used.
        let ivar = unsafe { class_getInstanceVariable(self.cls, IVAR_NAME.as_ptr()) };
        assert!(
            !ivar.is_null(),
            "aterm-objc: {:?} registered without its ivar",
            self.name
        );
        // SAFETY: `ivar` is a live `Ivar` handle from the class just registered.
        let ivar_offset = unsafe { ivar_getOffset(ivar) };
        ClassMeta {
            class_addr: self.cls.expose_provenance(),
            ivar_offset,
        }
    }
}

/// Resolve a superclass by name and start a builder, panicking with a message
/// that names the class on the two failures that can happen.
#[must_use]
pub fn begin(superclass_name: &'static CStr, class_name: &'static CStr) -> ClassBuilder {
    let sup = class(superclass_name);
    // SAFETY: `sup` came straight out of `objc_getClass`, so it is either a
    // live, immortal class object or null — and `ClassBuilder::new` asserts
    // non-null before the runtime ever reads it.
    unsafe { ClassBuilder::new(sup, class_name) }.unwrap_or_else(|| {
        panic!(
            "aterm-objc: objc_allocateClassPair({class_name:?}) returned nil — a \
             class with that name is already registered in this process"
        )
    })
}

/// Build the `objc_super` a `[super …]` send needs from inside a method of
/// `cls`.
///
/// `cls` must be the class that DEFINES the method doing the sending, not
/// `object_getClass(this)`: using the latter in a class that is itself
/// subclassed turns `[super foo]` into infinite recursion. The four real super
/// sends in the tree (`init` x3, `initWithFrame:` x2, `updateTrackingAreas` x2)
/// are all in leaf classes, but the rule is the same.
///
/// # Safety
/// `cls` must be a live class object and `this` a live instance of it.
#[inline]
#[must_use]
pub unsafe fn super_of(this: Id, cls: ClassPtr) -> crate::runtime::ObjcSuper {
    crate::runtime::ObjcSuper {
        receiver: this,
        // SAFETY: the caller pins `cls` as a live class object.
        super_class: unsafe { superclass_of(cls) },
    }
}

/// The `[super dealloc]` every declared class owes.
///
/// # Safety
/// `this` must be the instance currently being deallocated, and `cls` the class
/// that DEFINES the `dealloc` being run (so the send starts at its superclass).
pub unsafe fn send_super_dealloc(this: Id, cls: ClassPtr) {
    // SAFETY: the caller pins `this` as the instance being deallocated and
    // `cls` as the class defining that `dealloc`, so both are live.
    let sup = unsafe { super_of(this, cls) };
    // SAFETY: the prototype below is `-(void)dealloc`'s exact C signature and
    // the receiver slot holds a well-formed `objc_super`. Every class this
    // crate creates descends from `NSObject`, which implements `dealloc`, so
    // the send always resolves.
    unsafe {
        let f: unsafe extern "C" fn(*const crate::runtime::ObjcSuper, Sel) =
            crate::runtime::msg_super();
        f(&raw const sup, crate::sel!(dealloc));
    }
}

/// Abort the process after a Rust panic reached an `extern "C"` boundary.
///
/// Unwinding out of an ObjC frame is undefined behaviour, so every trampoline
/// [`crate::declare_class!`] generates catches and lands here. This is the
/// same rule the tree already applies to its C FFI entry points.
/// # Why not `eprintln!`
///
/// It was `eprintln!`, and `eprintln!` PANICS if the write fails — a closed or
/// full stderr, which is an ordinary state for a GUI app launched from Finder
/// or re-parented by `launchd`. The process still aborts either way, but a
/// panic here unwinds out of the very `extern "C"` frame this function exists
/// to stop, and lands on Rust's own shim with "thread caused non-unwinding
/// panic. aborting." — the unnamed message the guard was added to avoid. That
/// is F3's defect inside F3's own fix.
///
/// The replacement writes the three pieces straight to stderr and IGNORES the
/// result. It allocates nothing (no `format!`), so there is no second failure
/// mode, and the string every test greps for is unchanged.
#[cold]
#[inline(never)]
pub fn abort_on_unwind(method: &str) -> ! {
    use std::io::Write as _;
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(b"aterm-objc: panic escaped Objective-C method `");
    let _ = err.write_all(method.as_bytes());
    let _ = err.write_all(b"`; aborting\n");
    let _ = err.flush();
    std::process::abort()
}
