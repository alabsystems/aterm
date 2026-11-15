// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `aterm-census`: the main-loop census and the source guards over an aterm checkout.
//!
//! ```text
//! targo --unverified run -q -p aterm-census                      # everything, this checkout
//! targo --unverified run -q -p aterm-census -- --guards          # just the source guards
//! targo --unverified run -q -p aterm-census -- --mainloop        # just the main-loop census
//! targo --unverified run -q -p aterm-census -- --all <root>      # everything, another checkout
//! ```
//!
//! Bare `aterm-census` is `--all`; census flags combine. In
//! [`aterm_census::CENSUSES`] order, a clean census prints its verdict line
//! and a failed one its transcript and the command that reruns just it
//! (`--verbose` prints every transcript); a summary line ends the run.
//! Exit 0 = every selected census clean, 1 = a census failed (a finding, a
//! missed floor, or a walk that could not run), 2 = usage, or the root is not
//! a checkout.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use aterm_census::{CENSUSES, Census};

fn usage() -> String {
    let mut text = String::from(
        "usage: aterm-census [--all | --<census>...] [-v] [<root>]\n\n\
         Runs the main-loop census and the source guards over the aterm checkout at\n\
         <root> (default: the checkout holding the current directory). A clean\n\
         census prints its verdict; a failed one prints its findings and how to\n\
         rerun it.\n\n\
         \x20 --all          every census (the default when none is named)\n",
    );
    for census in CENSUSES {
        text.push_str(&format!("  --{:<12} {}\n", census.name, census.about));
    }
    text.push_str(
        "  -v, --verbose  every census's full transcript, clean or not\n\
         \x20 -h, --help     this text\n\n\
         Census flags combine. Exit: 0 clean, 1 a census failed, 2 usage or bad <root>.\n",
    );
    text
}

/// What the command line asked for.
struct Invocation {
    selected: Vec<&'static Census>,
    root: Option<PathBuf>,
    verbose: bool,
}

/// `Ok(None)` is `--help`; `Err` is a usage error to report.
fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Invocation>, String> {
    let mut selected: Vec<&'static Census> = Vec::new();
    let mut root: Option<PathBuf> = None;
    let mut verbose = false;
    for arg in args {
        if arg == "-h" || arg == "--help" {
            return Ok(None);
        }
        if arg == "-v" || arg == "--verbose" {
            verbose = true;
        } else if arg == "--all" {
            selected.extend(CENSUSES);
        } else if let Some(name) = arg.strip_prefix("--") {
            let census = CENSUSES
                .iter()
                .find(|c| c.name == name)
                .ok_or_else(|| format!("unknown option {arg:?}"))?;
            selected.push(census);
        } else if arg.starts_with('-') {
            return Err(format!("unknown option {arg:?}"));
        } else if let Some(first) = &root {
            return Err(format!(
                "one checkout root, not two ({} and {arg})",
                first.display()
            ));
        } else {
            root = Some(PathBuf::from(arg));
        }
    }
    if selected.is_empty() {
        selected.extend(CENSUSES);
    }
    // `--all --guards` runs the guards once, in table order.
    selected.sort_by_key(|c| CENSUSES.iter().position(|t| t.name == c.name));
    selected.dedup_by_key(|c| c.name);
    Ok(Some(Invocation {
        selected,
        root,
        verbose,
    }))
}

/// The checkout to walk when none is named: the nearest ancestor of the current
/// directory that holds [`CHECKOUT_MARKER`] (from `vendor/astream` that is the
/// superproject), else the current directory itself. Read from the filesystem, so
/// the census spawns nothing.
fn default_root() -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            cwd.ancestors()
                .find(|dir| dir.join(CHECKOUT_MARKER).is_file())
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The file every aterm checkout has and the censuses cannot run without.
const CHECKOUT_MARKER: &str = "crates/aterm-gui/Cargo.toml";

/// `path` as one shell word.
fn shell_word(path: &Path) -> String {
    let text = path.display().to_string();
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@".contains(c));
    if plain {
        text
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// Why a census run by this binary holds captures to no engine baseline: it reads no engine
/// (a census spawns nothing and reads no network), and `targo --unverified verify census` reads
/// one commit of it.
const STANDALONE: &str = "aterm-census reads no engine; `targo --unverified verify census` \
                          holds captures to one commit's baseline";

fn run(selected: &[&'static Census], root: &Path, verbose: bool) -> bool {
    let started = Instant::now();
    let outcomes = aterm_census::run_censuses(root, selected, &Err(STANDALONE.into()));
    let mut verdicts = Vec::new();
    for (census, outcome) in selected.iter().zip(&outcomes) {
        if outcome.ok && !verbose {
            println!("{}", outcome.verdict);
        } else {
            println!("\n{}", outcome.log.trim_end());
            if !outcome.ok {
                println!(
                    "rerun: targo --unverified run -q -p aterm-census -- --{} {}",
                    census.name,
                    shell_word(root)
                );
            }
            println!();
        }
        verdicts.push(format!(
            "{} {}",
            census.name,
            if outcome.ok { "ok" } else { "FAIL" }
        ));
    }
    let ok = outcomes.iter().all(|o| o.ok);
    println!(
        "census: {} · {} ({:.1} s, {})",
        if ok { "clean" } else { "FAILED" },
        verdicts.join(" · "),
        started.elapsed().as_secs_f64(),
        root.display()
    );
    ok
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match parse(args) {
        Ok(Some(invocation)) => invocation,
        Ok(None) => {
            print!("{}", usage());
            return ExitCode::SUCCESS;
        }
        Err(why) => {
            eprint!("aterm-census: {why}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    let root = invocation.root.unwrap_or_else(default_root);
    if !root.join(CHECKOUT_MARKER).is_file() {
        eprintln!(
            "aterm-census: {} is not an aterm checkout (no {CHECKOUT_MARKER}); pass the root \
             of one",
            root.display()
        );
        return ExitCode::from(2);
    }
    if run(&invocation.selected, &root, invocation.verbose) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
