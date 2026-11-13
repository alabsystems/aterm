// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `atpkg-publish`, the atpkg producer's binary; everything is in the library.

fn main() -> std::process::ExitCode {
    if let Err(error) = agh::git::HelperHost::Subcommand.record() {
        eprintln!("{error}");
        return std::process::ExitCode::FAILURE;
    }
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    #[cfg(unix)]
    if let Some(exit) = atpkg_publish::harness_local::early_entry() {
        return exit;
    }
    atpkg_publish::cli::main_entry(std::env::args())
}
