// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `declare_class!` macro itself.

/// Build a `&'static CStr` from tokens, checked at compile time.
#[macro_export]
macro_rules! cstr {
    ($($tok:tt)*) => {
        const {
            match ::core::ffi::CStr::from_bytes_with_nul(
                ::core::concat!($($tok)*, "\0").as_bytes(),
            ) {
                ::core::result::Result::Ok(s) => s,
                ::core::result::Result::Err(_) => panic!("name contains an interior NUL"),
            }
        }
    };
}

/// Substitute `()` for an omitted return type.
#[doc(hidden)]
#[macro_export]
macro_rules! __aterm_objc_ret {
    () => {
        ()
    };
    ($t:ty) => {
        $t
    };
}

/// A selector's token stream as the string the runtime knows it by (`@sel(a:b:)` becomes `"a:b:"`), so a containment report names the selector.
#[doc(hidden)]
#[macro_export]
macro_rules! __aterm_objc_sel_str {
    ($($tok:tt)+) => {
        ::core::concat!($(::core::stringify!($tok)),+)
    };
}

/// The body of a declared method's trampoline, by the containment marker after `@sel(…)`.
#[doc(hidden)]
#[macro_export]
macro_rules! __aterm_objc_method_call {
    ([] $name:ident :: $method:ident, [$($sel_tok:tt)+], $slf:ident, [$($ret:ty)?], ($($arg:ident),*)) => {
        match $crate::exception::contain(
            $crate::__aterm_objc_sel_str!($($sel_tok)+),
            move || $name::$method($slf $(, $arg)*),
        ) {
            ::core::result::Result::Ok(__v) => __v,
            ::core::result::Result::Err(_) => {
                $crate::exception::inert::<$crate::__aterm_objc_ret!($($ret)?)>()
            }
        }
    };
    ([abort_on_exception] $name:ident :: $method:ident, [$($sel_tok:tt)+], $slf:ident, [$($ret:ty)?], ($($arg:ident),*)) => {
        $name::$method($slf $(, $arg)*)
    };
    ([$other:ident] $name:ident :: $method:ident, [$($sel_tok:tt)+], $slf:ident, [$($ret:ty)?], ($($arg:ident),*)) => {
        ::core::compile_error!(::core::concat!(
            "aterm-objc: `@",
            ::core::stringify!($other),
            "` after @sel(",
            $crate::__aterm_objc_sel_str!($($sel_tok)+),
            ") is not a containment policy — the only marker is \
             `@abort_on_exception` (keep the abort for a method whose zero \
             return is not inert); write nothing for the default, which \
             contains an NSException and returns the inert zero",
        ))
    };
}

/// Count the `:` tokens in a selector's token stream, which [`declare_class!`] checks against the argument count the encoding is written from.
#[doc(hidden)]
#[macro_export]
macro_rules! __aterm_objc_colons {
    () => { 0usize };
    (: $($rest:tt)*) => { 1usize + $crate::__aterm_objc_colons!($($rest)*) };
    ($skip:tt $($rest:tt)*) => { $crate::__aterm_objc_colons!($($rest)*) };
}

/// Count token trees — used on the argument-name repetition, one `tt` each.
#[doc(hidden)]
#[macro_export]
macro_rules! __aterm_objc_count {
    () => { 0usize };
    ($head:tt $($rest:tt)*) => { 1usize + $crate::__aterm_objc_count!($($rest)*) };
}

