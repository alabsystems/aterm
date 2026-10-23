// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified verify` CLI entry point ([`aterm_verify::run_cli`]).

fn main() {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if raw
        .first()
        .is_some_and(|a| a == aterm_verify::testrun::RECORD_FLAG)
    {
        std::process::exit(aterm_verify::testrun::record_main(&raw[1..]));
    }
    if std::env::var_os(aterm_verify::docdriver::DRIVER_ENV).is_some() {
        std::process::exit(aterm_verify::docdriver::main(&raw));
    }
    let args: Vec<String> = raw
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
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
