// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE ORACLE: every `cfg_if!` invocation SHAPE in aterm's dependency graph,
//! copied from the consumer that writes it.
//!
//! # Why this file exists
//!
//! Twenty-three third-party packages reach this macro through
//! `[patch.crates-io]`, and almost none of them can be compiled on the machine
//! that develops it. `getrandom 0.3.4`, `libloading`, `polling`, `async-io`,
//! `async-process`, `async-signal`, `ahash` and `tiny-skia` are Linux-only
//! edges; `console_error_panic_hook`, `wasm-bindgen` and `wasm-bindgen-futures`
//! are wasm-only; `nix`'s and `crc32fast`'s call sites arrive on dev edges that
//! a plain `cargo build` never touches. "The build is green" therefore proves
//! almost nothing about this macro's surface — it exercises the seven mac-arm
//! consumers and stops.
//!
//! So the proof moves to a place that *does* compile here. Every invocation
//! below carries a comment naming the crate, file and line it came from.
//! **Compiling this file is the proof that the macro accepts what the real
//! consumers write.** When a consumer bumps and introduces a form this macro
//! cannot parse, the fix is to paste the new invocation in here first and watch
//! it fail.
//!
//! # What is verbatim and what is not — read this before trusting the file
//!
//! Three things are ALWAYS verbatim, because they are what the macro actually
//! sees: the **invocation syntax** (path, delimiter, trailing semicolon), the
//! **`#[cfg]` predicates**, and the **position** the invocation sits in (item,
//! statement, tail expression, associated item, inside another `macro_rules!`).
//!
//! Arm BODIES are verbatim wherever a body is itself the thing under test —
//! multi-item arms, `extern "C" { }` plus a following `use`, `#[path] mod imp;`,
//! `macro_rules!` inside an arm, a `let` that has to escape the invocation. A
//! body is reduced only when reproducing it would mean vendoring the
//! consumer's internals (`ring`'s `sha2_32_ffi!` and `cpu::` feature machinery,
//! `tiny-skia`'s SIMD wrappers). Every reduction is labelled `BODY REDUCED` at
//! the site, with what was kept and what was dropped. Nothing is silently
//! trimmed.
//!
//! Two other deliberate departures, both about the *host* rather than the
//! macro:
//!
//! * Where a consumer writes `mod foo;`, this file provides the real file under
//!   `tests/consumer_forms/`, so the file-backed `mod` and `#[path = "…"]`
//!   forms are exercised as written rather than rewritten to inline modules.
//!   The exception is `getrandom`'s backend chain, whose twenty-eight arms
//!   would need twenty-eight files; there the `mod X;` items become
//!   `mod X { … }`, labelled at the site.
//! * `#[allow(unsafe_op_in_unsafe_fn)]` appears on two modules. The consumers
//!   compile under their own lint config; this workspace denies that lint
//!   crate-wide. The allow keeps their tokens verbatim instead of editing them
//!   to satisfy a lint that has nothing to do with the macro.
//!
//! # The runtime half
//!
//! Accepting the syntax is only half the contract; the other half is selecting
//! the RIGHT arm, and that is invisible to a compiler. Every case that can
//! observe its own selection does: the `parking_lot_core` chain reports which
//! parker file it pulled in, `crc32fast` reports which `State` it re-exported,
//! `getrandom`'s backend chain reports its backend — and each is asserted
//! against a hand-written `cfg!()` cascade over the identical predicates. On
//! this host those cascades are the ones that matter; `tests/differential.rs`
//! is where the host-independent overlap cases live.

// Verbatim consumer predicates name cfgs those consumers' build scripts set —
// `getrandom_backend`, `supports_ptr_atomics`, `freebsdlike`, `linux_android`,
// `solarish`, `stable_arm_crc32_intrinsics`, `libloading_docs`, `std` — and
// features those consumers declare. None of them exist for THIS package, and
// editing them out would destroy the only thing under test. So the lint is
// allowed here and nowhere else in the crate.
#![allow(unexpected_cfgs)]
// The scaffolding (stub `libc`, stub `cpu`, imports a non-selected arm would
// have used) is deliberately larger than what any one cell consumes.
#![allow(dead_code, unused_imports, unused_variables)]
// The stub `libc` reproduces C's own spellings (`c_int`, `ssize_t`,
// `_Errno`) so the consumers' `use libc::…` lines need no editing.
#![allow(non_snake_case, non_camel_case_types)]

// -------------------------------------------------------------------------
// SHAPE 21 — `#[macro_use] extern crate cfg_if;`
//
// getrandom-0.2.17/src/lib.rs:213-214, and identically getrandom-0.3.4
// lib.rs:30-31, getrandom-0.4.2 lib.rs:15-16, console_error_panic_hook
// lib.rs:73-74 and nix's test/test.rs:1-2.
//
// This is why the macro must be a real `#[macro_export] macro_rules!` at the
// crate root: a `pub use` re-export of a macro defined in a submodule is NOT
// picked up by `#[macro_use] extern crate`, and six consumers would break.
// It is also what puts the bare `cfg_if!` name in textual scope for the rest of
// this file, which every "SHAPE 1" style invocation below depends on.
// -------------------------------------------------------------------------
#[macro_use]
extern crate cfg_if;

// -------------------------------------------------------------------------
// SCAFFOLDING. Not part of any consumer; it exists so their tokens can be
// copied without also copying their crates.
// -------------------------------------------------------------------------

/// Stand-in for the `libc` crate, so `getrandom`'s `util_libc.rs` arms can be
/// copied verbatim. Every per-platform `errno` entry point in that chain is
/// declared, not just this host's, so the chain compiles wherever it is run.
mod libc {
    #![allow(clippy::missing_safety_doc)]

    pub type c_int = i32;
    pub type ssize_t = isize;

    /// Backing storage so the stub entry points can hand out a real pointer.
    static mut ERRNO: c_int = 0;

    macro_rules! errno_entry_point {
        ($($name:ident),* $(,)?) => {$(
            pub unsafe fn $name() -> *mut c_int {
                &raw mut ERRNO
            }
        )*};
    }

    errno_entry_point!(
        __errno,
        __errno_location,
        ___errno,
        __error,
        _errnop,
        __get_errno_ptr,
        _Errno,
    );

    pub unsafe fn errnoGet() -> c_int {
        0
    }
}

/// Stand-in for `ring`'s `cpu` module, so its `use cpu::GetFeature as _;` arm
/// can be copied verbatim.
mod cpu {
    /// The trait `ring`'s arm imports anonymously.
    pub trait GetFeature<T> {
        /// Detect a feature, or not.
        fn get_feature(&self) -> Option<T>;
    }
}

