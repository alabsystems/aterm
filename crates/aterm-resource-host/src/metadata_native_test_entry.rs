// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one early entry of this crate's library test image. It runs the
//! production internal exec role, then the native metadata fixture's roles,
//! before libtest parses argv, without a helper executable. Only this image
//! mounts it: an integration image that compiles the same fixture modules
//! keeps its own argv.

extern "C" fn enter() {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Some(result) = crate::metadata_prelude::entry(&args) {
        std::process::exit(if result == std::process::ExitCode::SUCCESS {
            0
        } else {
            75
        });
    }
    crate::actor::metadata_native_role();
}

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static ENTRY: extern "C" fn() = enter;
