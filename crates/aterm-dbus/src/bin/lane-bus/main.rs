// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lane-bus`: D-Bus, XTEST, XDND, and compositor proxy test helper for the Linux runtime lane.

use std::process::ExitCode;

#[cfg(unix)]
mod relay;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
mod wlhide;
#[cfg(unix)]
mod xdnd;
#[cfg(unix)]
mod xgrab;
#[cfg(unix)]
mod xhide;
#[cfg(unix)]
mod xtest;

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        unix::main()
    }
    #[cfg(not(unix))]
    {
        eprintln!("lane-bus: NOT AVAILABLE: a unix D-Bus client for the Linux lane's guest");
        ExitCode::from(3)
    }
}
