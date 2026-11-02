// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish`, the atpkg producer's binary; everything is in the library.

fn main() -> std::process::ExitCode {
    #[cfg(unix)]
    if let Some(exit) = atpkg_publish::harness_local::early_entry() {
        return exit;
    }
    atpkg_publish::cli::main_entry(std::env::args())
}
