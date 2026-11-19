// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dev-only standalone `atpkg` entry ([`atpkg::cli::standalone_entry`], which says what it serves
//! and why it runs first). The CLI lives in the library (`atpkg::cli::main_entry`) so the one
//! `aterm` binary serves `aterm pkg` in-process; the bundle ships `atpkg` as an argv0 symlink onto
//! `aterm`.

fn main() -> std::process::ExitCode {
    atpkg::cli::standalone_entry()
}
