// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Runtime class creation: `objc_allocateClassPair`, `class_addMethod`, `class_addIvar`, `class_addProtocol`, `objc_registerClassPair`, plus `objc_msgSendSuper` for `[super …]`.

use std::ffi::{CStr, CString, c_void};
use std::mem::MaybeUninit;

use crate::runtime::{
    ClassPtr, Id, Sel, class, class_addIvar, class_addMethod, class_addProtocol,
    class_getInstanceVariable, ivar_getOffset, objc_allocateClassPair, objc_registerClassPair,
    protocol_or_register, superclass_of,
};

/// The name of the single ivar every declared class carries.
pub(crate) const IVAR_NAME: &CStr = c"_atermIvars";

/// A witness that the caller asked `+[NSThread isMainThread]` on this thread and got yes.
#[derive(Clone, Copy, Debug)]
pub struct MainThread {
    /// `!Send`/`!Sync`: a witness is only true on the thread that minted it.
    _not_send: std::marker::PhantomData<*const u8>,
}

impl MainThread {
    /// A witness, or `None` off the main thread.
    #[must_use]
    pub fn new() -> Option<Self> {
        static NS_THREAD_CLS: std::sync::atomic::AtomicPtr<c_void> =
            std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
        let cls = {
            let p = NS_THREAD_CLS.load(std::sync::atomic::Ordering::Relaxed);
            if !p.is_null() {
                Id::from_ptr(p)
            } else {
                let id = crate::runtime::class(c"NSThread").as_id();
                if !id.is_null() {
                    NS_THREAD_CLS.store(id.as_ptr(), std::sync::atomic::Ordering::Relaxed);
                }
                id
            }
        };
        // SAFETY: `+[NSThread isMainThread]` is a side-effect-free `BOOL` class query on an always-linked Foundation class, cast as `-(BOOL)(id, SEL)`.
        let is_main = unsafe {
            let f: unsafe extern "C-unwind" fn(Id, Sel) -> crate::encode::Bool =
                crate::checked_msg();
            f(cls, crate::sel!(isMainThread))
        };
        is_main.as_bool().then(Self::mint)
    }

    /// A witness without asking, for a caller with its own proof and for tests of classes that touch no AppKit state.
    /// # Safety
    /// The caller must be on the process main thread, or know that the class has no main-thread affinity and its `Ivars` may be dropped on any thread (`S3`).
    #[must_use]
    pub const unsafe fn new_unchecked() -> Self {
        Self::mint()
    }

    /// The one place the token is constructed.
    #[inline]
    const fn mint() -> Self {
        Self {
            _not_send: std::marker::PhantomData,
        }
    }
}

/// Per-class facts resolved once at registration and read on every ivar access.
#[derive(Clone, Copy, Debug)]
pub struct ClassMeta {
    class_addr: usize,
    ivar_offset: isize,
}

impl ClassMeta {
    /// The registered class object.
    #[inline]
    #[must_use]
    pub const fn class(&self) -> ClassPtr {
        // SAFETY: `class_addr` is private and written only by `ClassBuilder::register`, from the pair `objc_registerClassPair` just registered; classes are never disposed, so it is still a class.
        unsafe { ClassPtr::from_ptr(std::ptr::with_exposed_provenance_mut(self.class_addr)) }
    }

    /// Byte offset of the `_atermIvars` ivar within an instance.
    #[inline]
    #[must_use]
    pub const fn ivar_offset(&self) -> isize {
        self.ivar_offset
    }
}

/// The per-instance ivar payload: an initialised flag plus the Rust value.
#[repr(C)]
pub struct IvarSlot<T> {
    initialized: bool,
    value: MaybeUninit<T>,
}

impl<T> IvarSlot<T> {
    /// Fill an all-zero slot.
    /// # Safety
    /// `slot` must point at a properly aligned, writable `IvarSlot<T>` whose `initialized` flag is `false` (freshly `+alloc`ed; refilling would leak the old value).
    pub const unsafe fn init(slot: *mut Self, value: T) {
        // SAFETY: `slot` is aligned, writable and unfilled.
        unsafe {
            slot.write(Self {
                initialized: true,
                value: MaybeUninit::new(value),
            });
        }
    }

    /// Borrow the value.
    /// # Safety
    /// `slot` must point at an aligned, readable `IvarSlot<T>` (initialised or zeroed) that outlives `'a`.
    #[must_use]
    pub unsafe fn get<'a>(slot: *const Self) -> &'a T {
        // SAFETY: `slot` is aligned, readable and live for `'a`; the flag is readable either way because `+alloc` zeroed the instance.
        let this = unsafe { &*slot };
        assert!(
            this.initialized,
            "aterm-objc: ivars read before they were written — this instance was \
             not created by `alloc_init`, or a message reached it between \
             `+alloc` and the ivar store"
        );
        // SAFETY: only `init` sets `initialized`, in the same store that wrote a valid `T`, and `dispose` clears it before dropping; checked above.
        unsafe { this.value.assume_init_ref() }
    }

    /// Drop the value in place if it was ever written; idempotent.
    /// # Safety
    /// `slot` must point at an aligned, writable `IvarSlot<T>` (initialised or zeroed) that nothing else is borrowing.
    pub unsafe fn dispose(slot: *mut Self) {
        // SAFETY: the caller pins `slot` as writable; reading the flag is always valid because `+alloc` zeroed it.
        let initialized = unsafe { (*slot).initialized };
        if !initialized {
            return;
        }
        // SAFETY: as above.
        unsafe {
            (*slot).initialized = false;
            std::ptr::drop_in_place((*slot).value.as_mut_ptr());
        }
    }
}

