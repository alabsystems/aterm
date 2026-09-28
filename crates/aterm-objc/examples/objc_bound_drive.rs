// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The container drive as the example `aterm verify`'s objc-bound-drive stage
//! builds and runs (`--example objc_bound_drive`). The program is
//! `tests/main_thread_bound_drive.rs`, which the workspace test run executes
//! as a `harness = false` test; this file only gives the stage the target name
//! it builds, and goes when that stage does.

#[path = "../tests/main_thread_bound_drive.rs"]
mod drive;

fn main() -> std::process::ExitCode {
    drive::main()
}
