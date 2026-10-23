// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Method swizzling: replacing the implementation a live class already has.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};

use crate::encode::{Encode, strip_method_offsets};
use crate::runtime::{
    ClassPtr, Id, Sel, class_getInstanceMethod, method_getImplementation, method_setImplementation,
    superclass_of,
};

/// An `IMP` — the address of the function the runtime dispatches to.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Imp(NonNull<c_void>);

// SAFETY: an `Imp` is an immutable code address in a loaded image with no owner or thread affinity; copying or comparing it from any thread is sound.
unsafe impl Send for Imp {}
// SAFETY: as above.
unsafe impl Sync for Imp {}

// SAFETY: `@encode(IMP)` is `"^?"` (printed by clang, not assumed), and the type is `#[repr(transparent)]` over one pointer word, as `IMP` is.
unsafe impl Encode for Imp {
    const ENCODING: &'static str = "^?";
}

impl Imp {
    /// Wrap a raw code address; `None` for null ("no implementation").
    /// # Safety
    /// `ptr` must be an address the runtime handed out as an `IMP`, or the address of an `extern "C"` function in this image: [`SwizzleSite::original`] can produce a call through it.
    #[inline]
    #[must_use]
    pub const unsafe fn from_ptr(ptr: *const c_void) -> Option<Self> {
        match NonNull::new(ptr.cast_mut()) {
            Some(p) => Some(Self(p)),
            None => None,
        }
    }

    /// The raw code address, for identity comparison and for `dladdr`.
    #[inline]
    #[must_use]
    pub const fn as_ptr(self) -> *const c_void {
        self.0.as_ptr().cast_const()
    }
}

impl std::fmt::Debug for Imp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Imp({:p})", self.as_ptr())
    }
}

/// A method implementation's prototype, and the encoding derived from it.
/// # Safety
/// An implementor must be a bare `unsafe extern "C-unwind"` function pointer whose first two parameters are the receiver and selector, with [`Self::method_encoding`] generated from its own signature.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a method implementation `aterm-objc` can swizzle in",
    label = "not an `unsafe extern \"C-unwind\" fn(Id, Sel, ..) -> R`",
    note = "a swizzled implementation is entered exactly as the runtime enters             the method it replaces: the receiver first, the selector second, and             every remaining type must implement `Encode` so the install can             derive the method encoding from the prototype itself"
)]
pub unsafe trait MethodFn: Copy {
    /// The `@encode`-format method type string, `"<ret>@:<args…>"`, e.g. `"v@:@"` for `unsafe extern "C-unwind" fn(Id, Sel, Id)`.
    #[must_use]
    fn method_encoding() -> String;

    /// This function's address, as an `IMP`.
    #[must_use]
    fn imp(self) -> Imp;
}

macro_rules! impl_method_fn {
    ($($arg:ident),*) => {
        // SAFETY: the implementing type is an `unsafe extern "C-unwind"` function pointer with `Id`, `Sel` first, and `method_encoding` is generated from the same signature.
        unsafe impl<Ret: Encode, $($arg: Encode),*> MethodFn
            for unsafe extern "C-unwind" fn(Id, Sel, $($arg),*) -> Ret
        {
            fn method_encoding() -> String {
                let mut enc = String::new();
                enc.push_str(Ret::ENCODING);
                enc.push_str("@:");
                $( enc.push_str(<$arg as Encode>::ENCODING); )*
                enc
            }

            fn imp(self) -> Imp {
                const { assert!(size_of::<Self>() == size_of::<*const c_void>()) };
                // SAFETY: `Self` is a pointer-wide function pointer, so its value is the code address; function pointers are never null, and if one were, `expect` panics in safe code.
                let raw: *mut c_void = unsafe { std::mem::transmute_copy(&self) };
                Imp(NonNull::new(raw).expect("a function pointer is never null"))
            }
        }
    };
}

