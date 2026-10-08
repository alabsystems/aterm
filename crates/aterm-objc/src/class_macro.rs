// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `declare_class!` macro itself.

/// Build a `&'static CStr` from tokens, checked at compile time.
///
/// `cstr!("NSObject")` and `cstr!(stringify!(NSObject))` both work; an interior
/// NUL is a compile error.
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

/// A selector's token stream as the one string the runtime knows it by —
/// `@sel(toolbar:itemForItemIdentifier:)` becomes
/// `"toolbar:itemForItemIdentifier:"`, spelled as [`crate::sel!`] spells it —
/// so a containment report names the selector, not the Rust `fn`.
#[doc(hidden)]
#[macro_export]
macro_rules! __aterm_objc_sel_str {
    ($($tok:tt)+) => {
        ::core::concat!($(::core::stringify!($tok)),+)
    };
}

/// The body of a declared method's trampoline, by containment policy, chosen by
/// the marker after `@sel(…)`:
///
/// * none (default): the call runs under [`crate::exception::contain`]; an
///   `NSException` is reported against the selector and the trampoline returns
///   [`crate::exception::inert`], the all-zero value. The return type must be
///   [`crate::InertZero`].
/// * `@abort_on_exception`: the call runs bare, so an `NSException` reaches the
///   outer `catch_unwind` and the process aborts. For selectors whose zero is
///   a real answer, or return types with no inert zero.
///
/// Any other marker is a compile error naming the two that exist.
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

