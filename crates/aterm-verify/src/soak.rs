// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `verify soak [--ledger PATH]`: the crash restore's soak, keeper P4's precondition
//! (`docs/DESIGN-crash-restore-2026-10-20.md` §7, `docs/DESIGN-pty-keeper-2026-09-26.md` §7).
//!
//! NOT A CHECK OF THE TREE. It reads this machine's recovery ledger — the installed app's,
//! one row per windowed launch, in the log directory, or the file `--ledger` names — and
//! prints what `aterm_update::restore_soak` judges of it against the bar P4 waits on
//! (`restore_soak::P4`): the days the keeper link has been on, the real kills the keeper
//! brought back, how many sessions came back exactly, none blank, none missed. The judge is
//! the updater crate's own, over the same reader `aterm doctor` uses, so the rows are read
//! by one parser.
//!
//! Exit: `0` the bar is met, `1` it is not (yet), `2` usage, `3` could not run (no ledger
//! at the path: the installed app writes it, a dev build or a test never does).

use std::path::PathBuf;

use aterm_update::recovery_ledger::{default_ledger_path, read_rows};
use aterm_update::restore_soak::{P4, judge};

use crate::exit;

/// `verify soak`'s usage.
pub const USAGE: &str = "targo --unverified verify soak [--ledger PATH]

The crash restore's soak: keeper P4's precondition, judged from this machine's
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
        "=== verify soak: the crash restore (keeper P4's precondition) — {} rows of {} ===",
        soak.rows,
        path.display()
    );
    for line in soak.lines() {
        println!("  {line}");
    }
    if soak.met() { exit::PASS } else { exit::FAILED }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn usage_errors_are_exit_two() {
        assert_eq!(run(&args(&["--ledger"])), exit::USAGE);
        assert_eq!(run(&args(&["--ledger", "--help"])), exit::USAGE);
        assert_eq!(run(&args(&["extra"])), exit::USAGE);
        assert_eq!(run(&args(&["--help"])), exit::PASS);
    }

    #[test]
    fn no_ledger_could_not_run() {
        let dir = aterm_tempfile::TempDir::new().expect("a scratch dir");
        let missing = dir.path().join("recovery-ledger.log");
        let missing = missing.to_string_lossy().into_owned();
        assert_eq!(
            run(&args(&["--ledger", missing.as_str()])),
            exit::COULD_NOT_RUN
        );
    }

    /// A ledger of one launch with the keeper on and no kill is read and judged: not met.
    #[test]
    fn a_young_ledger_is_not_met() {
        let dir = aterm_tempfile::TempDir::new().expect("a scratch dir");
        let path = dir.path().join("recovery-ledger.log");
        std::fs::write(
            &path,
            "v=1 at=1790000000 pid=7 started=1789999999 build=1 version=0.1.0 launch=cold \
             prev=none prev_pid=? prev_build=? prev_version=? prev_uptime=? prev_stall=? \
             prev_pressure=? prev_trial=0 keeper=on\n",
        )
        .expect("write the ledger");
        assert_eq!(read_rows(&path).len(), 1, "the row parses");
        let path = path.to_string_lossy().into_owned();
        assert_eq!(run(&args(&["--ledger", path.as_str()])), exit::FAILED);
    }
}
