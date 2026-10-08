// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Standalone dev/test entry for `aterm-ctl`; the client lives in the library
//! (`aterm_ctl::main_entry`) so the `aterm` binary serves `aterm ctl`
//! in-process. Not shipped: the bundle's `aterm-ctl` is a symlink onto `aterm`.

use std::process::ExitCode;

fn main() -> ExitCode {
    aterm_ctl::main_entry(std::env::args_os().skip(1).collect())
}