// -------------------------------------------------------------------------
// SHAPES 1, 6, 15 — bare `cfg_if! {` at item position; an eight-arm else-if
// chain with NO final else; an arm holding an `extern "C" { }` block followed
// by a `use`.
//
// getrandom-0.2.17/src/util_libc.rs:11-33, verbatim (bodies included).
//
// The `extern "C" { }` arm is worth its own note. Written as it stands it is
// not valid edition-2024 Rust — 2024 requires `unsafe extern "C"` — and that is
// precisely the point: the arm is never SELECTED on any of aterm's cells, so it
// is never expanded, so its contents never reach the parser. What the macro
// still has to do is MATCH it, because matching happens before any `#[cfg]` is
// evaluated. If the body matcher were `item` or `stmt` instead of `tt`, this
// invocation would fail at the match, on every cell, with no arm selected.
// -------------------------------------------------------------------------
#[allow(unsafe_op_in_unsafe_fn)]
mod getrandom_0_2_util_libc {
    use crate::libc;

    cfg_if! {
        if #[cfg(any(target_os = "netbsd", target_os = "openbsd", target_os = "android", target_os = "cygwin"))] {
            use libc::__errno as errno_location;
        } else if #[cfg(any(target_os = "linux", target_os = "emscripten", target_os = "hurd", target_os = "redox", target_os = "dragonfly"))] {
            use libc::__errno_location as errno_location;
        } else if #[cfg(any(target_os = "solaris", target_os = "illumos"))] {
            use libc::___errno as errno_location;
        } else if #[cfg(any(target_os = "macos", target_os = "freebsd"))] {
            use libc::__error as errno_location;
        } else if #[cfg(target_os = "haiku")] {
            use libc::_errnop as errno_location;
        } else if #[cfg(target_os = "nto")] {
            use libc::__get_errno_ptr as errno_location;
        } else if #[cfg(any(all(target_os = "horizon", target_arch = "arm"), target_os = "vita"))] {
            extern "C" {
                // Not provided by libc: https://github.com/rust-lang/libc/issues/1995
                fn __errno() -> *mut libc::c_int;
            }
            use __errno as errno_location;
        } else if #[cfg(target_os = "aix")] {
            use libc::_Errno as errno_location;
        }
    }

    // getrandom-0.2.17/src/util_libc.rs:35-40 — a two-arm chain whose `else`
    // defines a function rather than importing one.
    cfg_if! {
        if #[cfg(target_os = "vxworks")] {
            use libc::errnoGet as get_errno;
        } else {
            unsafe fn get_errno() -> libc::c_int { *errno_location() }
        }
    }

    /// Proof that exactly one `get_errno` exists and is callable: a duplicate
    /// or a missing one would not compile.
    pub fn errno_is_reachable() -> libc::c_int {
        unsafe { get_errno() }
    }
}

// -------------------------------------------------------------------------
// SHAPES 2, 16 — qualified `cfg_if::cfg_if! {`, and multi-item arms whose
// items are a file-backed `mod` plus a `pub use` of something inside it.
//
// crc32fast-1.5.0/src/specialized/mod.rs:1-35, verbatim. The two `mod` files
// are real, under tests/consumer_forms/crc32fast_specialized/.
//
// THIS IS THE ISSUE-#90 SHAPE IN THE WILD. If the `#[cfg]` bound only to
// `mod pclmulqdq;`, the `pub use self::pclmulqdq::State;` after it would compile
// on every target and re-export a module that the cfg was supposed to exclude.
// -------------------------------------------------------------------------
// SCAFFOLDING ATTRIBUTE, not part of the consumer: `#[path]` on an INLINE
// module sets the directory its child modules resolve in, which keeps this
// test's stand-in files under tests/consumer_forms/ instead of loose in
// tests/. It changes nothing the macro sees.
#[path = "consumer_forms/crc32fast_specialized"]
mod crc32fast_specialized {
    cfg_if::cfg_if! {
        if #[cfg(all(
            target_feature = "sse2",
            any(target_arch = "x86", target_arch = "x86_64")
        ))] {
            mod pclmulqdq;
            pub use self::pclmulqdq::State;
        } else if #[cfg(all(stable_arm_crc32_intrinsics, target_arch = "aarch64"))] {
            mod aarch64;
            pub use self::aarch64::State;
        } else {
            #[derive(Clone)]
            pub enum State {}
            impl State {
                pub fn new(_: u32) -> Option<Self> {
                    None
                }

                pub fn update(&mut self, _buf: &[u8]) {
                    match *self {}
                }

                pub fn finalize(self) -> u32 {
                    match self{}
                }

                pub fn reset(&mut self) {
                    match *self {}
                }

                pub fn combine(&mut self, _other: u32, _amount: u64) {
                    match *self {}
                }
            }
        }
    }

    /// Which arm won, observable at run time. The `else` arm's `State` is an
    /// empty enum with no `origin`, so this mirrors the selection exactly.
    pub fn selected() -> &'static str {
        #[cfg(all(
            target_feature = "sse2",
            any(target_arch = "x86", target_arch = "x86_64")
        ))]
        {
            State::origin()
        }
        #[cfg(not(all(
            target_feature = "sse2",
            any(target_arch = "x86", target_arch = "x86_64")
        )))]
        {
            "fallback-enum"
        }
    }
}

// -------------------------------------------------------------------------
// SHAPE 3 — PARENTHESIS delimiters at item position with a trailing semicolon:
// `cfg_if::cfg_if!( … );`
//
// getrandom-0.3.4/src/error.rs:8-19, verbatim. Only two sites in the whole
// graph use parens (this one and getrandom-0.4.2's), and both add the
// semicolon. A `macro_rules!` takes all three delimiters for free; a proc macro
// or a delimiter-specific wrapper would not.
// -------------------------------------------------------------------------
mod getrandom_0_3_error {
    cfg_if::cfg_if!(
        if #[cfg(target_os = "uefi")] {
            // See the UEFI spec for more information:
            // https://uefi.org/specs/UEFI/2.10/Apx_D_Status_Codes.html
            type RawOsError = usize;
            type NonZeroRawOsError = core::num::NonZeroUsize;
            const UEFI_ERROR_FLAG: RawOsError = 1 << (RawOsError::BITS - 1);
        } else {
            type RawOsError = i32;
            type NonZeroRawOsError = core::num::NonZeroI32;
        }
    );

    /// The three-item UEFI arm and the two-item fallback both matter: this
    /// proves the selected arm contributed ALL of its items, not just the first.
    pub fn both_aliases_exist(v: RawOsError) -> Option<NonZeroRawOsError> {
        NonZeroRawOsError::new(v)
    }
}

