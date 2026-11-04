// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development entry point for the exact CLI shipped as `aterm db`.
//! Commands retain the production entry point's read and write effects.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    aterm_db::cli::main_entry(std::env::args_os().skip(1).collect())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("aterm db requires Unix");
    std::process::ExitCode::FAILURE
}
