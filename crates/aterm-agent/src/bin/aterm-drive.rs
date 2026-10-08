// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only standalone entry for `aterm-drive`; the CLI lives in
//! `aterm_agent::drive_cli::main_entry`, which the shipped `aterm` binary serves
//! as `aterm drive` and through its argv0 alias.

use std::process::ExitCode;

fn main() -> ExitCode {
    aterm_agent::drive_cli::main_entry(std::env::args_os().skip(1).collect())
}
