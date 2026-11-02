// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only development inspection of pending writer recovery.

#[cfg(all(unix, any(test, debug_assertions)))]
fn main() -> std::process::ExitCode {
    use std::{path::Path, process::ExitCode};
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [home] = args.as_slice() else {
        eprintln!("usage: writer_recovery_inspect <absolute-home>");
        return ExitCode::from(2);
    };
    let home = Path::new(home);
    if !home.is_absolute() {
        eprintln!("usage: writer_recovery_inspect <absolute-home>");
        return ExitCode::from(2);
    }
    let paths = aterm_secrets::client::Paths::for_home(home);
    match aterm_db::inspect_writer_recovery(&paths) {
        Ok(value) => {
            aterm_uds::stdout::answer_exit("writer_recovery_inspect", &format!("{value}\n"), 0)
        }
        Err(error) => {
            eprintln!("writer_recovery_inspect: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(all(unix, any(test, debug_assertions))))]
fn main() -> std::process::ExitCode {
    eprintln!("writer_recovery_inspect requires a Unix debug build");
    std::process::ExitCode::from(2)
}