// -------------------------------------------------------------------------
// SHAPE 3 again, with DOC COMMENTS inside the arms and a `pub` item.
//
// getrandom-0.4.2/src/error.rs:6-28, verbatim. Doc comments are `#[doc]`
// attributes by the time the macro sees them, so this is really "an arm
// beginning with an attribute" — twenty sites in the graph do it.
// -------------------------------------------------------------------------
mod getrandom_0_4_error {
    cfg_if::cfg_if!(
        if #[cfg(target_os = "uefi")] {
            // See the UEFI spec for more information:
            // https://uefi.org/specs/UEFI/2.10/Apx_D_Status_Codes.html

            /// Raw error code.
            ///
            /// This alias mirrors unstable [`std::io::RawOsError`].
            ///
            /// [`std::io::RawOsError`]: https://doc.rust-lang.org/std/io/type.RawOsError.html
            pub type RawOsError = usize;
            type NonZeroRawOsError = core::num::NonZeroUsize;
            const UEFI_ERROR_FLAG: RawOsError = 1 << (RawOsError::BITS - 1);
        } else {
            /// Raw error code.
            ///
            /// This alias mirrors unstable [`std::io::RawOsError`].
            ///
            /// [`std::io::RawOsError`]: https://doc.rust-lang.org/std/io/type.RawOsError.html
            pub type RawOsError = i32;
            type NonZeroRawOsError = core::num::NonZeroI32;
        }
    );
}

// -------------------------------------------------------------------------
// SHAPES 4, 10 — BRACES with a trailing semicolon in STATEMENT position, and a
// `let` binding that has to survive past the end of the invocation.
//
// naga-29.0.3/src/error.rs:110-126, verbatim except that `NoColor` and
// `alloc::vec::Vec` become `String` (naga's `termcolor` and `stderr` features
// are off in aterm's graph, so the arm this host selects is the third one,
// which is `String::new()` upstream too).
//
// `Self { inner }` on the line after the macro is the whole point: the binding
// is created inside the expansion and read outside it. That works only because
// the tokens are re-emitted in the CALLER's syntax context. A proc-macro
// implementation that re-spanned them would break this — and would break
// shadowing cases silently before breaking this one loudly.
// -------------------------------------------------------------------------
mod naga_error {
    pub struct DiagnosticBuffer {
        inner: String,
    }

    impl DiagnosticBuffer {
        pub fn new() -> Self {
            cfg_if::cfg_if! {
                if #[cfg(feature = "termcolor")] {
                    let inner = String::from("termcolor");
                } else if #[cfg(feature = "stderr")] {
                    let inner = String::from("stderr");
                } else {
                    let inner = String::new();
                }
            };

            Self { inner }
        }

        pub fn inner(&self) -> &str {
            &self.inner
        }
    }
}

// -------------------------------------------------------------------------
// SHAPES 4, 5, 11(no-else) — braces + semicolon in statement position, an
// if-ONLY chain, and an expansion that must be EMPTY when nothing matches.
//
// nix-0.29.0/src/sys/socket/addr.rs:583-594, verbatim (the `self.sun_len`
// assignment becomes a local field write on a stub struct).
//
// `Ok(())` follows the invocation. An expansion of `()` or `;` instead of
// nothing would either change the function's value or trip an
// unused-expression lint under this workspace's `-D warnings`.
// -------------------------------------------------------------------------
mod nix_addr {
    pub struct SocketAddressLengthNotDynamic;

    pub struct UnixAddr {
        pub sun_len: u8,
    }

    impl UnixAddr {
        pub fn set_length(
            &mut self,
            new_length: usize,
        ) -> std::result::Result<(), SocketAddressLengthNotDynamic> {
            // `new_length` is only used on some platforms, so it must be provided even when not used
            #![allow(unused_variables)]
            cfg_if! {
                if #[cfg(any(linux_android,
                             target_os = "fuchsia",
                             solarish,
                             target_os = "redox",
                    ))] {
                    self.sun_len = new_length as u8;
                }
            };
            Ok(())
        }
    }
}

// -------------------------------------------------------------------------
// SHAPE 5 — an if-ONLY chain at item position whose single arm holds a `use`
// AND a file-backed `mod`.
//
// ring-0.17.14/src/aead/chacha20_poly1305/mod.rs:24-32, verbatim. On mac-arm
// and on x86_64 the arm IS selected, so `mod integrated;` resolves to a real
// file under tests/consumer_forms/ring_chacha/.
// -------------------------------------------------------------------------
// SCAFFOLDING ATTRIBUTE, not part of the consumer: `#[path]` on an INLINE
// module sets the directory its child modules resolve in, which keeps this
// test's stand-in files under tests/consumer_forms/ instead of loose in
// tests/. It changes nothing the macro sees.
#[path = "consumer_forms/ring_chacha"]
mod ring_chacha {
    use crate::cpu;

    cfg_if! {
        if #[cfg(any(
                all(target_arch = "aarch64", target_endian = "little"),
                target_arch = "x86_64"))] {
            use cpu::GetFeature as _;
            mod integrated;
        }
    }
}