/// Define an Objective-C class at runtime.
///
/// ```no_run
/// # use aterm_objc::Id;
/// # struct ReduceMotionIvars { relay: Box<dyn Fn()> }
/// aterm_objc::declare_class! {
///     /// The notification-observer target.
///     pub(crate) struct ReduceMotionTarget: NSObject {
///         const NAME: &str = "ATermDocReduceMotionTarget";
///         type Ivars = ReduceMotionIvars;
///
///         @sel(reduceMotionDidChange:)
///         fn reduce_motion_did_change(&self, _note: Id) {
///             (self.ivars().relay)();
///         }
///     }
/// }
/// ```
///
/// # Shape
///
/// * `@sel(name:)`, not an attribute: in `macro_rules!`,
///   `$(#[$m:meta])* #[sel(…)]` is ambiguous because `sel(bump)` is itself a
///   valid `meta`, and a leading `@` cannot start an attribute.
/// * `Send`/`Sync` are structural (the marker is `!Send`) and interior
///   mutability lives in the ivar type (`Cell`, `RefCell`). Instantiating
///   without a [`crate::MainThread`] witness does not compile:
///
///   ```compile_fail
///   aterm_objc::declare_class! {
///       struct DocNoWitness: NSObject {
///           const NAME: &str = "ATermDocNoWitness";
///           type Ivars = ();
///
///           @sel(ping)
///           fn ping(&self) {}
///       }
///   }
///   // `alloc_init` takes a `MainThread` witness first.
///   let _ = DocNoWitness::alloc_init(());
///   ```
/// * One ivar, not a struct of them — see [`crate::declare`].
/// * `&self` is captured as an `ident`: a `self` token emitted by the macro
///   lives in the macro's hygiene context, so a call-site body could not name
///   it (`E0424`).
///
/// # Safety
/// What the macro cannot check and the caller owes.
///
/// Checked at compile time: the selector's colon count equals the argument
/// count; `Ivars` is aligned to at most 16 bytes (`S4`, see
/// [`crate::ClassBuilder::add_rust_ivar`]); a method has at most fourteen
/// arguments ([`crate::MsgFn`]'s ceiling), so everything registered is
/// sendable from Rust.
///
/// An arity mismatch:
///
/// ```compile_fail
/// aterm_objc::declare_class! {
///     struct DocArityMismatch: NSObject {
///         const NAME: &str = "ATermDocArityMismatch";
///         type Ivars = ();
///
///         // `noArgsAtAll` has no colon, so it takes no argument — but the
///         // encoding written from the Rust signature says `"v@:q"`, and
///         // AppKit reads arity from the encoding.
///         @sel(noArgsAtAll)
///         fn no_args_at_all(&self, _n: i64) {}
///     }
/// }
/// ```
///
/// A method wider than the ceiling:
///
/// ```compile_fail
/// aterm_objc::declare_class! {
///     struct DocTooWide: NSObject {
///         const NAME: &str = "ATermDocTooWide";
///         type Ivars = ();
///
///         // Fifteen arguments — seventeen parameters.
///         @sel(a:b:c:d:e:f:g:h:i:j:k:l:m:n:o:)
///         fn too_wide(
///             &self,
///             _a: i64, _b: i64, _c: i64, _d: i64, _e: i64,
///             _f: i64, _g: i64, _h: i64, _i: i64, _j: i64,
///             _k: i64, _l: i64, _m: i64, _n: i64, _o: i64,
///         ) {}
///     }
/// }
/// ```
///
/// Fourteen arguments compile, register and send (`tests/declared_abi.rs`).
/// Matched arities of one, two and three arguments:
///
/// ```
/// # use aterm_objc::{Bool, Id, Sel};
/// aterm_objc::declare_class! {
///     struct DocArityMatched: NSObject {
///         const NAME: &str = "ATermDocArityMatched";
///         type Ivars = ();
///
///         @sel(oneArg:)
///         fn one_arg(&self, _n: i64) {}
///
///         @sel(addFirst:second:)
///         fn add_two(&self, a: i64, b: i64) -> i64 { a + b }
///
///         @sel(control:textView:doCommandBySelector:)
///         fn control(&self, _c: Id, _tv: Id, _s: Sel) -> Bool { Bool::NO }
///     }
/// }
/// ```
#[macro_export]
macro_rules! declare_class {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident : $super:ident {
            const NAME: &str = $objc_name:literal;
            type Ivars = $ivars:ty;
            $(protocols: [$($proto:ident),* $(,)?];)?

            $(
                $(#[$mmeta:meta])*
                @sel($($sel_tok:tt)+)
                $(@$policy:ident)?
                fn $method:ident(& $slf:ident $(, $arg:ident : $argty:ty)* $(,)?) $(-> $ret:ty)? $body:block
            )*
        }
    ) => {
        $(#[$meta])*
        #[repr(C)]
        $vis struct $name {
            /// The instance's real storage belongs to the Objective-C runtime;
            /// this type is a zero-sized marker at that address, never a value.
            _opaque: [u8; 0],
            /// Makes the type `!Send`/`!Sync`: every declared class in aterm is
            /// main-thread AppKit state.
            _not_thread_safe: ::core::marker::PhantomData<*const u8>,
        }

        // SAFETY: `$name` is a zero-sized marker reached only through a reference to a live instance of the class `class()` registers, once, before any instance can exist.
        unsafe impl $crate::ClassType for $name {
            const NAME: &'static str = $objc_name;

            fn class() -> $crate::ClassPtr {
                Self::meta().class()
            }
        }

        impl $name {
            /// The class + ivar offset, registering the pair on first call.
            fn meta() -> &'static $crate::ClassMeta {
                static META: ::std::sync::OnceLock<$crate::ClassMeta> =
                    ::std::sync::OnceLock::new();
                META.get_or_init(|| {
                    let mut __b = $crate::begin(
                        $crate::cstr!(::core::stringify!($super)),
                        $crate::cstr!($objc_name),
                    );
                    const {
                        ::core::assert!(
                            ::core::mem::align_of::<$crate::IvarSlot<$ivars>>() <= 16,
                            ::core::concat!(
                                "aterm-objc: `",
                                ::core::stringify!($name),
                                "`'s `Ivars` need more than the 16-byte \
                                 alignment `class_createInstance`'s allocator \
                                 guarantees, so the ivar slot would land \
                                 misaligned in some fraction of instances",
                            ),
                        )
                    };
                    __b.add_rust_ivar::<$ivars>();
                    $($( __b.add_protocol($crate::cstr!(::core::stringify!($proto))); )*)?

                    {
                        unsafe extern "C" fn __dealloc(__this: $crate::Id, _cmd: $crate::Sel) {
                            let __guard = ::std::panic::catch_unwind(
                                ::std::panic::AssertUnwindSafe(|| {
                                    let __meta = META.get().expect(
                                        "-dealloc runs only on an instance, \
                                         which cannot exist before the class \
                                         is registered and META initialised",
                                    );
                                    let __slot = ::core::ptr::with_exposed_provenance_mut::<
                                        $crate::IvarSlot<$ivars>,
                                    >(
                                        __this
                                            .expose_provenance()
                                            .wrapping_add_signed(__meta.ivar_offset()),
                                    );
                                    // SAFETY: `__this` is being deallocated, so the slot is live, writable and unaliased; `dispose` tolerates a never-written slot.
                                    unsafe { $crate::IvarSlot::<$ivars>::dispose(__slot) };
                                    // SAFETY: `__this` is being deallocated and `__meta.class()` defines this `dealloc`, so the super send starts at its superclass.
                                    unsafe {
                                        $crate::send_super_dealloc(__this, __meta.class());
                                    }
                                }),
                            );
                            if __guard.is_err() {
                                $crate::abort_on_unwind("dealloc");
                            }
                        }
                        // SAFETY: `__dealloc` is `extern "C"` with the exact `-(void)dealloc` prototype `(id, SEL)`, matches its encoding, and cannot unwind (the guard aborts).
                        unsafe {
                            __b.add_method(
                                $crate::sel!(dealloc),
                                __dealloc as *const ::core::ffi::c_void,
                                &$crate::method_encoding!(()),
                            );
                        }
                    }

                    $({
                        const {
                            ::core::assert!(
                                $crate::__aterm_objc_count!($($arg)*) + 2 <= 16,
                                ::core::concat!(
                                    "aterm-objc: `fn ",
                                    ::core::stringify!($method),
                                    "` has more arguments than a message send \
                                     can carry — `MsgFn` is implemented up to \
                                     SIXTEEN parameters including the implicit \
                                     `self` and `_cmd`, i.e. fourteen \
                                     arguments. Registering it would create a \
                                     method Objective-C can reach and Rust \
                                     cannot.",
                                ),
                            )
                        };
                        const {
                            ::core::assert!(
                                $crate::__aterm_objc_colons!($($sel_tok)+)
                                    == $crate::__aterm_objc_count!($($arg)*),
                                ::core::concat!(
                                    "aterm-objc: @sel(",
                                    ::core::stringify!($($sel_tok)+),
                                    ") and `fn ",
                                    ::core::stringify!($method),
                                    "` disagree on arity — an Objective-C \
                                     selector takes one argument per colon, \
                                     and the type encoding this macro writes \
                                     from the Rust signature is where the \
                                     runtime reads that arity from",
                                ),
                            )
                        };
                        unsafe extern "C" fn __tramp(
                            __this: $crate::Id,
                            _cmd: $crate::Sel,
                            $($arg: $argty),*
                        ) $(-> $ret)? {
                            let __guard = ::std::panic::catch_unwind(
                                ::std::panic::AssertUnwindSafe(move || {
                                    // SAFETY: the runtime delivers a message
                                    // only to a live instance of the declaring
                                    // class or a subclass (nil short-circuits
                                    // before any IMP), and subclassing does not
                                    // move this class's ivar offset. `$name` is
                                    // zero-sized, so the reference borrows no
                                    // bytes of its own.
                                    let __self: &$name =
                                        unsafe { &*__this.cast::<$name>().cast_const() };
                                    $crate::__aterm_objc_method_call!(
                                        [$($policy)?] $name::$method,
                                        [$($sel_tok)+],
                                        __self,
                                        [$($ret)?],
                                        ($($arg),*)
                                    )
                                }),
                            );
                            match __guard {
                                ::core::result::Result::Ok(__v) => __v,
                                ::core::result::Result::Err(_) => {
                                    $crate::abort_on_unwind(::core::stringify!($method))
                                }
                            }
                        }
                        // SAFETY: `__tramp` is `extern "C"` with the `(id, SEL, ..)` prototype the runtime calls, its encoding comes from the same Rust types, and it cannot unwind (panics abort; NSExceptions are contained).
                        unsafe {
                            __b.add_method(
                                $crate::sel!($($sel_tok)+),
                                __tramp as *const ::core::ffi::c_void,
                                &$crate::method_encoding!(
                                    $crate::__aterm_objc_ret!($($ret)?) ; $($argty),*
                                ),
                            );
                        }
                    })*

                    __b.register()
                })
            }

            /// This instance as a raw `id`, borrowed.
            #[inline]
            #[allow(dead_code, reason = "generated for every declared class; each class calls a subset")]
            $vis fn as_id(&self) -> $crate::Id {
                $crate::Id::from_ptr(::core::ptr::from_ref(self).cast_mut().cast())
            }

            /// The `objc_super` a `[super …]` send from this class needs.
            #[inline]
            #[allow(dead_code, reason = "generated for every declared class; each class calls a subset")]
            $vis fn super_receiver(&self) -> $crate::ObjcSuper {
                // SAFETY: `self` borrows a live instance and `Self::meta()` is
                // the registered class declaring this method: the right pair
                // for a `[super …]` send from here.
                unsafe { $crate::super_of(self.as_id(), Self::meta().class()) }
            }

            /// This instance's Rust ivars.
            #[inline]
            #[allow(dead_code, reason = "generated for every declared class; each class calls a subset")]
            $vis fn ivars(&self) -> &$ivars {
                let __meta = Self::meta();
                let __slot = ::core::ptr::with_exposed_provenance::<$crate::IvarSlot<$ivars>>(
                    ::core::ptr::from_ref(self)
                        .expose_provenance()
                        .wrapping_add_signed(__meta.ivar_offset()),
                );
                // SAFETY: `self` borrows a live instance, so the slot at the
                // registered offset is aligned, readable and live for the
                // borrow. `IvarSlot::get` checks it was written, in release too.
                unsafe { $crate::IvarSlot::get(__slot) }
            }

            /// `+alloc` and store `ivars`, returning the +1 but uninitialised
            /// instance: the first half of [`Self::alloc_init`], for a class
            /// whose designated initializer is not `-init` (e.g. `NSView`'s
            /// `-initWithFrame:`).
            ///
            /// # Safety
            ///
            /// The caller must send the result exactly one designated
            /// initializer and adopt that initializer's +1 result (or release
            /// the instance).
            #[allow(dead_code, reason = "generated for every declared class; each class calls a subset")]
            $vis unsafe fn alloc_ivars(
                _mtm: $crate::MainThread,
                ivars: $ivars,
            ) -> $crate::Id {
                let __meta = Self::meta();
                // SAFETY: `+alloc` on a registered class returns a +1 zeroed instance or nil, so the ivar slot is unwritten, as `IvarSlot::init` requires.
                unsafe {
                    let __alloc: unsafe extern "C-unwind" fn(
                        $crate::ClassPtr,
                        $crate::Sel,
                    ) -> $crate::Id = $crate::checked_msg();
                    let __raw = __alloc(__meta.class(), $crate::sel!(alloc));
                    if __raw.is_null() {
                        return $crate::Id::NIL;
                    }
                    let __slot = ::core::ptr::with_exposed_provenance_mut::<
                        $crate::IvarSlot<$ivars>,
                    >(
                        __raw.expose_provenance().wrapping_add_signed(__meta.ivar_offset())
                    );
                    $crate::IvarSlot::init(__slot, ivars);
                    __raw
                }
            }

            /// `+alloc`, store `ivars`, `-init`. `None` if either returns nil.
            #[allow(dead_code, reason = "generated for every declared class; each class calls a subset")]
            $vis fn alloc_init(
                mtm: $crate::MainThread,
                ivars: $ivars,
            ) -> ::core::option::Option<$crate::Retained<Self>> {
                // SAFETY: `alloc_ivars` returns a +1 instance owing one
                // designated initializer, and `-init` is NSObject's, right for
                // any class whose chain does not require a stricter one. It
                // consumes the +1 and returns +1 or nil (having released it,
                // so `dealloc` already dropped the ivars).
                unsafe {
                    let __raw = Self::alloc_ivars(mtm, ivars);
                    if __raw.is_null() {
                        return ::core::option::Option::None;
                    }
                    let __init: unsafe extern "C-unwind" fn(
                        $crate::Id,
                        $crate::Sel,
                    ) -> $crate::Id = $crate::checked_msg();
                    let __obj = __init(__raw, $crate::sel!(init));
                    $crate::Retained::from_owned(__obj)
                }
            }

            $(
                $(#[$mmeta])*
                #[allow(non_snake_case, dead_code, clippy::too_many_arguments)]
                fn $method(& $slf $(, $arg: $argty)*) $(-> $ret)? $body
            )*
        }
    };
}
