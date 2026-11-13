// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify soak [--ledger PATH]` runner for `aterm_update::restore_soak`.

use std::path::PathBuf;

use aterm_update::recovery_ledger::{default_ledger_path, read_rows};
use aterm_update::restore_soak::{P4, judge};

use crate::exit;

/// `verify soak`'s usage.
pub const USAGE: &str = "targo --unverified verify soak [--ledger PATH]

The crash restore's soak: how the on-by-default keeper does, judged from this machine's
recovery ledger (the installed app's; --ledger reads another file). It judges the
machine, not the tree: the days the keeper link has been on without a break, the
launches that recovered sessions after a kill, the share of those sessions that came
back exactly, none blank, and no kill the keeper held and did not bring back.

Exit: 0 met, 1 not met (yet), 2 usage, 3 could not run (no ledger).
";

/// Run `verify soak` with the arguments after `soak`.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    let mut ledger: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return exit::PASS;
            }
            "--ledger" => match it.next() {
                Some(path) if !path.is_empty() && !path.starts_with('-') => {
                    ledger = Some(PathBuf::from(path));
                }
                _ => {
                    eprintln!("verify soak: --ledger needs a path (verify soak --help)");
                    return exit::USAGE;
                }
            },
            other => {
                eprintln!("verify soak: unknown argument `{other}` (verify soak --help)");
                return exit::USAGE;
            }
        }
    }
    let Some(path) = ledger.or_else(default_ledger_path) else {
        eprintln!("verify soak: COULD NOT RUN — no log directory resolves for this user");
        return exit::COULD_NOT_RUN;
    };
    if !path.is_file() {
        eprintln!(
            "verify soak: COULD NOT RUN — no recovery ledger at {} (the installed app writes \
             one row per windowed launch; a dev build, a test and a headless launch write none)",
            path.display()
        );
        return exit::COULD_NOT_RUN;
    }
    let rows = read_rows(&path);
    let Some(now) = aterm_time::unix_secs() else {
        eprintln!("verify soak: COULD NOT RUN — the clock reads before the epoch");
        return exit::COULD_NOT_RUN;
    };
    let soak = judge(&rows, now, &P4);
    println!(
        "=== verify soak: the crash restore (the on-by-default keeper) — {} rows of {} ===",
        soak.rows,
        path.display()
    );
    for line in soak.lines() {
        println!("  {line}");
    }
    if soak.met() { exit::PASS } else { exit::FAILED }
}

#[cfg(test)]
#[path = "soak_tests.rs"]
mod tests;
