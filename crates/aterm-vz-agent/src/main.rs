// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The guest agent's executable ([`aterm_vz_agent`]): the agent in the Linux guest, the
//! unix-socket seam on macOS, and in a debug build the tests' stand-in commands.

use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(any(test, debug_assertions))]
    if args.first().is_some_and(|a| a == "stand-in") {
        return aterm_vz_agent::stand_in::run(&args[1..]);
    }
    aterm_vz_agent::main(&args)
}

/// Off Unix there is no agent: the library is Unix's ([`aterm_vz_agent`]).
#[cfg(not(unix))]
fn main() -> ExitCode {
    eprintln!(
        "aterm-vz-agent: the Linux lane's guest agent runs in its Linux guest (and its test seam \
         on macOS), never on Windows"
    );
    ExitCode::from(2)
}
