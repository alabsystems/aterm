// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `targo --unverified forge`: arg parsing lives in [`cli`], every verb's work in the library.

mod cli;

use aterm_forge::{Outcome, exit};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let inv = match cli::parse(std::env::args().skip(1)) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("cargo forge: {}\n\n{}", e.message(), cli::USAGE);
            return ExitCode::from(exit::USAGE);
        }
    };

    // Absolute always: a relative root would be applied twice downstream (as
    // `current_dir` and inside `--manifest-path`).
    let root = match inv.root.clone().map_or_else(discover_root, canonical_root) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("cargo forge: {msg}");
            return ExitCode::from(exit::COULD_NOT_RUN);
        }
    };

    let out = match inv.cmd {
        cli::Cmd::Help => {
            print!("{}", cli::USAGE);
            return ExitCode::from(exit::PASS);
        }
        cli::Cmd::Survey { cells, top, json } => {
            aterm_forge::survey::run(&root, &cells, top, json.as_deref())
        }
        cli::Cmd::Blame { pkg, cells } => aterm_forge::blame::run(&root, &pkg, &cells),
        cli::Cmd::Budget {
            update,
            allow_regress,
        } => aterm_forge::budget::run(
            &aterm_forge::resolve::Cargo::from_env(),
            &root,
            update,
            allow_regress.as_deref(),
        ),
        cli::Cmd::Attest => aterm_forge::attest::run(&root),
        cli::Cmd::Check { cells } => {
            aterm_forge::check::run(&aterm_forge::resolve::Cargo::from_env(), &root, &cells)
        }
        cli::Cmd::SerdeAttrs { paths } => aterm_forge::serde_attrs::run(&root, &paths),
    };

    match out {
        Ok(Outcome { ok, log }) => {
            print!("{log}");
            ExitCode::from(if ok { exit::PASS } else { exit::FAIL })
        }
        Err(msg) => {
            eprintln!("cargo forge: could not run: {msg}");
            ExitCode::from(exit::COULD_NOT_RUN)
        }
    }
}

/// Resolve a user-supplied `--root` to an absolute path, refusing by name when
/// it does not exist — a typo'd root must not read as an empty workspace.
fn canonical_root(given: PathBuf) -> Result<PathBuf, String> {
    let abs = given.canonicalize().map_err(|e| {
        format!(
            "--root `{}` cannot be resolved: {e} — give an existing directory holding the \
             workspace `Cargo.toml`, or drop `--root` and run from inside the workspace",
            given.display()
        )
    })?;
    Ok(aterm_forge::resolve::strip_verbatim_prefix(abs))
}

/// Walk up from CWD to the directory holding the workspace `Cargo.toml`,
/// identified by `[workspace]` plus the members glob, since vendored crates
/// carry an empty `[workspace]` stub below the root.
fn discover_root() -> Result<PathBuf, String> {
    let mut dir: &Path =
        &std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    loop {
        let manifest = dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && text.contains("[workspace]")
            && text.contains("members")
        {
            return Ok(dir.to_path_buf());
        }
        dir = dir.parent().ok_or_else(|| {
            "no workspace Cargo.toml found above the current directory — pass --root".to_string()
        })?;
    }
}
