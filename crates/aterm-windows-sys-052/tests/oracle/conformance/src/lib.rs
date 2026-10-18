// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Every declaration of crates/aterm-windows-sys-052 against the registry
//! windows-sys 0.52.0, decided by the compiler for the Windows triple being
//! checked (`targo --unverified run -p aterm-win32-oracle -- oracle-052` runs
//! x86_64- and aarch64-pc-windows-msvc):
//!
//! - each type alias: identity, both ways, plus size and alignment;
//! - the constant: its type and value;
//! - the function: a whole fn-pointer coercion in each direction, which fixes
//!   the ABI string, unsafety, argument and return types.
//!
//! The import itself, the symbol a caller references and the DLL it resolves
//! to, has no const view in Rust; the driver compares it from the compiled
//! objects (the `probe-*` features).
//!
//! Under a `plant-*` feature the facade is swapped for a copy with one
//! planted mismatch, and the driver requires the check to FAIL.

#![no_std]
#![allow(non_snake_case, non_camel_case_types)]

use upstream::Win32::Foundation as UF;
use upstream::Win32::System::Threading as UT;

#[cfg(not(any(
    feature = "plant-type",
    feature = "plant-size",
    feature = "plant-const",
    feature = "plant-abi",
    feature = "plant-arg",
    feature = "plant-ret",
    feature = "plant-link-name",
    feature = "plant-dll"
)))]
use facade as shim;

use shim::Win32::Foundation as SF;
use shim::Win32::System::Threading as ST;

// --- types -----------------------------------------------------------------

const _: fn(SF::BOOL) -> UF::BOOL = |x| x;
const _: fn(UF::BOOL) -> SF::BOOL = |x| x;
const _: () = assert!(size_of::<SF::BOOL>() == size_of::<UF::BOOL>());
const _: () = assert!(align_of::<SF::BOOL>() == align_of::<UF::BOOL>());

const _: fn(ST::PROCESSOR_FEATURE_ID) -> UT::PROCESSOR_FEATURE_ID = |x| x;
const _: fn(UT::PROCESSOR_FEATURE_ID) -> ST::PROCESSOR_FEATURE_ID = |x| x;
const _: () =
    assert!(size_of::<ST::PROCESSOR_FEATURE_ID>() == size_of::<UT::PROCESSOR_FEATURE_ID>());
const _: () =
    assert!(align_of::<ST::PROCESSOR_FEATURE_ID>() == align_of::<UT::PROCESSOR_FEATURE_ID>());

// --- the constant ----------------------------------------------------------

const _: UT::PROCESSOR_FEATURE_ID = ST::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE;
const _: () = assert!(
    ST::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE == UT::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE
);

// --- the function ------------------------------------------------------------

const _: unsafe extern "system" fn(UT::PROCESSOR_FEATURE_ID) -> UF::BOOL =
    ST::IsProcessorFeaturePresent;
const _: unsafe extern "system" fn(ST::PROCESSOR_FEATURE_ID) -> SF::BOOL =
    UT::IsProcessorFeaturePresent;

// --- the import (the driver's import phase) --------------------------------

/// One call through the facade: its object's undefined symbol is the import.
#[cfg(feature = "probe-facade")]
#[unsafe(no_mangle)]
pub extern "C" fn aterm_wb1_probe() -> i32 {
    unsafe { ST::IsProcessorFeaturePresent(ST::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE) as i32 }
}

/// One call through the registry crate, for the same comparison.
#[cfg(feature = "probe-upstream")]
#[unsafe(no_mangle)]
pub extern "C" fn aterm_wb1_probe() -> i32 {
    unsafe { UT::IsProcessorFeaturePresent(UT::PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE) as i32 }
}

// --- the negative controls ---------------------------------------------------

/// The facade with ONE planted mismatch per `plant-*` feature.
#[cfg(any(
    feature = "plant-type",
    feature = "plant-size",
    feature = "plant-const",
    feature = "plant-abi",
    feature = "plant-arg",
    feature = "plant-ret",
    feature = "plant-link-name",
    feature = "plant-dll"
))]
mod shim {
    pub mod Win32 {
        pub mod Foundation {
            /// Same size and alignment as `i32`: only identity catches it.
            #[cfg(feature = "plant-type")]
            pub type BOOL = u32;
            #[cfg(not(feature = "plant-type"))]
            pub type BOOL = i32;
        }
        pub mod System {
            pub mod Threading {
                use super::super::Foundation::BOOL;

                #[cfg(feature = "plant-size")]
                pub type PROCESSOR_FEATURE_ID = u64;
                #[cfg(not(feature = "plant-size"))]
                pub type PROCESSOR_FEATURE_ID = u32;

                pub const PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE: PROCESSOR_FEATURE_ID =
                    if cfg!(feature = "plant-const") {
                        31
                    } else {
                        30
                    };

                #[cfg(feature = "plant-abi")]
                #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
                unsafe extern "C" {
                    pub fn IsProcessorFeaturePresent(
                        processorfeature: PROCESSOR_FEATURE_ID,
                    ) -> BOOL;
                }

                #[cfg(feature = "plant-arg")]
                #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
                unsafe extern "system" {
                    pub fn IsProcessorFeaturePresent(processorfeature: i32) -> BOOL;
                }

                #[cfg(feature = "plant-ret")]
                #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
                unsafe extern "system" {
                    pub fn IsProcessorFeaturePresent(processorfeature: PROCESSOR_FEATURE_ID) -> u8;
                }

                #[cfg(feature = "plant-link-name")]
                #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
                unsafe extern "system" {
                    #[link_name = "IsProcessorFeaturePresentA"]
                    pub fn IsProcessorFeaturePresent(
                        processorfeature: PROCESSOR_FEATURE_ID,
                    ) -> BOOL;
                }

                #[cfg(feature = "plant-dll")]
                #[link(name = "kernelbase.dll", kind = "raw-dylib", modifiers = "+verbatim")]
                unsafe extern "system" {
                    pub fn IsProcessorFeaturePresent(
                        processorfeature: PROCESSOR_FEATURE_ID,
                    ) -> BOOL;
                }

                #[cfg(not(any(
                    feature = "plant-abi",
                    feature = "plant-arg",
                    feature = "plant-ret",
                    feature = "plant-link-name",
                    feature = "plant-dll"
                )))]
                #[link(name = "kernel32.dll", kind = "raw-dylib", modifiers = "+verbatim")]
                unsafe extern "system" {
                    pub fn IsProcessorFeaturePresent(
                        processorfeature: PROCESSOR_FEATURE_ID,
                    ) -> BOOL;
                }
            }
        }
    }
}
