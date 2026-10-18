// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `windows-sys` 0.52: a first-party replacement for the crates.io package of
//! the same name, holding exactly what its one consumer reads (the manifest
//! says why the directory name differs).
//!
//! # The consumer
//!
//! ```text
//! ring 0.17.14   aarch64 Windows only   src/cpu/arm/windows.rs:16-30
//!                IsProcessorFeaturePresent(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE);
//!                nonzero means AES, PMULL and SHA-256 are all present
//! ```
//!
//! Nothing else in any cell's graph or in the lock asks for the 0.52 line. On
//! every other target ring does not depend on this package at all.
//!
//! # The declarations, at upstream's paths
//!
//! ```text
//! Win32::Foundation::BOOL                                   i32
//! Win32::System::Threading::PROCESSOR_FEATURE_ID            u32
//! Win32::System::Threading::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE   30
//! Win32::System::Threading::IsProcessorFeaturePresent       kernel32.dll, "system",
//!                                                           (PROCESSOR_FEATURE_ID) -> BOOL
//! ```
//!
//! They are Win32 ABI facts (winnt.h, processthreadsapi.h) and match the
//! registry crate's: `tests/oracle/` const-asserts every size, alignment,
//! value and signature against windows-sys 0.52.0 on x86_64- and
//! aarch64-pc-windows-msvc and compares the imported symbol and DLL, with a
//! planted mismatch of each kind that must fail (`targo --unverified run -p
//! aterm-win32-oracle -- oracle-052`). `tests/recorded.rs` holds the
//! recorded answers on any host.
//!
//! # Divergences
//!
//! Each is deliberate and unreachable from ring.
//!
//! 1. **Linking.** Upstream imports through windows-targets'
//!    `windows.0.52.0.lib`, a prebuilt import library in a `windows_*` crate
//!    whose build script puts it on the search path. This crate names
//!    `kernel32.dll` with `kind = "raw-dylib"`, as windows-link does for the
//!    0.61 line, so rustc writes the import itself: no import library, build
//!    script or third-party package. The object still references
//!    `__imp_IsProcessorFeaturePresent`, resolved from `kernel32.dll`.
//! 2. **Only these four items, never gated.** The rest of upstream (every
//!    other module, `core`, every other feature) is absent. The two features
//!    ring asks for, and the two they imply, are declared and gate nothing,
//!    so the declarations the oracle checks are the ones that compile,
//!    whatever a build enables.
//! 3. **Off Windows** the function is declared with no link attribute, as
//!    upstream's own non-Windows arm does; nothing calls it there.

#![no_std]
// Upstream's names are the contract: ring writes `Win32::System::Threading`,
// `BOOL` and `IsProcessorFeaturePresent`.
#![allow(non_snake_case, non_camel_case_types)]

/// `Win32` (upstream feature `Win32`).
pub mod Win32 {
    /// `Win32::Foundation` (upstream feature `Win32_Foundation`).
    pub mod Foundation {
        /// The Win32 `BOOL`: a 32-bit signed integer, nonzero for true.
        pub type BOOL = i32;
    }

    /// `Win32::System` (upstream feature `Win32_System`).
    pub mod System {
        /// `Win32::System::Threading` (upstream feature
        /// `Win32_System_Threading`).
        pub mod Threading {
            use super::super::Foundation::BOOL;

            /// The `ProcessorFeature` argument of [`IsProcessorFeaturePresent`].
            pub type PROCESSOR_FEATURE_ID = u32;

            /// The ARMv8 AES, PMULL, SHA-1 and SHA-256 instructions (winnt.h).
            pub const PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE: PROCESSOR_FEATURE_ID = 30u32;

            #[cfg_attr(
                all(windows, not(target_arch = "x86")),
                link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")
            )]
            #[cfg_attr(
                all(windows, target_arch = "x86"),
                link(
                    name = "kernel32.dll",
                    kind = "raw-dylib",
                    modifiers = "+verbatim",
                    import_name_type = "undecorated"
                )
            )]
            unsafe extern "system" {
                /// kernel32's `IsProcessorFeaturePresent`: nonzero when the
                /// processor supports `processorfeature`.
                pub fn IsProcessorFeaturePresent(processorfeature: PROCESSOR_FEATURE_ID) -> BOOL;
            }
        }
    }
}