// -------------------------------------------------------------------------
// SHAPES 7, 8, 18 — the longest chain in the graph (27 else-if arms plus a
// final else), a cfg_if! NESTED inside an arm, and `compile_error!` parked in
// arms that must never fire.
//
// getrandom-0.3.4/src/backends.rs:10-209. Predicates, nesting, `compile_error!`
// bodies, `concat!`/`env!` and the inner `#[cfg]` on `mod sanitizer;` are all
// verbatim.
//
// BODY REDUCED, and this is the one place in the file where it is mechanical
// rather than judged: each `mod X;` becomes `mod X { pub const BACKEND: &str =
// "X"; }`. Twenty-eight file-backed modules would need twenty-eight files, and
// the substitution keeps every arm multi-item, keeps the `pub use X::*;` that
// follows it, and makes the SELECTED backend observable at run time — which the
// file-backed form would not.
//
// Three things this case proves that no shorter chain does:
//   * RECURSION DEPTH. getrandom sets no `#![recursion_limit]`, so this chain
//     runs on the default 128. It compiles here, on the same default.
//   * The nested `cfg_if!` at the `getrandom_backend = "wasm_js"` arm is
//     reached by MACRO_USE scope, from inside an expansion of the outer
//     invocation. A `pub use` re-export instead of `#[macro_export]` breaks
//     exactly this.
//   * Twenty `compile_error!`s sit in unselected arms across the graph; four
//     are here. `#[cfg]` prevents expansion rather than discarding output, so
//     none of them fires. The CONTROL for that claim — the same
//     `compile_error!` in a SELECTED arm really does fail the build — is the
//     `compile_fail` doctest on `cfg_if!` in src/lib.rs.
// -------------------------------------------------------------------------
mod getrandom_0_3_backends {
    cfg_if! {
        if #[cfg(getrandom_backend = "custom")] {
            mod custom { pub const BACKEND: &str = "custom"; }
            pub use custom::*;
        } else if #[cfg(getrandom_backend = "linux_getrandom")] {
            mod getrandom { pub const BACKEND: &str = "getrandom"; }
            mod sanitizer { pub const BACKEND: &str = "sanitizer"; }
            pub use getrandom::*;
        } else if #[cfg(getrandom_backend = "linux_raw")] {
            mod linux_raw { pub const BACKEND: &str = "linux_raw"; }
            mod sanitizer { pub const BACKEND: &str = "sanitizer"; }
            pub use linux_raw::*;
        } else if #[cfg(getrandom_backend = "rdrand")] {
            mod rdrand { pub const BACKEND: &str = "rdrand"; }
            pub use rdrand::*;
        } else if #[cfg(getrandom_backend = "rndr")] {
            mod rndr { pub const BACKEND: &str = "rndr"; }
            pub use rndr::*;
        } else if #[cfg(getrandom_backend = "efi_rng")] {
            mod efi_rng { pub const BACKEND: &str = "efi_rng"; }
            pub use efi_rng::*;
        } else if #[cfg(getrandom_backend = "windows_legacy")] {
            mod windows_legacy { pub const BACKEND: &str = "windows_legacy"; }
            pub use windows_legacy::*;
        } else if #[cfg(getrandom_backend = "wasm_js")] {
            cfg_if! {
                if #[cfg(feature = "wasm_js")] {
                    mod wasm_js { pub const BACKEND: &str = "wasm_js"; }
                    pub use wasm_js::*;
                } else {
                    compile_error!(concat!(
                        "The \"wasm_js\" backend requires the `wasm_js` feature \
                        for `getrandom`. For more information see: \
                        https://docs.rs/getrandom/", env!("CARGO_PKG_VERSION"), "/#webassembly-support"
                    ));
                }
            }
        } else if #[cfg(getrandom_backend = "unsupported")] {
            mod unsupported { pub const BACKEND: &str = "unsupported"; }
            pub use unsupported::*;
        } else if #[cfg(all(target_os = "linux", target_env = ""))] {
            mod linux_raw { pub const BACKEND: &str = "linux_raw"; }
            mod sanitizer { pub const BACKEND: &str = "sanitizer"; }
            pub use linux_raw::*;
        } else if #[cfg(target_os = "espidf")] {
            mod esp_idf { pub const BACKEND: &str = "esp_idf"; }
            pub use esp_idf::*;
        } else if #[cfg(any(
            target_os = "haiku",
            target_os = "redox",
            target_os = "nto",
            target_os = "aix",
        ))] {
            mod use_file { pub const BACKEND: &str = "use_file"; }
            pub use use_file::*;
        } else if #[cfg(any(
            target_os = "macos",
            target_os = "openbsd",
            target_os = "vita",
            target_os = "emscripten",
        ))] {
            mod getentropy { pub const BACKEND: &str = "getentropy"; }
            pub use getentropy::*;
        } else if #[cfg(any(
            // Rust supports Android API level 19 (KitKat) [0] and the next upgrade targets
            // level 21 (Lollipop) [1], while `getrandom(2)` was added only in
            // level 23 (Marshmallow). Note that it applies only to the "old" `target_arch`es,
            // RISC-V Android targets sufficiently new API level, same will apply for potential
            // new Android `target_arch`es.
            // [0]: https://blog.rust-lang.org/2023/01/09/android-ndk-update-r25.html
            // [1]: https://github.com/rust-lang/rust/pull/120593
            all(
                target_os = "android",
                any(
                    target_arch = "aarch64",
                    target_arch = "arm",
                    target_arch = "x86",
                    target_arch = "x86_64",
                ),
            ),
            // Only on these `target_arch`es Rust supports Linux kernel versions (3.2+)
            // that precede the version (3.17) in which `getrandom(2)` was added:
            // https://doc.rust-lang.org/stable/rustc/platform-support.html
            all(
                target_os = "linux",
                any(
                    target_arch = "aarch64",
                    target_arch = "arm",
                    target_arch = "powerpc",
                    target_arch = "powerpc64",
                    target_arch = "s390x",
                    target_arch = "x86",
                    target_arch = "x86_64",
                    // Minimum supported Linux kernel version for MUSL targets
                    // is not specified explicitly (as of Rust 1.77) and they
                    // are used in practice to target pre-3.17 kernels.
                    all(
                        target_env = "musl",
                        not(
                            any(
                                target_arch = "riscv64",
                                target_arch = "riscv32",
                            ),
                        ),
                    ),
                ),
            )
        ))] {
            mod use_file { pub const BACKEND: &str = "use_file"; }
            mod linux_android_with_fallback { pub const BACKEND: &str = "linux_android_with_fallback"; }
            mod sanitizer { pub const BACKEND: &str = "sanitizer"; }
            pub use linux_android_with_fallback::*;
        } else if #[cfg(any(
            target_os = "android",
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "hurd",
            target_os = "illumos",
            target_os = "cygwin",
            // Check for target_arch = "arm" to only include the 3DS. Does not
            // include the Nintendo Switch (which is target_arch = "aarch64").
            all(target_os = "horizon", target_arch = "arm"),
        ))] {
            mod getrandom { pub const BACKEND: &str = "getrandom"; }
            #[cfg(any(target_os = "android", target_os = "linux"))]
            mod sanitizer { pub const BACKEND: &str = "sanitizer"; }
            pub use getrandom::*;
        } else if #[cfg(target_os = "solaris")] {
            mod solaris { pub const BACKEND: &str = "solaris"; }
            pub use solaris::*;
        } else if #[cfg(target_os = "netbsd")] {
            mod netbsd { pub const BACKEND: &str = "netbsd"; }
            pub use netbsd::*;
        } else if #[cfg(target_os = "fuchsia")] {
            mod fuchsia { pub const BACKEND: &str = "fuchsia"; }
            pub use fuchsia::*;
        } else if #[cfg(any(
            target_os = "ios",
            target_os = "visionos",
            target_os = "watchos",
            target_os = "tvos",
        ))] {
            mod apple_other { pub const BACKEND: &str = "apple_other"; }
            pub use apple_other::*;
        } else if #[cfg(all(target_arch = "wasm32", target_os = "wasi"))] {
            cfg_if! {
                if #[cfg(target_env = "p1")] {
                    mod wasi_p1 { pub const BACKEND: &str = "wasi_p1"; }
                    pub use wasi_p1::*;
                } else if #[cfg(target_env = "p2")] {
                    mod wasi_p2 { pub const BACKEND: &str = "wasi_p2"; }
                    pub use wasi_p2::*;
                } else {
                    compile_error!(
                        "Unknown version of WASI (only previews 1 and 2 are supported) \
                        or Rust version older than 1.80 was used"
                    );
                }
            }
        } else if #[cfg(target_os = "hermit")] {
            mod hermit { pub const BACKEND: &str = "hermit"; }
            pub use hermit::*;
        } else if #[cfg(target_os = "vxworks")] {
            mod vxworks { pub const BACKEND: &str = "vxworks"; }
            pub use vxworks::*;
        } else if #[cfg(target_os = "solid_asp3")] {
            mod solid { pub const BACKEND: &str = "solid"; }
            pub use solid::*;
        } else if #[cfg(all(windows, target_vendor = "win7"))] {
            mod windows_legacy { pub const BACKEND: &str = "windows_legacy"; }
            pub use windows_legacy::*;
        } else if #[cfg(windows)] {
            mod windows { pub const BACKEND: &str = "windows"; }
            pub use windows::*;
        } else if #[cfg(all(target_arch = "x86_64", target_env = "sgx"))] {
            mod rdrand { pub const BACKEND: &str = "rdrand"; }
            pub use rdrand::*;
        } else if #[cfg(all(target_arch = "wasm32", any(target_os = "unknown", target_os = "none")))] {
            cfg_if! {
                if #[cfg(feature = "wasm_js")] {
                    mod wasm_js { pub const BACKEND: &str = "wasm_js"; }
                    pub use wasm_js::*;
                } else {
                    compile_error!(concat!(
                        "The wasm32-unknown-unknown targets are not supported by default; \
                        you may need to enable the \"wasm_js\" configuration flag. Note \
                        that enabling the `wasm_js` feature flag alone is insufficient. \
                        For more information see: \
                        https://docs.rs/getrandom/", env!("CARGO_PKG_VERSION"), "/#webassembly-support"
                    ));
                }
            }
        } else {
            compile_error!(concat!(
                "target is not supported. You may need to define a custom backend see: \
                https://docs.rs/getrandom/", env!("CARGO_PKG_VERSION"), "/#custom-backend"
            ));
        }
    }
}

