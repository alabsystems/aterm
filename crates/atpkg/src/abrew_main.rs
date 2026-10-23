// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Dev-only standalone `atpkg-abrew` entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    atpkg::abrew::main_entry(std::env::args_os().skip(1).collect())
}
