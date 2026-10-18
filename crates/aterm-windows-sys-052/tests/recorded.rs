// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The four declarations against the answers windows-sys 0.52.0 gave the
//! oracle (`tests/oracle/PROVENANCE.md`; `targo --unverified run -p
//! aterm-win32-oracle -- oracle-052` runs it; recorded 2026-10-15 on x86_64- and
//! aarch64-pc-windows-msvc before the `[patch]` row took the registry crate
//! out of the graph). The oracle needs the registry crate and a Windows std;
//! this holds the same answers on any host, so a changed width, signedness,
//! value or signature fails wherever the workspace tests run.

use windows_sys::Win32::Foundation::BOOL;
use windows_sys::Win32::System::Threading::{
    IsProcessorFeaturePresent, PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE, PROCESSOR_FEATURE_ID,
};

// Recorded: BOOL is `i32` and PROCESSOR_FEATURE_ID is `u32`. Identity, so size,
// alignment and a same-width change of signedness are all held.
const _: fn(BOOL) -> i32 = |x| x;
const _: fn(PROCESSOR_FEATURE_ID) -> u32 = |x| x;

// Recorded: `unsafe extern "system" fn(u32) -> i32`. A const, so the symbol is
// never referenced on a host that cannot link it.
const _: unsafe extern "system" fn(u32) -> i32 = IsProcessorFeaturePresent;

#[test]
fn the_constant_holds_its_recorded_value() {
    assert_eq!(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE, 30);
}
