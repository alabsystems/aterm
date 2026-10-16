// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dev-only standalone `atpkg` entry. The CLI lives in the library (`atpkg::cli::main_entry`)
//! so the one `aterm` binary serves `aterm pkg` in-process; the bundle ships `atpkg` as an argv0
//! symlink onto `aterm`.

use std::process::ExitCode;

fn main() -> ExitCode {
    atpkg::cli::main_entry(std::env::args_os().skip(1).collect())
}