// -------------------------------------------------------------------------
// SHAPES 9, 22 — TAIL-EXPRESSION position (the macro IS the function's value)
// with arms of three DIFFERENT concrete types unified by `-> impl DerefMut`,
// and `use cfg_if::cfg_if;` as the import form.
//
// wgpu-29.0.3/src/util/mutex.rs:40-59, verbatim. In aterm's graph neither
// `parking_lot` nor wgpu's build-script `std` cfg is set, so the arm selected
// here is the third — the `RefCell` spin loop, `break lock;` and all.
// -------------------------------------------------------------------------
mod wgpu_util_mutex {
    pub struct Mutex<T: ?Sized> {
        inner: core::cell::RefCell<T>,
    }

    impl<T> Mutex<T> {
        pub fn new(value: T) -> Self {
            Self {
                inner: core::cell::RefCell::new(value),
            }
        }
    }

    impl<T: ?Sized> Mutex<T> {
        pub fn lock(&self) -> impl core::ops::DerefMut<Target = T> + '_ {
            cfg_if::cfg_if! {
                if #[cfg(feature = "parking_lot")] {
                    self.inner.lock()
                } else if #[cfg(std)] {
                    self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
                } else {
                    loop {
                        let Ok(lock) = self.inner.try_borrow_mut() else {
                            // Without `std` all we can do is spin until the current lock is released
                            core::hint::spin_loop();
                            continue;
                        };

                        break lock;
                    }
                }
            }
        }
    }
}

// -------------------------------------------------------------------------
// SHAPE 9 again — tail expression, arch chain, on ALL FOUR cells.
//
// ring-0.17.14/src/digest/sha2/sha2_32.rs:24-50. Invocation form, position and
// predicates verbatim.
//
// BODY REDUCED: the real arms call `sha2_32_ffi!` against `ring`'s assembly and
// its `cpu::{GetFeature, arm::Sha256, intel::…}` feature types; reproducing them
// means vendoring `ring`. Each arm becomes a distinct `u32` instead, which
// keeps what this case is here for — a value-producing invocation as the entire
// body of a function, on a chain of `target_arch`/`target_endian` predicates.
// The `let`-and-`if` shape inside the arms is kept.
// -------------------------------------------------------------------------
mod ring_sha2_32 {
    pub fn block_data_order_32(feature_detected: bool) -> u32 {
        cfg_if! {
            if #[cfg(all(target_arch = "aarch64", target_endian = "little"))] {
                if feature_detected { 1 } else { 2 }
            } else if #[cfg(all(target_arch = "arm", target_endian = "little"))] {
                if feature_detected { 3 } else { 4 }
            } else if #[cfg(target_arch = "x86_64")] {
                if feature_detected { 5 } else { 6 }
            } else {
                7
            }
        }
    }
}

// -------------------------------------------------------------------------
// SHAPE 9 again — tail expression inside a method, ~90 sites of this shape in
// tiny-skia alone.
//
// tiny-skia-0.11.4/src/wide/f32x4_t.rs:61-72. Invocation form, position and
// predicates verbatim.
//
// BODY REDUCED: the real arms call `f32x4_floor` / `vrndmq_f32` intrinsics and
// tiny-skia's own `cast` / `trunc_int` / `cmp_gt` / `blend` helpers. Each arm
// becomes an `f32` expression instead; the tail position and the
// feature+arch+target_feature predicate chain — the parts the macro sees — are
// unchanged.
// -------------------------------------------------------------------------
mod tiny_skia_f32x4 {
    pub fn floor(v: f32) -> f32 {
        cfg_if::cfg_if! {
            if #[cfg(all(feature = "simd", target_feature = "simd128"))] {
                v - 0.5
            } else if #[cfg(all(feature = "simd", target_arch = "aarch64", target_feature = "neon"))] {
                v - 0.25
            } else {
                let roundtrip = v as i32 as f32;
                if roundtrip > v { roundtrip - 1.0 } else { roundtrip }
            }
        }
    }
}

// -------------------------------------------------------------------------
// SHAPES 12, 20 — `cfg_if!` invoked from INSIDE another `macro_rules!`, with
// `$expr` fragments substituted into the arm bodies, and a TRAILING COMMA
// inside an `all(...)` predicate.
//
// half-2.7.1/src/binary16/arch.rs:14-83, the `convert_fn!` macro verbatim,
// including the `target_arch = "aarch64",` trailing comma at :55.
//
// THIS IS WHY THE BODY MATCHER MUST BE `tt`. By the time `cfg_if!` sees
// `$f16c`, it is an opaque `NtExpr`. An opaque expression fragment matches `tt`
// and matches NOTHING else — not `item`, not `stmt`, not `block`. Any of those
// specifiers would fail at the match, loudly, and only on the cells where half
// is in the graph (all four, via naga and ciborium-ll).
//
// Also note `if x86_feature("f16c") { … }`: `cfg_if!`'s grammar is `if
// #[cfg(..)]`, and half's outer grammar is `if <call>`. Two different `if`
// languages, one nested in the other, and the `tt` matchers keep them apart.
// -------------------------------------------------------------------------
mod half_convert_fn {
    macro_rules! convert_fn {
        (if x86_feature("f16c") { $f16c:expr }
        else if aarch64_feature("fp16") { $aarch64:expr }
        else if loongarch64_feature("lsx") { $loongarch64:expr }
        else { $fallback:expr }) => {
            cfg_if::cfg_if! {
                // Use intrinsics directly when a compile target or using no_std
                if #[cfg(all(
                    any(target_arch = "x86", target_arch = "x86_64"),
                    target_feature = "f16c"
                ))] {
                    $f16c
                }
                else if #[cfg(all(
                    target_arch = "aarch64",
                    target_feature = "fp16"
                ))] {
                    $aarch64
                }
                else if #[cfg(all(
                    feature = "nightly",
                    target_arch = "loongarch64",
                    target_feature = "lsx"
                ))] {
                    $loongarch64
                }

                // Use CPU feature detection if using std
                else if #[cfg(all(
                    feature = "std",
                    any(target_arch = "x86", target_arch = "x86_64")
                ))] {
                    if IS_X86_FEATURE_DETECTED {
                        $f16c
                    } else {
                        $fallback
                    }
                }
                else if #[cfg(all(
                    feature = "std",
                    target_arch = "aarch64",
                ))] {
                    if IS_AARCH64_FEATURE_DETECTED {
                        $aarch64
                    } else {
                        $fallback
                    }
                }

                // Fallback to software
                else {
                    $fallback
                }
            }
        };
    }

    // BODY REDUCED: half's `use std::arch::is_x86_feature_detected;` plus the
    // `is_x86_feature_detected!("f16c")` call become these consts. They live in
    // arms this crate never selects (`feature = "std"` is not a feature of
    // `cfg-if`), so nothing here is expanded; they exist so the macro
    // DEFINITION above can stay verbatim.
    const IS_X86_FEATURE_DETECTED: bool = false;
    const IS_AARCH64_FEATURE_DETECTED: bool = false;

    pub fn f32_to_f16(f: f32) -> u16 {
        convert_fn! {
            if x86_feature("f16c") {
                (f as u16).wrapping_add(1)
            } else if aarch64_feature("fp16") {
                (f as u16).wrapping_add(2)
            } else if loongarch64_feature("lsx") {
                (f as u16).wrapping_add(3)
            } else {
                f as u16
            }
        }
    }
}

