// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `cargo forge` — the front door. Arg parsing lives in [`cli`], every verb's
//! work lives in the library, and this file stays thin forever.

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

    // ABSOLUTE, ALWAYS. A relative `--root` is used twice downstream — as the
    // child `current_dir` AND inside `--manifest-path <root>/Cargo.toml` — and
    // the two compose into a path one level too high. Resolving it here means
    // every verb reports the same root and no verb needs its own workaround.
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
        } => aterm_forge::budget::run(&root, update, allow_regress.as_deref()),
        cli::Cmd::Attest => aterm_forge::attest::run(&root),
        cli::Cmd::Check { cells } => aterm_forge::check::run(&root, &cells),
        cli::Cmd::MirrorEmit { out } => aterm_forge::mirror::run_emit(&root, &out),
        cli::Cmd::MirrorVerify { dir } => aterm_forge::mirror::run_verify(&root, &dir),
        cli::Cmd::MirrorBundle { dir, out } => {
            aterm_forge::mirror_bundle::run_bundle(&root, &dir, &out)
        }
        cli::Cmd::MirrorUnbundle { file, out, force } => {
            aterm_forge::mirror_bundle::run_unbundle(&root, &file, &out, force)
        }
        cli::Cmd::MirrorCheckBundle { file } => {
            aterm_forge::mirror_bundle::run_check_bundle(&root, &file)
        }
        cli::Cmd::MirrorConfig { write } => aterm_forge::mirror_config::run_config(&root, write),
    };

    match out {
        Ok(Outcome { ok, log }) => {
            print!("{log}");
            ExitCode::from(if ok { exit::PASS } else { exit::FAIL })
        }
        // COULD_NOT_RUN is a distinct code on purpose: a gate that cannot run
        // must never be read as a gate that passed.
        Err(msg) => {
            eprintln!("cargo forge: could not run: {msg}");
            ExitCode::from(exit::COULD_NOT_RUN)
        }
    }
}

/// Resolve a user-supplied `--root` to an absolute path, refusing by name when
/// it does not exist — a typo'd root must not read as an empty workspace.
fn canonical_root(given: PathBuf) -> Result<PathBuf, String> {
    given.canonicalize().map_err(|e| {
        format!(
            "--root `{}` cannot be resolved: {e} — give an existing directory holding the \
             workspace `Cargo.toml`, or drop `--root` and run from inside the workspace",
            given.display()
        )
    })
}

/// Walk up from CWD to the directory holding the workspace `Cargo.toml`.
/// Identified by `[workspace]`, so a vendored crate's own manifest (which
/// carries an empty `[workspace]` stub) never wins over the real root: the
/// stub-bearing manifests all sit UNDER the root, so the first match walking
/// up from a vendor directory would be wrong. Require the members glob too.
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
