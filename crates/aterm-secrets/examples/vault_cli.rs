// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development entry point for the exact CLI shipped as `aterm secrets`.
//! Commands have the same read and write effects as the shipped entry point.

fn main() -> std::process::ExitCode {
    aterm_secrets::cli::main_entry(std::env::args_os().skip(1).collect())
}