// -------------------------------------------------------------------------
// SHAPES 13, 17, 19 — an arm that DEFINES a `macro_rules!`, an arm carrying
// doc comments, and the macro staying visible to code AFTER the invocation by
// textual scope.
//
// nix-0.29.0/src/sys/ioctl/mod.rs:494-580. Invocation form, predicate and the
// doc-comment-then-`macro_rules!` arm shape are verbatim; the doc block is
// ABRIDGED (nix's runs ~40 lines per arm) and the generated function body is
// reduced from a real `ioctl` call to arithmetic.
//
// The assertion is not that it compiles — it is that `ioctl_write_int!` can be
// CALLED below, outside the invocation. Emitting an arm inside a block or a
// module would leave the macro invisible there, and the error would land at the
// call site rather than at the macro, which is how this gets misdiagnosed as a
// feature-flag problem.
// -------------------------------------------------------------------------
mod nix_ioctl {
    cfg_if! {
        if #[cfg(freebsdlike)] {
            /// Generates a wrapper function for a ioctl that writes an integer to the kernel.
            ///
            /// The arguments to this macro are:
            ///
            /// * The function name
            /// * The ioctl identifier
            /// * The ioctl sequence number
            macro_rules! ioctl_write_int {
                ($name:ident, $ioty:expr, $nr:expr) => (
                    pub fn $name(fd: i32, data: i32) -> i32 {
                        fd ^ ($ioty as i32) ^ ($nr as i32) ^ data ^ 0x0BSD
                    }
                )
            }
        } else {
            /// Generates a wrapper function for a ioctl that writes an integer to the kernel.
            ///
            /// The arguments to this macro are:
            ///
            /// * The function name
            /// * The ioctl identifier
            /// * The ioctl sequence number
            ///
            /// `nix::sys::ioctl::ioctl_param_type` depends on the OS:
            /// *   BSD - `libc::c_int`
            /// *   Linux - `libc::c_ulong`
            macro_rules! ioctl_write_int {
                ($name:ident, $ioty:expr, $nr:expr) => (
                    pub fn $name(fd: i32, data: i32) -> i32 {
                        fd ^ ($ioty as i32) ^ ($nr as i32) ^ data
                    }
                )
            }
        }
    }

    // The macro defined inside the selected arm, used AFTER the invocation.
    ioctl_write_int!(vt_activate, b'v', 4);

    /// An `else` arm holding nothing at all — seven sites in the graph have an
    /// empty arm body (polling lib.rs:1047, nix ioctl/mod.rs). `{ }` must be a
    /// legal arm and must expand to nothing.
    pub fn empty_arm_body_is_legal() -> u8 {
        cfg_if! {
            if #[cfg(any())] {
            } else if #[cfg(all())] {
            } else {
            }
        }
        3
    }
}

// -------------------------------------------------------------------------
// SHAPE 11 — ASSOCIATED-ITEM position: `cfg_if!` invoked directly inside a
// trait `impl`, expanding to the impl's methods.
//
// ahash-0.8.12/src/random_state.rs:150-167, verbatim (the `AtomicUsize`
// counter is real; only `RandomSource` is declared locally instead of
// imported). A statement-shaped or block-shaped expansion is not legal here.
// -------------------------------------------------------------------------
mod ahash_random_state {
    use core::sync::atomic::{AtomicUsize, Ordering};

    pub trait RandomSource {
        fn gen_hasher_seed(&self) -> usize;
    }

    pub struct DefaultRandomSource {
        counter: AtomicUsize,
    }

    impl DefaultRandomSource {
        pub const fn new() -> Self {
            Self {
                counter: AtomicUsize::new(0),
            }
        }
    }

    impl RandomSource for DefaultRandomSource {
        cfg_if::cfg_if! {
            if #[cfg(all(target_arch = "arm", target_os = "none"))] {
                fn gen_hasher_seed(&self) -> usize {
                    let stack = self as *const _ as usize;
                    let previous = self.counter.load(Ordering::Relaxed);
                    let new = previous.wrapping_add(stack);
                    self.counter.store(new, Ordering::Relaxed);
                    new
                }
            } else {
                fn gen_hasher_seed(&self) -> usize {
                    let stack = self as *const _ as usize;
                    self.counter.fetch_add(stack, Ordering::Relaxed)
                }
            }
        }
    }
}

// -------------------------------------------------------------------------
// SHAPES 16, 22 — `use cfg_if::cfg_if;` as the import, and arms whose items are
// `#[path = "…"] mod imp;`.
//
// parking_lot_core-0.9.12/src/thread_parker/mod.rs:1 and :53-90, verbatim. The
// eight backend files are real, under
// tests/consumer_forms/parking_lot_core_thread_parker/.
//
// THIS IS THE HEADLINE HAZARD MADE OBSERVABLE. On Linux the first two
// predicates BOTH hold: first-match-wins selects `linux.rs` (the futex parker),
// last-match-wins would select `unix.rs` (the pthread-condvar parker) and
// compile just as green. `parker_matches_rustc` below asserts the selection
// against a hand-written `cfg!()` cascade over the identical predicates.
// -------------------------------------------------------------------------
// SCAFFOLDING ATTRIBUTE, not part of the consumer: `#[path]` on an INLINE
// module sets the directory its child modules resolve in, which keeps this
// test's stand-in files under tests/consumer_forms/ instead of loose in
// tests/. It changes nothing the macro sees.
#[path = "consumer_forms/parking_lot_core_thread_parker"]
mod parking_lot_core_thread_parker {
    use cfg_if::cfg_if;

