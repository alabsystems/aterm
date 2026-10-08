// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Dev-only shim over [`aterm_link::cli::dispatch`] (the shipped entry is the one
//! `aterm` binary), so `targo run -p aterm-link` and the integration tests can
//! drive the bridge directly.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    aterm_link::cli::dispatch(&args)
}
