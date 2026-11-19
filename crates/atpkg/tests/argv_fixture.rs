// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A stand-in for a program a shim starts (a build, the aterm a twin asks), copied under that
//! program's name: with [`DIR`] naming a folder it records how it was started in
//! `<folder>/<its stem>.argv` (each argument, then a NUL) and `<folder>/<its stem>.env` (each of
//! [`ENV_SEEN`] it holds, `NAME=value` and a NUL), says `<its stem> ran` on its standard output,
//! and exits with the code `<EXIT_PREFIX><STEM>` names (0 without one). Under the stem `aterm`
//! it answers `--into <folder>` as `aterm harness tap --launch` does: it records whether that
//! folder stood (`aterm.into`: `1` or `0`) and, where [`WORDS`] is set, writes its value there
//! as the twin's words, through a temporary file it renames.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

/// The folder the stand-in records into; without it the stand-in is not this one.
const DIR: &str = "ATPKG_ARGV_FIXTURE";
/// The words the `aterm` stand-in writes to `--into`'s value file.
const WORDS: &str = "ATPKG_ARGV_FIXTURE_WORDS";
/// The prefix of the variable naming a stem's exit code (`…_EXIT_CLAUDE`).
const EXIT_PREFIX: &str = "ATPKG_ARGV_FIXTURE_EXIT_";
/// The variables whose presence a test judges in the started program's environment.
const ENV_SEEN: [&str; 5] = [
    "__atpkg_tap",
    "__atpkg_tap_d",
    "__atpkg_tap_n",
    "DISABLE_AUTOUPDATER",
    "ATERM_LAUNCH_STDOUT_TTY",
];

pub fn entry() -> Option<ExitCode> {
    let dir = PathBuf::from(std::env::var_os(DIR)?);
    let stem = std::env::current_exe()
        .ok()?
        .file_stem()?
        .to_string_lossy()
        .into_owned();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    Some(match record(&dir, &stem, &args) {
        Ok(()) => {
            let code = std::env::var(format!("{EXIT_PREFIX}{}", stem.to_ascii_uppercase()))
                .ok()
                .and_then(|code| code.parse::<u8>().ok())
                .unwrap_or(0);
            ExitCode::from(code)
        }
        Err(error) => {
            eprintln!("argv fixture: {error}");
            ExitCode::from(99)
        }
    })
}

fn record(dir: &std::path::Path, stem: &str, args: &[OsString]) -> std::io::Result<()> {
    let mut argv = String::new();
    for arg in args {
        argv.push_str(&arg.to_string_lossy());
        argv.push('\0');
    }
    std::fs::write(dir.join(format!("{stem}.argv")), argv)?;
    let mut env = String::new();
    for name in ENV_SEEN {
        if let Some(value) = std::env::var_os(name) {
            env.push_str(name);
            env.push('=');
            env.push_str(&value.to_string_lossy());
            env.push('\0');
        }
    }
    std::fs::write(dir.join(format!("{stem}.env")), env)?;
    if stem == "aterm"
        && let Some(at) = args.iter().position(|arg| arg == "--into")
        && let Some(into) = args.get(at + 1)
    {
        let into = PathBuf::from(into);
        let stood = !into.as_os_str().is_empty() && into.is_dir();
        std::fs::write(dir.join("aterm.into"), if stood { "1" } else { "0" })?;
        if stood && let Some(words) = std::env::var_os(WORDS) {
            let tmp = into.join("value.tmp");
            std::fs::write(&tmp, words.to_string_lossy().as_bytes())?;
            std::fs::rename(&tmp, into.join("value"))?;
        }
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "{stem} ran")?;
    out.flush()
}