/// Count the `:` tokens in a selector's token stream.
///
/// A selector's arity is its colon count, while the runtime reads the argument
/// list from the encoding; [`declare_class!`] reconciles the two in a `const`
/// block, so `@sel(noArgsAtAll)` over `fn(&self, n: i64)` does not register
/// `"v@:q"`.
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
/// aterm's replacement for objc2's `declare_class!`, shaped by the declared
/// classes in `aterm-gui` and `vendor/winit` (see [`crate::declare`]).
///
/// ```ignore
/// aterm_objc::declare_class! {
///     /// The notification-observer target.
///     pub(crate) struct ReduceMotionTarget: NSObject {
///         const NAME: &str = "ATermReduceMotionTarget";
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
/// # What it generates
///
/// * An opaque, zero-sized, `!Send`/`!Sync` marker type named `$name`, reached
///   only through a reference to a live instance.
/// * `$name::class()` — registers the pair once per process (`OnceLock`) and
///   returns it.
/// * `$name::alloc_init(mtm, ivars)` — `+alloc`, store the ivars, `-init`,
///   against a [`crate::MainThread`] witness. Ivars are stored before `init`
///   because a superclass `init` may call overridden methods that read
///   `ivars()` (`NSView -initWithFrame:` can send `updateTrackingAreas`).
/// * `$name::ivars()` — `&Ivars`, at the registered ivar offset.
/// * `-dealloc` — drops the Rust ivars, then `[super dealloc]`.
/// * One `extern "C"` trampoline per `@sel(…)` method, with the correct type
///   encoding and a panic guard (a Rust panic aborts with the method's name).
///   An `NSException` raised inside is contained through [`crate::exception`]:
///   Rust `Drop`s on the unwound frames run and the method returns the inert
///   zero of its return type. `@abort_on_exception` after `@sel(…)` keeps the
///   abort; `-dealloc` always does.
///
/// # Differences from objc2
///
/// * `@sel(name:)`, not `#[method(name:)]`: in `macro_rules!`,
///   `$(#[$m:meta])* #[sel(…)]` is ambiguous because `sel(bump)` is itself a
///   valid `meta`. A leading `@` cannot start an attribute. Selector spelling
///   is otherwise objc2's.
/// * No `type Mutability`. `Send`/`Sync` are structural (the marker is
///   `!Send`) and interior mutability lives in the ivar type (`Cell`,
///   `RefCell`). The main-thread gate it provided is the [`crate::MainThread`]
///   parameter of `alloc_init`/`alloc_ivars`; instantiating without one does
///   not compile:
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
/// * A written initialised flag — see [`crate::IvarSlot`].
///
/// # Safety
///
/// What the macro cannot check and the caller owes:
///
/// * `$super` must be linked into the binary and a legal superclass to add
///   ivars to (every non-fragile ObjC class is).
/// * A method's argument types must be the ones the framework actually passes
///   (the mirror of [`crate::msg`]'s obligation).
/// * `NAME` must be unique in the process; registration panics otherwise.
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
/// Fourteen arguments compile, register and send (`tests/adversary_w2c.rs`).
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
            /// this type is a zero-sized marker AT that address, never a value.
            _opaque: [u8; 0],
            /// Makes the type `!Send`/`!Sync`: every declared class in aterm is
            /// main-thread AppKit state.
            _not_thread_safe: ::core::marker::PhantomData<*const u8>,
        }

        // SAFETY: `$name` is a zero-sized marker reached only through a
        // reference to a live instance of the class `class()` registers, once,
        // before any instance can exist.
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
                    // The instance base is only 16-aligned, so the ivar slot
                    // cannot be more (`S4`). `add_rust_ivar` asserts the same;
                    // this copy makes the error name the class.
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

                    // -dealloc: drop the Rust ivars, then [super dealloc].
                    // Not contained: an `NSException` here aborts, since a
                    // half-deallocated object is no state to continue from.
                    // The super send is `"C-unwind"`, so frames above it still
                    // run their drops.
                    {
                        unsafe extern "C" fn __dealloc(__this: $crate::Id, _cmd: $crate::Sel) {
                            let __guard = ::std::panic::catch_unwind(
                                ::std::panic::AssertUnwindSafe(|| {
                                    // `get()`, not `$name::meta()`: -dealloc
                                    // runs only on an instance, which exists
                                    // only after `class()` returned, so the
                                    // cell is set. A non-blocking peek also
                                    // satisfies the lazy-init reentrancy
                                    // census, which reads any call inside the
                                    // initializer as re-entry, and can never
                                    // park a -dealloc on the main thread.
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
                                    // SAFETY: `__this` is being deallocated,
                                    // so the slot is live, writable and
                                    // unaliased; `dispose` tolerates a
                                    // never-written slot.
                                    unsafe { $crate::IvarSlot::<$ivars>::dispose(__slot) };
                                    // SAFETY: `__this` is being deallocated
                                    // and `__meta.class()` defines this
                                    // `dealloc`, so the super send starts at
                                    // its superclass.
                                    unsafe {
                                        $crate::send_super_dealloc(__this, __meta.class());
                                    }
                                }),
                            );
                            if __guard.is_err() {
                                $crate::abort_on_unwind("dealloc");
                            }
                        }
                        // SAFETY: `__dealloc` is `extern "C"` with the exact
                        // `-(void)dealloc` prototype `(id, SEL)`, matches its
                        // encoding, and cannot unwind (the guard aborts).
                        unsafe {
                            __b.add_method(
                                $crate::sel!(dealloc),
                                __dealloc as *const ::core::ffi::c_void,
                                &$crate::method_encoding!(()),
                            );
                        }
                    }

                    $({
                        // The selector's colon count is its arity, and the
                        // encoding below comes from the Rust argument types:
                        // reconcile them at compile time. Also cap the arity at
                        // `MsgFn`'s sixteen parameters, so every registered
                        // method is sendable from Rust.
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
                        // `catch_unwind` outside, Objective-C `@try` inside. A
                        // Rust panic is not swallowed by `@catch (id)`: it runs
                        // its drops and aborts at the outer guard with its
                        // message. An `NSException` is caught by the inner
                        // wrapper after every Rust `Drop` between the raise and
                        // here has run, and the method returns its inert zero
                        // (under `@abort_on_exception` it reaches the outer
                        // guard and aborts). See `crate::exception`.
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
                        // SAFETY: `__tramp` is `extern "C"` with the
                        // `(id, SEL, ..)` prototype the runtime calls, its
                        // encoding comes from the same Rust types, and it cannot
                        // unwind (panics abort; NSExceptions are contained).
                        unsafe {
                            __b.add_method(
                                $crate::sel!($($sel_tok)+),
                                __tramp as *const ::core::ffi::c_void,
                                // One semicolon, then a comma-separated list,
                                // as `method_encoding!` expects.
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
            /// instance — the first half of [`Self::alloc_init`], for a class
            /// whose designated initializer is not `-init` (e.g. `NSView`'s
            /// `-initWithFrame:`).
            ///
            /// Ivars are stored before the initializer runs, as in
            /// `alloc_init`. `_mtm` is unused at run time; it makes a missing
            /// main-thread check a type error (see [`crate::MainThread`]).
            ///
            /// # Safety
            ///
            /// The returned reference is +1 and not initialised. The caller must
            /// send it exactly one designated initializer and adopt that
            /// initializer's +1 result (or release the instance). Prefer
            /// [`Self::alloc_init`] when `-init` is the right initializer.
            #[allow(dead_code, reason = "generated for every declared class; each class calls a subset")]
            $vis unsafe fn alloc_ivars(
                _mtm: $crate::MainThread,
                ivars: $ivars,
            ) -> $crate::Id {
                let __meta = Self::meta();
                // SAFETY: `+alloc` on a registered class returns a +1 zeroed
                // instance or nil, so the ivar slot is unwritten, as
                // `IvarSlot::init` requires.
                unsafe {
                    let __alloc: unsafe extern "C-unwind" fn(
                        $crate::ClassPtr,
                        $crate::Sel,
                    ) -> $crate::Id = $crate::msg();
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
            /// `mtm` is the [`crate::MainThread`] witness every instantiation
            /// owes.
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
                    ) -> $crate::Id = $crate::msg();
                    let __obj = __init(__raw, $crate::sel!(init));
                    $crate::Retained::from_owned(__obj)
                }
            }

            $(
                $(#[$mmeta])*
                // `non_snake_case`: methods are named for their selectors;
                // `dead_code`: the runtime is the caller; `too_many_arguments`:
                // the arity is the selector's, and the lint fires inside this
                // expansion where a call-site `#[allow]` cannot reach it.
                #[allow(non_snake_case, dead_code, clippy::too_many_arguments)]
                fn $method(& $slf $(, $arg: $argty)*) $(-> $ret)? $body
            )*
        }
    };
}
