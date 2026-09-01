// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `declare_class!` macro itself.

/// Build a `&'static CStr` from tokens, checked at compile time.
///
/// `cstr!("NSObject")` and `cstr!(stringify!(NSObject))` both work; a name with
/// an interior NUL is a compile error, which is the same property
/// `metal/ffi.rs` gets from taking `c"…"` literals.
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

/// Count the `:` tokens in a selector's token stream.
///
/// The arity of an Objective-C selector IS its colon count, and the runtime
/// reads the argument list from the type ENCODING beside it — two facts that a
/// macro can be made to reconcile, and that [`declare_class!`] does reconcile
/// in a `const` block. Without it `@sel(noArgsAtAll)` over `fn(&self, n: i64)`
/// compiles clean and registers `"v@:q"`: a one-argument encoding on a
/// colon-free selector, which AppKit will happily read arity from.
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
/// This is aterm's replacement for objc2's `declare_class!`, shaped by what the
/// seven real sites in `aterm-gui` do rather than by objc2's surface — see
/// [`crate::declare`] for that inventory.
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
/// * An opaque, zero-sized, `!Send`/`!Sync` marker type named `$name`, which is
///   only ever reached through a reference to a live instance.
/// * `$name::class()` — registers the pair on first call (`OnceLock`, so exactly
///   once per process) and returns the class.
/// * `$name::alloc_init(ivars)` — `+alloc`, store the ivars, `-init`. The ivars
///   are stored BEFORE `init` runs, deliberately: a superclass `init` may send
///   overridden methods back to the instance (`NSView -initWithFrame:` calls
///   `updateTrackingAreas` on some paths), and those methods read `ivars()`.
/// * `$name::ivars()` — `&Ivars`, at the registered ivar offset.
/// * `-dealloc` — drops the Rust ivars, then `[super dealloc]`. Generated
///   unconditionally, because that is the ONLY thing any of the seven real
///   sites needs from `dealloc`.
/// * One `extern "C"` trampoline per `@sel(…)` method, each with the correct
///   Objective-C type encoding and each wrapped in a panic guard, because
///   unwinding out of an ObjC frame is undefined behaviour.
///
/// # Divergences from objc2, and why
///
/// * **`@sel(name:)` not `#[method(name:)]`** — FORCED, not chosen. objc2 reads
///   its attribute with a proc macro; a `macro_rules!` arm that matches
///   `$(#[$m:meta])* #[sel(…)]` is locally ambiguous, because `sel(bump)` is
///   itself a valid `meta` and the parser cannot tell the last doc comment from
///   the selector. A leading `@` cannot start an attribute, so the ambiguity
///   disappears and doc comments keep their natural position above the method.
///   The selector spelling itself is unchanged — same `stringify!`-per-token
///   trick, so `@sel(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:)`
///   is written exactly as objc2 writes it, and — since the arity fix below —
///   means what it says at three arguments as well as one.
/// * **No `type Mutability`** — objc2 uses it to drive `Send`/`Sync` and its
///   `&mut` story. Every class in the tree is main-thread AppKit state and
///   [`crate::Retained`] is unconditionally `!Send`, so the parameter would
///   have exactly one useful value. Interior mutability is spelled the Rust
///   way, in the ivar type (`Cell`, `RefCell`), which is what all seven sites
///   already do.
/// * **One ivar, not a struct of them** — see [`crate::declare`].
/// * **`&self` is captured as an `ident`, not matched literally** — macro
///   hygiene: a `self` token emitted from the macro's own definition lives in
///   the macro's syntax context, so a body written at the call site could not
///   name it (`error[E0424]: expected value, found module \`self\``). Capturing
///   the receiver as `$slf:ident` carries the call site's own `self` through.
/// * **A written initialised flag** — see [`crate::IvarSlot`].
///
/// # Safety
///
/// Declaring a class is unsafe work the macro does on your behalf; what it
/// CANNOT check, and what the caller therefore owes:
///
/// * `$super` must name a class that is linked into the binary, and must be a
///   legal superclass to add ivars to (every non-fragile ObjC class is).
/// * ~~A `@sel(…)` name must match its Rust signature … the macro cannot count
///   the colons for you.~~ IT NOW DOES: the expansion carries a `const` block
///   asserting that the selector's colon count equals the number of declared
///   arguments, so `@sel(noArgsAtAll)` over `fn(&self, n: i64)` — which used to
///   compile and register the one-argument encoding `"v@:q"` against a
///   colon-free selector — is a compile error naming both halves. The check
///   lives in the same expansion as the encoding it guards.
/// * A method's argument types must be the ones the framework actually passes.
///   This is the same obligation [`crate::msg`] imposes in the other direction.
/// * `NAME` must be globally unique in the process. Registration panics rather
///   than silently returning nil if it is not.
///
/// # The colon count is CHECKED, and more than one argument works
///
/// The two properties are one expansion, which is why they are one section.
/// An arity disagreement is a compile error naming both halves:
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
/// The identical class with the colon its argument requires compiles, and so
/// do two and three arguments — the shapes that used to fail with
/// `error: no rules expected ';'` before the encoding call was fixed:
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

        // SAFETY: `$name` is a zero-sized opaque marker that is only ever
        // reached through a reference to a live instance of the class
        // `class()` registers, and `class()` registers exactly that class,
        // exactly once, before any instance can exist.
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
                    __b.add_rust_ivar::<$ivars>();
                    $($( __b.add_protocol($crate::cstr!(::core::stringify!($proto))); )*)?

                    // -dealloc: drop the Rust ivars, then [super dealloc].
                    {
                        unsafe extern "C" fn __dealloc(__this: $crate::Id, _cmd: $crate::Sel) {
                            let __guard = ::std::panic::catch_unwind(
                                ::std::panic::AssertUnwindSafe(|| {
                                    let __meta = $name::meta();
                                    let __slot = ::core::ptr::with_exposed_provenance_mut::<
                                        $crate::IvarSlot<$ivars>,
                                    >(
                                        __this
                                            .expose_provenance()
                                            .wrapping_add_signed(__meta.ivar_offset()),
                                    );
                                    // SAFETY: `__this` is the instance the
                                    // runtime is deallocating, so the slot is
                                    // live, writable and unaliased; `dispose`
                                    // itself tolerates a never-written slot.
                                    unsafe { $crate::IvarSlot::<$ivars>::dispose(__slot) };
                                    // SAFETY: `__this` is the instance being
                                    // deallocated and `__meta.class()` is the
                                    // class that DEFINES this `dealloc`, so the
                                    // super send starts at the right place.
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
                        // `-(void)dealloc` prototype `(id, SEL)`, matches the
                        // encoding passed beside it, and cannot unwind (the
                        // guard above aborts instead).
                        unsafe {
                            __b.add_method(
                                $crate::sel!(dealloc),
                                __dealloc as *const ::core::ffi::c_void,
                                &$crate::method_encoding!(()),
                            );
                        }
                    }

                    $({
                        // The selector's colon count IS its arity, and the
                        // encoding below is written from the Rust argument
                        // types. Reconcile the two HERE, at compile time,
                        // rather than letting AppKit discover the disagreement
                        // through `NSMethodSignature` at run time.
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
                                    // only to a live instance of the class that
                                    // declares the method or of a SUBCLASS of
                                    // it — never to nil, which short-circuits
                                    // before any IMP runs — so `__this` is
                                    // non-null and addresses one. A subclass
                                    // instance is equally fine here: ivars sit
                                    // at the offset the runtime assigned to
                                    // THIS class, which subclassing does not
                                    // move. `$name` is a zero-sized marker, so
                                    // the reference borrows no bytes of its own.
                                    let __self: &$name =
                                        unsafe { &*__this.cast::<$name>().cast_const() };
                                    $name::$method(__self $(, $arg)*)
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
                        // encoding is derived from those same Rust types, and
                        // it cannot unwind (the guard above aborts instead).
                        unsafe {
                            __b.add_method(
                                $crate::sel!($($sel_tok)+),
                                __tramp as *const ::core::ffi::c_void,
                                // ONE semicolon, then a COMMA-separated list.
                                // This used to be `$(; $argty)*`, which emits
                                // `Ret ; A ; B` at arity 2 and does not match
                                // `method_encoding!`'s pattern at all: every
                                // two-argument class failed with
                                // `error: no rules expected ';'` pointing at
                                // the whole invocation. Nothing in the crate
                                // took more than one argument, so it shipped.
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
            #[allow(dead_code)]
            $vis fn as_id(&self) -> $crate::Id {
                ::core::ptr::from_ref(self).cast_mut().cast()
            }

            /// The `objc_super` a `[super …]` send from THIS class needs.
            #[inline]
            #[allow(dead_code)]
            $vis fn super_receiver(&self) -> $crate::ObjcSuper {
                // SAFETY: `self` borrows a live instance and `Self::meta()`
                // returns the registered class that declares this method, so
                // both arguments are live and the pair is the correct one for
                // a `[super …]` send from here.
                unsafe { $crate::super_of(self.as_id(), Self::meta().class()) }
            }

            /// This instance's Rust ivars.
            #[inline]
            #[allow(dead_code)]
            $vis fn ivars(&self) -> &$ivars {
                let __meta = Self::meta();
                let __slot = ::core::ptr::with_exposed_provenance::<$crate::IvarSlot<$ivars>>(
                    ::core::ptr::from_ref(self)
                        .expose_provenance()
                        .wrapping_add_signed(__meta.ivar_offset()),
                );
                // SAFETY: `self` borrows a live instance, so the slot at the
                // registered offset is aligned, readable and live for the
                // borrow. Whether it was WRITTEN is checked by `IvarSlot::get`
                // itself, in release builds too — see the note there on
                // instances Objective-C code creates without `alloc_init`.
                unsafe { $crate::IvarSlot::get(__slot) }
            }

            /// `+alloc`, store `ivars`, `-init`. `None` if either returns nil.
            #[allow(dead_code)]
            $vis fn alloc_init(ivars: $ivars) -> ::core::option::Option<$crate::Retained<Self>> {
                let __meta = Self::meta();
                // SAFETY: `+alloc` on a registered class returns a +1,
                // zero-filled instance or nil; the ivar slot inside it is
                // therefore unwritten, which is exactly `IvarSlot::init`'s
                // precondition. `-init` then consumes that +1 and returns the
                // initialised +1 (or nil, having released it, in which case
                // `dealloc` already dropped the ivars).
                unsafe {
                    let __alloc: unsafe extern "C" fn(
                        $crate::ClassPtr,
                        $crate::Sel,
                    ) -> $crate::Id = $crate::msg();
                    let __raw = __alloc(__meta.class(), $crate::sel!(alloc));
                    if __raw.is_null() {
                        return ::core::option::Option::None;
                    }
                    let __slot = ::core::ptr::with_exposed_provenance_mut::<
                        $crate::IvarSlot<$ivars>,
                    >(
                        __raw.expose_provenance().wrapping_add_signed(__meta.ivar_offset())
                    );
                    $crate::IvarSlot::init(__slot, ivars);
                    let __init: unsafe extern "C" fn(
                        $crate::Id,
                        $crate::Sel,
                    ) -> $crate::Id = $crate::msg();
                    let __obj = __init(__raw, $crate::sel!(init));
                    $crate::Retained::from_owned(__obj)
                }
            }

            $(
                $(#[$mmeta])*
                #[allow(non_snake_case, dead_code)]
                fn $method(& $slf $(, $arg: $argty)*) $(-> $ret)? $body
            )*
        }
    };
}
