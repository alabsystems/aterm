// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only standalone entry for `aterm-fleet`; the CLI lives in
//! `aterm_agent::fleet_cli::main_entry`, which the shipped `aterm` binary serves
//! as `aterm fleet` and through its argv0 alias.

fn main() -> std::process::ExitCode {
    aterm_agent::fleet_cli::main_entry(std::env::args_os().skip(1).collect())
}
