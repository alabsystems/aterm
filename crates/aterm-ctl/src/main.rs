// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thin standalone entry for `aterm-ctl` — the client lives in the library
//! (`aterm_ctl::main_entry`) so the ONE `aterm` binary can serve `aterm ctl`
//! in-process; this bin exists for dev/test builds and is NOT shipped (the
//! bundle carries an `aterm-ctl` symlink onto `aterm`, dispatched by argv0).

use std::process::ExitCode;

fn main() -> ExitCode {
    aterm_ctl::main_entry(std::env::args_os().skip(1).collect())
}