impl_method_fn!();
impl_method_fn!(A0);
impl_method_fn!(A0, A1);
impl_method_fn!(A0, A1, A2);
impl_method_fn!(A0, A1, A2, A3);
impl_method_fn!(A0, A1, A2, A3, A4);
impl_method_fn!(A0, A1, A2, A3, A4, A5);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6, A7);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_method_fn!(A0, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);

/// The class a selector's implementation actually lives on.
/// # Safety
/// `cls` must be a live class object or null.
#[must_use]
pub unsafe fn owning_class(cls: ClassPtr, sel: Sel) -> Option<ClassPtr> {
    if cls.is_null() {
        return None;
    }
    // SAFETY: `cls` is live; the lookup answers NULL for an unimplemented selector and walks the superclass chain itself.
    let method = unsafe { class_getInstanceMethod(cls, sel) };
    if method.is_null() {
        return None;
    }
    let mut owner = cls;
    loop {
        // SAFETY: `owner` is live (a superclass of a live class); a root class answers null.
        let sup = unsafe { superclass_of(owner) };
        if sup.is_null() {
            return Some(owner);
        }
        // SAFETY: `sup` is a live class object.
        let sup_method = unsafe { class_getInstanceMethod(sup, sel) };
        if !std::ptr::eq(sup_method, method) {
            return Some(owner);
        }
        owner = sup;
    }
}

/// What an install did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swizzle {
    /// The row was replaced.
    Replaced {
        /// The class the row actually lives on — not necessarily the class named in the call.
        owner: ClassPtr,
        /// The displaced implementation.
        previous: Imp,
    },
    /// The row already held exactly this implementation and nothing was written (idempotent re-entry).
    AlreadyInstalled {
        /// As above.
        owner: ClassPtr,
        /// The implementation the first install displaced, read back out of this site — the value a chain must call.
        previous: Imp,
    },
}

impl Swizzle {
    /// The class whose method table was read or written.
    #[must_use]
    pub const fn owner(self) -> ClassPtr {
        match self {
            Self::Replaced { owner, .. } | Self::AlreadyInstalled { owner, .. } => owner,
        }
    }

    /// The implementation a chain must call.
    #[must_use]
    pub const fn previous(self) -> Imp {
        match self {
            Self::Replaced { previous, .. } | Self::AlreadyInstalled { previous, .. } => previous,
        }
    }
}

/// Why an install refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwizzleError {
    /// The class pointer was null — a [`crate::class`] lookup that failed, or an `object_getClass` on nil.
    NullClass,
    /// Nothing in the class's chain implements the selector, so there is nothing to replace (adding a method is [`crate::ClassBuilder`]'s job).
    NoSuchMethod,
    /// The row has no type encoding, so the install's check cannot run.
    NoEncoding,
    /// The runtime's encoding and the one derived from the Rust prototype disagree (claim 3 in the module docs).
    EncodingMismatch {
        /// What the runtime holds, offsets stripped.
        registered: String,
        /// What [`MethodFn`] derived from the function being installed.
        expected: String,
    },
    /// This site holds an original and the row no longer holds what this site installed: a third party swizzled on top.
    Displaced {
        /// The implementation this call was asked to install.
        expected: Imp,
        /// What the row actually holds.
        found: Imp,
    },
    /// The row already holds the implementation being installed but this site has no original to chain to; a replaced implementation is not recoverable from the runtime.
    OrphanedInstall,
    /// This site is installed on a different row than the one this call names.
    WrongRow {
        /// The `(class, selector)` this site is installed on.
        claimed: (ClassPtr, Sel),
        /// The `(class, selector)` this call names.
        asked: (ClassPtr, Sel),
    },
    /// Another thread claimed this site first; nothing was written.
    Contended,
    /// `method_setImplementation` displaced something other than what this site published: a third party swizzled inside the install window.
    Raced {
        /// What this site published before the swap.
        published: Imp,
        /// What the swap actually displaced, and what the site now holds.
        actual: Imp,
    },
}

