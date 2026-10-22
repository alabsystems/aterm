// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified verify` — the `verify` alias runs this binary in place
//! with the caller's arguments; [`aterm_verify::run_cli`] is the whole CLI.

fn main() {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    // Cargo's runner during the test check: cargo runs this binary in place of
    // every test binary to record how it would start it
    // (`aterm_verify::testrun`). Answered before anything else.
    if raw
        .first()
        .is_some_and(|a| a == aterm_verify::testrun::RECORD_FLAG)
    {
        std::process::exit(aterm_verify::testrun::record_main(&raw[1..]));
    }
    // Cargo's doc driver during the doctests: the stage names the real one in
    // the environment and this binary as `RUSTDOC` (`aterm_verify::docdriver`).
    if std::env::var_os(aterm_verify::docdriver::DRIVER_ENV).is_some() {
        std::process::exit(aterm_verify::docdriver::main(&raw));
    }
    let args: Vec<String> = raw
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    // The in-terminal half the perf arena lanes launch inside each terminal.
    #[cfg(unix)]
    if args
        .first()
        .is_some_and(|a| a == aterm_verify::perf::ARENA_INNER_FLAG)
    {
        std::process::exit(aterm_verify::perf::arena_inner(&args[1..]));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    std::process::exit(aterm_verify::run_cli(&args, &cwd));
}
