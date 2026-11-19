// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Measure one aRegistry machine picker against the explicitly selected local replica.

fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--local")) || args.next().is_some() {
        eprintln!("usage: registry_open --local");
        return std::process::ExitCode::from(2);
    }
    #[cfg(unix)]
    match aterm_gui::measure_local_registry() {
        Ok(report) => {
            println!("{report}");
            std::process::ExitCode::SUCCESS
        }
        Err(report) => {
            eprintln!("{report}");
            std::process::ExitCode::FAILURE
        }
    }
    #[cfg(not(unix))]
    {
        eprintln!("status=unsupported");
        std::process::ExitCode::FAILURE
    }
}
