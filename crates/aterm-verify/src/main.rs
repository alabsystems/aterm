// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified verify` CLI entry point ([`aterm_verify::run_cli`]).

fn main() -> std::process::ExitCode {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--version"))
        && std::env::args_os().nth(2).is_none()
    {
        println!("{}", aterm_verify::FROZEN_TOOL_VERSION);
        return std::process::ExitCode::SUCCESS;
    }
    // First, while this process has one thread: a tethered child starts as this executable's
    // tether verb, and this executable is recorded as the one its own tethered spawns run through.
    if let Some(code) = aterm_uds::tether::entry() {
        return code;
    }
    // Before a stage can rebuild this executable: the census memo is keyed by the code that
    // judges (one stat now; the bytes are hashed when the census first runs).
    aterm_census::pin_code();
    // Before any dispatch: every child of this dedicated CLI process (targo,
    // the test binaries, this binary re-invoked as a recorder or doc driver)
    // inherits the raised limit.
    aterm_uds::spawnfd::raise_soft_nofile();
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = aterm_verify::compile_width::wrapper_entry(&raw) {
        return std::process::ExitCode::from(code);
    }
    if raw
        .first()
        .is_some_and(|a| a == aterm_verify::docdriver::RUNTOOL_FLAG)
    {
        std::process::exit(aterm_verify::docdriver::runtool_main(&raw[1..]));
    }
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
    #[cfg(unix)]
    if let Some(code) = aterm_verify::watch::background_entry(&raw) {
        std::process::exit(code);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    std::process::exit(aterm_verify::run_cli(&args, &cwd));
}
