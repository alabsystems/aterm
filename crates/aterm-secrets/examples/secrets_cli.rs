// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Run the secrets CLI entry point during development without composing the GUI.

fn main() -> std::process::ExitCode {
    aterm_secrets::cli::main_entry(std::env::args_os().skip(1).collect())
}
