// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A three-line shim. The command line itself is [`aterm_link::cli::dispatch`],
//! a LIBRARY entry, because the shipped product is ONE `aterm` binary with argv0
//! symlinks beside it (`expose = ["aterm"]`, `bundle = []`) — a second
//! executable would never reach a user. This bin exists so `cargo run -p
//! aterm-link` and this crate's integration tests can drive the bridge directly.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    aterm_link::cli::dispatch(&args)
}
