// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Recorded differential tests against `windows-sys` 0.52.0 oracle answers.

use windows_sys::Win32::Foundation::BOOL;
use windows_sys::Win32::System::Threading::{
    IsProcessorFeaturePresent, PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE, PROCESSOR_FEATURE_ID,
};

const _: fn(BOOL) -> i32 = |x| x;
const _: fn(PROCESSOR_FEATURE_ID) -> u32 = |x| x;

const _: unsafe extern "system" fn(u32) -> i32 = IsProcessorFeaturePresent;

#[test]
fn the_constant_holds_its_recorded_value() {
    assert_eq!(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE, 30);
}
