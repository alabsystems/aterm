// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The guest agent's executable ([`aterm_vz_agent`]): the agent in the Linux guest, the
//! unix-socket seam on macOS, and in a debug build the tests' stand-in commands.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(any(test, debug_assertions))]
    if args.first().is_some_and(|a| a == "stand-in") {
        return aterm_vz_agent::stand_in::run(&args[1..]);
    }
    aterm_vz_agent::main(&args)
}
