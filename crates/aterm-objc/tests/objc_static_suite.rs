// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Static-layout integration test suite for `aterm-objc`.

#[cfg(target_os = "macos")]
#[path = "support/sdk_consts.rs"]
mod sdk_consts;
#[cfg(target_os = "macos")]
#[path = "support/sdk_decls.rs"]
mod sdk_decls;

mod abi;
mod blocks;
mod cf_runtime;
mod cf_sdk;
mod conformance_wake_timer;
mod declared_abi;
mod declared_class;
mod declared_lifetimes;
mod encode_offsets;
mod gui_appkit_constants;
mod main_thread_bound;
mod runtime;
mod stret_declared;
mod winit_seam_constants;
