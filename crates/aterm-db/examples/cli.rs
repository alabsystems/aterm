// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Development entry for exercising the composed database verb without a window.
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    aterm_db::cli::main_entry(std::env::args_os().skip(1).collect())
}
#[cfg(not(unix))]
fn main() {}
