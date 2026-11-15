// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dev-only standalone `atpkg` entry. The CLI lives in the library (`atpkg::cli::main_entry`)
//! so the one `aterm` binary serves `aterm pkg` in-process; the bundle ships `atpkg` as an argv0
//! symlink onto `aterm`.
//!
//! It names no fleet source (`atpkg::peers::set_fleet`), so a download it runs races only the
//! LAN peers a person wrote in `[packages] peers`, never this Mac's fleet members. The source the
//! `aterm` binary sets (`crates/aterm/src/unit_peers.rs`) is not a call or two away: it reads
//! `aterm-net`'s fleet book and the swarm's sharing switch, which atpkg does not depend on, and
//! `aterm-cli` depends on atpkg, so this package cannot link it.

use std::process::ExitCode;

fn main() -> ExitCode {
    // First, while this process has one thread, as aterm's `main` does: a build's claim holder
    // forks away from its front's spawn, and this executable is recorded as the one the front
    // spawns that holder from (and tethered spawns run through).
    if let Some(code) = atpkg::build_admit::hold::entry() {
        return code;
    }
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // This binary is a development tool. Qualify its real retained image before tests
    // start a separately bounded admission/consumer phase; no producer override exists.
    if args
        .first()
        .is_some_and(|arg| arg == "__qualify_retained_helper")
    {
        if args.len() != 1 {
            return ExitCode::from(2);
        }
        return match aterm_uds::tether::prepared_executable_within(
            aterm_uds::deadline::FIRST_EXEC_HANG,
        ) {
            Ok(Some(_)) => {
                println!("retained helper prepared");
                ExitCode::SUCCESS
            }
            result => {
                eprintln!("retained helper qualification failed: {result:?}");
                ExitCode::FAILURE
            }
        };
    }
    atpkg::cli::main_entry(args)
}