    cfg_if! {
        if #[cfg(any(target_os = "linux", target_os = "android"))] {
            #[path = "linux.rs"]
            mod imp;
        } else if #[cfg(unix)] {
            #[path = "unix.rs"]
            mod imp;
        } else if #[cfg(windows)] {
            #[path = "windows/mod.rs"]
            mod imp;
        } else if #[cfg(target_os = "redox")] {
            #[path = "redox.rs"]
            mod imp;
        } else if #[cfg(all(target_env = "sgx", target_vendor = "fortanix"))] {
            #[path = "sgx.rs"]
            mod imp;
        } else if #[cfg(all(
            feature = "nightly",
            target_family = "wasm",
            target_feature = "atomics"
        ))] {
            #[path = "wasm_atomic.rs"]
            mod imp;
        } else if #[cfg(target_family = "wasm")] {
            #[path = "wasm.rs"]
            mod imp;
        } else {
            #[path = "generic.rs"]
            mod imp;
        }
    }

    /// Which parker file the chain actually pulled in.
    pub fn selected() -> &'static str {
        imp::PARKER
    }
}

// -------------------------------------------------------------------------
// SHAPE 23 — a MODULE-LOCAL `extern crate cfg_if;` followed by
// `use self::cfg_if::cfg_if;`.
//
// libloading-0.8.9/src/os/unix/consts.rs:56-58, verbatim (the arm chain is
// abridged to three of its ~12 arms; the import form is the shape under test).
// This resolves the macro as a path item off the crate root, which
// `#[macro_export]` provides and a module-scoped `macro_rules!` would not.
// -------------------------------------------------------------------------
mod libloading_consts {
    type c_int = i32;

    #[cfg(any(not(libloading_docs), unix))]
    mod posix {
        extern crate cfg_if;
        use self::cfg_if::cfg_if;
        use super::c_int;
        cfg_if! {
            if #[cfg(target_os = "haiku")] {
                pub(super) const RTLD_LAZY: c_int = 0;
            } else if #[cfg(target_os = "aix")] {
                pub(super) const RTLD_LAZY: c_int = 4;
            } else if #[cfg(any(
                target_os = "linux",
                target_os = "android",
                target_os = "emscripten",

                target_os = "macos",
                target_os = "ios",
                target_os = "tvos",
                target_os = "visionos",
                target_os = "watchos",

                target_os = "freebsd",
                target_os = "dragonfly",
                target_os = "openbsd",
                target_os = "netbsd",
            ))] {
                pub(super) const RTLD_LAZY: c_int = 1;
            } else {
                pub(super) const RTLD_LAZY: c_int = 1;
            }
        }
    }

    #[cfg(any(not(libloading_docs), unix))]
    pub fn rtld_lazy() -> c_int {
        posix::RTLD_LAZY
    }
}

// -------------------------------------------------------------------------
// SHAPE 24 — a FUNCTION-LOCAL `extern crate cfg_if;` followed by
// `cfg_if::cfg_if!` in tail position.
//
// libloading-0.8.9/src/os/unix/mod.rs:268-288, verbatim (the two arm bodies
// become `bool` expressions instead of `dlsym` calls).
// -------------------------------------------------------------------------
mod libloading_get {
    pub fn get() -> bool {
        extern crate cfg_if;
        cfg_if::cfg_if! {
            // These targets are known to have MT-safe `dlerror`.
            if #[cfg(any(
                target_os = "linux",
                target_os = "android",
                target_os = "openbsd",
                target_os = "macos",
                target_os = "ios",
                target_os = "solaris",
                target_os = "illumos",
                target_os = "redox",
                target_os = "fuchsia",
                target_os = "cygwin",
            ))] {
                true
            } else {
                false
            }
        }
    }
}

// -------------------------------------------------------------------------
// SHAPE 14 — an arm holding `extern crate` plus items carrying a PROC-MACRO
// attribute, and a bare `extern { }` block.
//
// console_error_panic_hook-0.1.7/src/lib.rs:78-95, verbatim.
//
// The module is gated `not(target_arch = "wasm32")` — not to dodge anything the
// macro does, but because on wasm this arm WOULD be selected and would then
// need a real `wasm_bindgen` dependency, which this crate has no business
// acquiring. On every host that runs these tests the arm is dead, and what is
// under test is that the macro MATCHES `#[wasm_bindgen] extern { … }` and
// `extern crate wasm_bindgen;` as `tt` without ever expanding them.
// -------------------------------------------------------------------------
#[cfg(not(target_arch = "wasm32"))]
mod console_error_panic_hook {
    use std::panic;

    cfg_if! {
        if #[cfg(target_arch = "wasm32")] {
            extern crate wasm_bindgen;
            use wasm_bindgen::prelude::*;

            #[wasm_bindgen]
            extern {
                #[wasm_bindgen(js_namespace = console)]
                fn error(msg: String);

                type Error;

                #[wasm_bindgen(constructor)]
                fn new() -> Error;

                #[wasm_bindgen(structural, method, getter)]
                fn stack(error: &Error) -> String;
            }

            fn hook_impl(info: &panic::PanicInfo) {
                let mut msg = info.to_string();
                let e = Error::new();
                let stack = e.stack();
                msg.push_str(&stack);
                error(msg);
            }
        } else {
            fn hook_impl(info: &panic::PanicHookInfo) {
                let _ = info;
            }
        }
    }

    /// The selected arm's `hook_impl` exists exactly once.
    pub fn install() {
        let _: fn(&panic::PanicHookInfo) = hook_impl;
    }
}

// -------------------------------------------------------------------------
// SHAPE 21 (overlap, NO else) — arms whose bodies are ONLY `use` items.
//
// wgpu-hal-29.0.3/src/lib.rs:312-317, verbatim.
//
// This is the case where "dropped the negation" is SILENT rather than loud.
// Both arms import a name spelled `Arc`; if both were emitted the error would
// be a duplicate import, but reorder the arms or make one a glob and the
// collision resolves quietly to the wrong type. `supports_ptr_atomics` is a
// build-script cfg that is unset here, and `portable-atomic` is not a feature
// of this crate, so NEITHER arm is selected and the whole invocation must
// expand to nothing — 30 chains in the graph have no final else.
// -------------------------------------------------------------------------
mod wgpu_hal_arc {
    extern crate alloc;

    cfg_if::cfg_if! {
        if #[cfg(supports_ptr_atomics)] {
            use alloc::sync::Arc;
        } else if #[cfg(feature = "portable-atomic")] {
            use portable_atomic_util::Arc;
        }
    }
}

