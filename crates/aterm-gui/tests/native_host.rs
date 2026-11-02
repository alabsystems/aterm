// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original-image main for the real control/native host fixture.

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv = std::env::args_os().skip(1).collect::<Vec<_>>();
    // This must precede every fixture role and option. The production retained
    // command names this same executable as `harness native-launch ...`, and
    // System::own_launcher compares the actual current executable's identity.
    if argv.first().is_some_and(|word| word == "harness") {
        return aterm_agent::harness::cli::main_entry(argv.into_iter().skip(1).collect());
    }
    if !argv.is_empty() {
        eprintln!("native_host: expected no fixture arguments");
        return ExitCode::from(2);
    }
    #[cfg(all(unix, debug_assertions))]
    {
        match aterm_gui::run_native_host_fixture() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("native_host: FAIL: {error}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(all(unix, debug_assertions)))]
    {
        eprintln!("native_host: NOT RUN: requires a Unix debug build");
        ExitCode::from(2)
    }
}
