// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The dev shim: `aterm agh` and argv0 `agh` are the shipped forms.

fn main() -> std::process::ExitCode {
    if let Err(error) = agh::git::HelperHost::Direct.record() {
        eprintln!("{error}");
        return std::process::ExitCode::FAILURE;
    }
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    agh::cli::main_entry(std::env::args_os().skip(1).collect())
}
