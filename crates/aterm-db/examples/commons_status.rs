// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only Commons diagnostics from the current development build.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    aterm_db::cli::main_entry(
        std::iter::once("commons-status".into())
            .chain(std::env::args_os().skip(1))
            .collect(),
    )
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("commons_status requires Unix Commons support");
    std::process::ExitCode::FAILURE
}