impl std::fmt::Display for SwizzleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NullClass => f.write_str("the class pointer is null"),
            Self::NoSuchMethod => f.write_str(
                "no class in the chain implements this selector, so there is no \
                 implementation to replace",
            ),
            Self::NoEncoding => {
                f.write_str("the registered row carries no type encoding to check against")
            }
            Self::EncodingMismatch {
                registered,
                expected,
            } => write!(
                f,
                "the runtime holds {registered:?} for this selector but the Rust prototype \
                 encodes to {expected:?} — installing it would enter the method at the wrong \
                 prototype, which no encoding check elsewhere in this tree could see"
            ),
            Self::Displaced { expected, found } => write!(
                f,
                "this site is already installed and the row holds {found:?} rather than the \
                 {expected:?} this call would install — another party swizzled on top, and \
                 re-installing would publish THEIR implementation as the original and chain \
                 past ours"
            ),
            Self::WrongRow { claimed, asked } => write!(
                f,
                "this site is installed on {:?}/{:?} and this call names \
                 {:?}/{:?} — a site claims a ROW, so answering about a \
                 different one would hand back an original that never came \
                 from it",
                claimed.0, claimed.1, asked.0, asked.1
            ),
            Self::Contended => f.write_str(
                "another thread is installing into this site right now and claimed it first; \
                 nothing was written",
            ),
            Self::OrphanedInstall => f.write_str(
                "the row already holds this implementation but this site has no original to \
                 chain to; a replaced implementation is not recoverable from the runtime",
            ),
            Self::Raced { published, actual } => write!(
                f,
                "the swap displaced {actual:?}, not the {published:?} this site published — a \
                 third party swizzled inside the install window; the site has been repaired"
            ),
        }
    }
}

impl std::error::Error for SwizzleError {}

/// What a site claimed: the row, and the implementation that row held.
#[derive(Clone, Copy, Debug)]
struct Claim {
    /// The implementation the row held before this site replaced it.
    original: Imp,
    /// The class the row actually lives on — see the module docs on blast
    /// radius.
    owner: ClassPtr,
    /// The selector.
    sel: Sel,
}

// SAFETY: every field is an immortal, immutable word — a code address, a class object and an interned selector — readable from any thread.
unsafe impl Send for Claim {}
// SAFETY: as above.
unsafe impl Sync for Claim {}

/// One swizzle: the row patched, and the implementation it displaced.
pub struct SwizzleSite<F: MethodFn> {
    /// The [`Claim`] this site published, or null when nothing is installed.
    claim: AtomicPtr<Claim>,
    /// The prototype the install verified. Zero-sized; a function pointer type
    /// is `Send`/`Sync`, so this costs the site no auto-trait.
    _proto: PhantomData<F>,
}

impl<F: MethodFn> Default for SwizzleSite<F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: MethodFn> std::fmt::Debug for SwizzleSite<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwizzleSite")
            .field("claim", &self.claim())
            .finish()
    }
}

