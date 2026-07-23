// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Thin standalone entry for `aterm-fleet` — the CLI lives in the library
//! (`aterm_agent::fleet_cli::main_entry`) so the ONE `aterm` binary serves
//! `aterm fleet` in-process; this bin exists for dev builds and is NOT
//! shipped (the bundle carries an argv0 symlink onto `aterm`).

fn main() -> ! {
    aterm_agent::fleet_cli::main_entry(std::env::args_os().skip(1).collect())
}