/// A class pair under construction.
pub struct ClassBuilder {
    cls: ClassPtr,
    name: CString,
}

impl ClassBuilder {
    /// Start a new class pair.
    /// # Safety
    /// `superclass` must be a live class object; the runtime reads it to lay out the new class.
    #[must_use]
    pub unsafe fn new(superclass: ClassPtr, name: &CStr) -> Option<Self> {
        assert!(
            !superclass.is_null(),
            "aterm-objc: superclass not found for class {name:?} — the framework \
             that defines it is not linked into this binary"
        );
        // SAFETY: `superclass` is a live, immortal class (non-null asserted above); `name` is a valid C string the runtime copies.
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
    pub const fn as_ptr(&self) -> ClassPtr {
        self.cls
    }

    /// Add an ivar big enough for `T`, named `_atermIvars`.
    ///
    /// `T` may not be aligned above 16 (`S4`): `class_createInstance` allocates
    /// with `malloc`, which guarantees only 16-byte alignment, and
    /// `class_addIvar` aligns the ivar's offset but not the base (a 32-aligned
    /// slot landed misaligned in 19 of 4,096 `+alloc`s). A misaligned slot
    /// makes every `&T` undefined behaviour, so an over-aligned `T` is refused
    /// at compile time:
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
    /// // SAFETY: never reached — the `const` assertion above rejects the
    /// // over-aligned `Ivars` before this line can run.
    /// let _ = DocOverAligned::alloc_init(
    ///     unsafe { aterm_objc::MainThread::new_unchecked() },
    ///     WideIvars(1),
    /// );
    /// ```
    pub fn add_rust_ivar<T>(&mut self) {
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
        // SAFETY: `self.cls` is an allocated, unregistered pair (the only state where `class_addIvar` is legal); the strings are valid C strings the runtime copies; `size`/`align_log2` describe a real Rust type.
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
    /// # Safety
    /// `imp` must be an `extern "C"` function whose prototype is exactly `(Id, Sel, ..args)` matching `types`, and it must not unwind.
    pub unsafe fn add_method(&mut self, name: Sel, imp: *const c_void, types: &str) {
        let types = CString::new(types).expect("type encoding has no interior NUL");
        // SAFETY: the caller pins `imp`'s prototype; `self.cls` is a live class pair and `name` a live selector.
        let ok = unsafe { class_addMethod(self.cls, name, imp, types.as_ptr()) };
        assert!(
            ok.as_bool(),
            "aterm-objc: class_addMethod failed on {:?} — a method with that \
             selector is already defined directly on this class",
            self.name
        );
    }

    /// Declare conformance to a protocol, by name.
    pub fn add_protocol(&mut self, name: &'static CStr) {
        let proto = protocol_or_register(name);
        // SAFETY: `self.cls` is a live class pair and `proto` a live, immortal protocol object — `protocol_or_register` never returns null.
        let ok = unsafe { class_addProtocol(self.cls, proto) };
        assert!(
            ok.as_bool(),
            "aterm-objc: class_addProtocol({name:?}) failed on {:?}",
            self.name
        );
    }

    /// Register the pair and resolve the ivar offset.
    #[must_use]
    pub fn register(self) -> ClassMeta {
        // SAFETY: `self.cls` is an allocated, unregistered pair; registering it is the documented terminal operation and makes the class usable.
        unsafe { objc_registerClassPair(self.cls) };
        // SAFETY: the class is registered, so its ivar table is final; the name is the one `add_rust_ivar` used.
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

/// Resolve a superclass by name and start a builder, panicking with a message that names the class on the two failures that can happen.
#[must_use]
pub fn begin(superclass_name: &'static CStr, class_name: &'static CStr) -> ClassBuilder {
    let sup = class(superclass_name);
    // SAFETY: `sup` came from `objc_getClass`, so it is an immortal class or null, and `ClassBuilder::new` asserts non-null before the runtime reads it.
    unsafe { ClassBuilder::new(sup, class_name) }.unwrap_or_else(|| {
        panic!(
            "aterm-objc: objc_allocateClassPair({class_name:?}) returned nil — a \
             class with that name is already registered in this process"
        )
    })
}

/// Build the `objc_super` a `[super …]` send needs from inside a method of `cls`.
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
/// # Safety
/// `this` must be the instance currently being deallocated, and `cls` the class that defines the `dealloc` being run (so the send starts at its superclass).
pub unsafe fn send_super_dealloc(this: Id, cls: ClassPtr) {
    // SAFETY: the caller pins `this` as the instance being deallocated and `cls` as the class defining that `dealloc`, so both are live.
    let sup = unsafe { super_of(this, cls) };
    // SAFETY: the prototype is `-(void)dealloc`'s exact signature and the receiver slot holds a well-formed `objc_super`.
    unsafe {
        let f: unsafe extern "C-unwind" fn(*const crate::runtime::ObjcSuper, Sel) =
            crate::runtime::msg_super();
        f(&raw const sup, crate::sel!(dealloc));
    }
}

/// Abort the process after a Rust panic reached an `extern "C"` boundary.
#[cold]
#[inline(never)]
pub fn abort_on_unwind(method: &str) -> ! {
    use std::io::Write as _;
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(b"aterm-objc: panic escaped Objective-C method `");
    let _ = err.write_all(method.as_bytes());
    let _ = err.write_all(b"`; aborting\n");
    let _ = err.flush();
    #[expect(
        clippy::disallowed_methods,
        reason = "unwinding into an Objective-C frame is undefined behaviour: a real crash, not a stand-in"
    )]
    std::process::abort()
}