impl<F: MethodFn> SwizzleSite<F> {
    /// An empty site, `const` so it can be a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            claim: AtomicPtr::new(std::ptr::null_mut()),
            _proto: PhantomData,
        }
    }

    /// The implementation this site displaced, if it has installed one.
    #[must_use]
    pub fn original(&self) -> Option<F> {
        let raw = self.claim()?.original.as_ptr();
        const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
        // SAFETY: published by `install`, which refused unless the runtime's encoding equals the one `F` derives; `F` is a pointer-wide function pointer, so this reads the word back as the checked type.
        Some(unsafe { std::mem::transmute_copy::<*const c_void, F>(&raw) })
    }

    /// The row this site is installed on (the class the implementation lives on, and the selector), or `None` if it never installed.
    #[must_use]
    pub fn installed_row(&self) -> Option<(ClassPtr, Sel)> {
        self.claim().map(|c| (c.owner, c.sel))
    }

    /// The published claim, if any.
    fn claim(&self) -> Option<&'static Claim> {
        let raw = self.claim.load(Ordering::Acquire);
        if raw.is_null() {
            return None;
        }
        // SAFETY: a non-null value was published by `install`'s compare-exchange from `Box::into_raw`, and is never freed or written again (the `Raced` repair publishes a new claim), so `&'static` is valid.
        Some(unsafe { &*raw })
    }

    /// Replace `cls`'s implementation of `sel` with `new`, publishing the displaced one into this site.
    /// # Safety
    /// `new` must be entered exactly as the runtime enters the implementation it replaces and must not unwind (catch and [`crate::abort_on_unwind`], as [`crate::declare_class!`]'s trampolines do).
    /// # Errors
    /// Every [`SwizzleError`] except [`SwizzleError::Raced`] means nothing was written.
    pub unsafe fn install(
        &'static self,
        cls: ClassPtr,
        sel: Sel,
        new: F,
    ) -> Result<Swizzle, SwizzleError> {
        if cls.is_null() {
            return Err(SwizzleError::NullClass);
        }
        // SAFETY: the caller pins `cls` as live-or-null and the null case is handled above.
        let owner = unsafe { owning_class(cls, sel) }.ok_or(SwizzleError::NoSuchMethod)?;

        if let Some(claimed) = self.installed_row()
            && (claimed.0 != owner || claimed.1 != sel)
        {
            return Err(SwizzleError::WrongRow {
                claimed,
                asked: (owner, sel),
            });
        }

        // SAFETY: `owner` is a live class that implements `sel`.
        let registered = unsafe { crate::runtime::method_types(owner, sel) }
            .map(|raw| strip_method_offsets(&raw))
            .ok_or(SwizzleError::NoEncoding)?;
        let expected = F::method_encoding();
        if registered != expected {
            return Err(SwizzleError::EncodingMismatch {
                registered,
                expected,
            });
        }

        // SAFETY: `owner` is live and implements `sel`; the `Method` is used within this call and never stored.
        let method = unsafe { class_getInstanceMethod(owner, sel) };
        if method.is_null() {
            // `owning_class` found the row through the same accessor, but a
            return Err(SwizzleError::NoSuchMethod);
        }
        // SAFETY: `method` is the live handle from the lookup above.
        let current = unsafe { method_getImplementation(method) };
        // SAFETY: a registered method has an implementation; `from_ptr` answers `None` rather than asserting if not.
        let current = unsafe { Imp::from_ptr(current) }.ok_or(SwizzleError::NoSuchMethod)?;
        let new_imp = new.imp();

        if current == new_imp {
            return match self.claim() {
                Some(c) => Ok(Swizzle::AlreadyInstalled {
                    owner,
                    previous: c.original,
                }),
                None => Err(SwizzleError::OrphanedInstall),
            };
        }
        if self.claim().is_some() {
            // Installed once and the row has moved since; do not chain through
            return Err(SwizzleError::Displaced {
                expected: new_imp,
                found: current,
            });
        }

        let claim = Box::into_raw(Box::new(Claim {
            original: current,
            owner,
            sel,
        }));
        if self
            .claim
            .compare_exchange(
                std::ptr::null_mut(),
                claim,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            // SAFETY: `claim` came from `Box::into_raw` above and the failed compare-exchange means no other thread saw it.
            drop(unsafe { Box::from_raw(claim) });
            return Err(SwizzleError::Contended);
        }

        // SAFETY: `method` is the live handle above; `new_imp` is an `extern "C"` function whose prototype matched the row's encoding.
        let displaced = unsafe { method_setImplementation(method, new_imp.as_ptr()) };
        // SAFETY: the row had an implementation (read above), so the call returns it.
        let displaced = unsafe { Imp::from_ptr(displaced) }.unwrap_or(current);

        if displaced != current {
            let repaired = Box::into_raw(Box::new(Claim {
                original: displaced,
                owner,
                sel,
            }));
            self.claim.store(repaired, Ordering::Release);
            return Err(SwizzleError::Raced {
                published: current,
                actual: displaced,
            });
        }

        Ok(Swizzle::Replaced {
            owner,
            previous: displaced,
        })
    }
}
