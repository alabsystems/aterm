// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `windows-sys` 0.52 replacement providing the Win32 ABI declarations used by `ring`.

#![no_std]
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
        /// `Win32::System::Threading` (upstream feature `Win32_System_Threading`).
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
                /// Returns nonzero when the processor supports `processorfeature`.
                pub fn IsProcessorFeaturePresent(processorfeature: PROCESSOR_FEATURE_ID) -> BOOL;
            }
        }
    }
}