// -------------------------------------------------------------------------
// SHAPE 2 — arms whose items are `impl` blocks.
//
// ring-0.17.14/src/cpu.rs:149-166, verbatim (the `NotSend::VALUE` field becomes
// a unit).
// -------------------------------------------------------------------------
mod ring_cpu {
    pub struct Features(());

    cfg_if::cfg_if! {
        if #[cfg(any(all(target_arch = "aarch64", target_endian = "little"), all(target_arch = "arm", target_endian = "little"),
                     target_arch = "x86", target_arch = "x86_64"))] {
            impl Features {
                // SAFETY: This must only be called after CPU features have been written
                // and synchronized.
                pub(super) unsafe fn new_after_feature_flags_written_and_synced_unchecked() -> Self {
                    Self(())
                }
            }
        } else {
            impl Features {
                pub(super) fn new_no_features_to_detect() -> Self {
                    Self(())
                }
            }
        }
    }
}

// -------------------------------------------------------------------------
// SHAPE 17 — an if-only chain at item position whose arm holds nested
// `#[cfg]`-attributed imports, a trait and an impl.
//
// polling-3.11.0/src/lib.rs:1047-1063, verbatim (the `AsRawFd`/`AsFd` imports
// become local traits so the arm compiles off-Linux too; the nested `#[cfg]`
// attributes inside the arm are what matters and are unchanged).
// -------------------------------------------------------------------------
mod polling_as_raw_source {
    pub type RawFd = i32;

    pub trait AsRawFd {
        fn as_raw_fd(&self) -> RawFd;
    }

    cfg_if! {
        if #[cfg(any(unix, target_os = "hermit"))] {
            /// A resource with a raw file descriptor.
            pub trait AsRawSource {
                /// Returns the raw file descriptor.
                fn raw(&self) -> RawFd;
            }

            impl<T: AsRawFd> AsRawSource for &T {
                fn raw(&self) -> RawFd {
                    self.as_raw_fd()
                }
            }
        }
    }
}

// =========================================================================
// THE RUNTIME HALF — arm SELECTION, asserted against rustc's own `cfg!()`.
// =========================================================================

#[test]
fn parking_lot_core_selects_the_same_parker_rustc_would() {
    // The identical predicate chain, evaluated by rustc rather than by the
    // macro. On Linux the first two both hold and this is the difference
    // between the futex parker and the pthread-condvar parker.
    let expected = if cfg!(any(target_os = "linux", target_os = "android")) {
        "linux.rs"
    } else if cfg!(unix) {
        "unix.rs"
    } else if cfg!(windows) {
        "windows/mod.rs"
    } else if cfg!(target_os = "redox") {
        "redox.rs"
    } else if cfg!(all(target_env = "sgx", target_vendor = "fortanix")) {
        "sgx.rs"
    } else if cfg!(all(
        feature = "nightly",
        target_family = "wasm",
        target_feature = "atomics"
    )) {
        "wasm_atomic.rs"
    } else if cfg!(target_family = "wasm") {
        "wasm.rs"
    } else {
        "generic.rs"
    };
    assert_eq!(parking_lot_core_thread_parker::selected(), expected);
}

#[test]
fn crc32fast_selects_the_same_state_rustc_would() {
    let expected = if cfg!(all(
        target_feature = "sse2",
        any(target_arch = "x86", target_arch = "x86_64")
    )) {
        "pclmulqdq"
    } else if cfg!(all(stable_arm_crc32_intrinsics, target_arch = "aarch64")) {
        "aarch64"
    } else {
        "fallback-enum"
    };
    assert_eq!(crc32fast_specialized::selected(), expected);
}

#[test]
fn the_long_chain_and_its_nested_invocation_compile_and_run() {
    // getrandom's 28-arm chain: the selected arm's inline module is reachable,
    // which means the arm contributed BOTH of its items (the `mod` and the
    // `pub use` after it), not just the first.
    assert!(!getrandom_0_3_backends::BACKEND.is_empty());
}

#[test]
fn value_producing_invocations_produce_values() {
    // Tail position, three shapes, three consumers.
    assert!(ring_sha2_32::block_data_order_32(true) > 0);
    assert!(tiny_skia_f32x4::floor(2.75) <= 2.75);
    let m = wgpu_util_mutex::Mutex::new(41u32);
    *m.lock() += 1;
    assert_eq!(*m.lock(), 42);
}

#[test]
fn statement_position_invocations_leave_the_block_alone() {
    let mut a = nix_addr::UnixAddr { sun_len: 0 };
    assert!(a.set_length(7).is_ok());
    assert_eq!(nix_ioctl::empty_arm_body_is_legal(), 3);
}

#[test]
fn hygiene_lets_a_binding_escape_the_invocation() {
    // naga's `let inner = …;` inside the macro, read by `Self { inner }` after.
    assert!(naga_error::DiagnosticBuffer::new().inner().is_empty());
}

#[test]
fn associated_item_position_produced_exactly_one_method() {
    use ahash_random_state::RandomSource as _;
    let s = ahash_random_state::DefaultRandomSource::new();
    let a = s.gen_hasher_seed();
    let b = s.gen_hasher_seed();
    assert_ne!(a, b, "the non-arm-`arm` method fetch_adds, so these differ");
}

#[test]
fn a_macro_defined_inside_an_arm_is_callable_after_it() {
    assert_eq!(nix_ioctl::vt_activate(0, 0), (b'v' as i32) ^ 4);
}

#[test]
fn the_remaining_scaffolded_forms_are_live() {
    assert_eq!(getrandom_0_2_util_libc::errno_is_reachable(), 0);
    assert!(getrandom_0_3_error::both_aliases_exist(0).is_none());
    assert!(libloading_get::get() || !libloading_get::get());
    #[cfg(any(not(libloading_docs), unix))]
    assert_eq!(libloading_consts::rtld_lazy(), 1);
}

#[test]
fn a_cfg_if_driven_from_another_macros_expr_fragments_picks_the_right_arm() {
    // `half`'s `convert_fn!` chain, evaluated by rustc instead of by the macro.
    // Each arm's stand-in body adds a distinct constant to the input, so the
    // return value names the arm. mac-arm lands on the `fp16` arm — the second
    // — because aarch64-apple-darwin has that target_feature on by default,
    // which is exactly the kind of thing an implementation that mis-ordered the
    // arms would get wrong while still compiling.
    let expected: u16 = if cfg!(all(
        any(target_arch = "x86", target_arch = "x86_64"),
        target_feature = "f16c"
    )) {
        10
    } else if cfg!(all(target_arch = "aarch64", target_feature = "fp16")) {
        11
    } else if cfg!(all(
        feature = "nightly",
        target_arch = "loongarch64",
        target_feature = "lsx"
    )) {
        12
    } else {
        // The remaining arms are all `feature = "std"` guarded. `std` is not a
        // feature of this package, so they are unreachable and the chain falls
        // through to `$fallback`.
        9
    };
    assert_eq!(half_convert_fn::f32_to_f16(9.0), expected);
}
