// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Thin standalone entry for `atpkg` — the CLI lives in the library
//! (`atpkg::cli::main_entry`) so the ONE `aterm` binary serves `aterm pkg`
//! in-process; this bin exists for dev builds and is NOT shipped (the bundle
//! carries an `atpkg` argv0 symlink onto `aterm`).

use std::process::ExitCode;

fn main() -> ExitCode {
    atpkg::cli::main_entry(std::env::args_os().skip(1).collect())
}
