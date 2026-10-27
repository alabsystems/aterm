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
            eprintln!("forge: {}\n\n{}", e.message(), cli::USAGE);
            return ExitCode::from(exit::USAGE);
        }
    };

    // Absolute always: a relative root would be applied twice downstream (as
    // `current_dir` and inside `--manifest-path`).
    let root = inv.root.clone().map_or_else(discover_root, canonical_root);

    let out = match (inv.cmd, root) {
        (cli::Cmd::Help, _) => {
            print!("{}", cli::USAGE);
            return ExitCode::from(exit::PASS);
        }
        (_, Err(msg)) => {
            eprintln!("forge: {msg}");
            return ExitCode::from(exit::COULD_NOT_RUN);
        }
        (cli::Cmd::Survey { cells, top }, Ok(root)) => aterm_forge::survey::run(&root, &cells, top),
        (cli::Cmd::Blame { pkg, cells }, Ok(root)) => aterm_forge::blame::run(&root, &pkg, &cells),
        (
            cli::Cmd::Budget {
                update,
                allow_regress,
            },
            Ok(root),
        ) => aterm_forge::budget::run(
            &aterm_forge::resolve::Cargo::from_env(),
            &root,
            update,
            allow_regress.as_deref(),
        ),
        (cli::Cmd::Attest, Ok(root)) => aterm_forge::attest::run(&root),
        (cli::Cmd::Check { cells }, Ok(root)) => {
            aterm_forge::check::run(&aterm_forge::resolve::Cargo::from_env(), &root, &cells)
        }
        (cli::Cmd::SerdeAttrs { paths }, Ok(root)) => aterm_forge::serde_attrs::run(&root, &paths),
        (cli::Cmd::AbrewHelp { options }, Ok(root)) => {
            aterm_forge::abrew_help::run(&root, &options)
        }
    };

    match out {
        Ok(Outcome { ok, log }) => {
            print!("{log}");
            ExitCode::from(if ok { exit::PASS } else { exit::FAIL })
        }
        Err(msg) => {
            eprintln!("forge: could not run: {msg}");
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

/// Walk up from the CWD (itself first) to the first `Cargo.toml` whose text
/// contains both `[workspace]` and `members`, since vendored crates carry an
/// empty `[workspace]` stub below the root.
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
            "no workspace Cargo.toml found at or above the current directory — pass --root"
                .to_string()
        })?;
    }
}
