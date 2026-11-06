// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Developer-only front door for qualifying the protected helper without replacing the app.
//! Its credential protocol is the shipped library's same `aterm secrets git-credential` path.

fn main() -> std::process::ExitCode {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "secrets") {
        args.remove(0);
    }
    aterm_secrets::cli::main_entry(args)
}
