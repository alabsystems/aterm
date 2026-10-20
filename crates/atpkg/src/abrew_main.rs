// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dev-only standalone `atpkg-abrew` entry. The engine lives in the
//! library (`atpkg::abrew::main_entry`) so the one `aterm` binary serves
//! argv0 `abrew`/`atpkg-abrew` in-process; the bundle ships both as
//! argv0 symlinks onto `aterm`.

use std::process::ExitCode;

fn main() -> ExitCode {
    atpkg::abrew::main_entry(std::env::args_os().skip(1).collect())
}
