// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quiet machine for a conformance matrix (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! QUIET-ADMIT). Only this crate's own matrices mount it: the other crates whose benches share the
//! runner beside it do not depend on aterm-capacity.

use std::process::ExitCode;

/// Run `matrix` on the quiet machine: alone among quiet claims once the load is low, at once
/// under `--admit now`, and at once under `verify measure`, whose own claim covers this process.
/// A ledger that cannot be used is said, and the rows run unadmitted: the instrument still refuses
/// a starved take. A bad `--admit` is a usage error (2).
pub fn run(target: &str, matrix: impl FnOnce() -> ExitCode) -> ExitCode {
    let mode = match admit_mode(std::env::args().skip(1)) {
        Ok(mode) => mode,
        Err(why) => {
            eprintln!("measure {target}: {why}");
            return ExitCode::from(2);
        }
    };
    let need = aterm_capacity::Need::timing(format!("measure {target}"));
    let _quiet = match aterm_capacity::admit(need, mode) {
        Ok(claim) => Some(claim),
        Err(e) => {
            eprintln!("measure {target}: {e}; the rows run unadmitted");
            None
        }
    };
    matrix()
}

/// `--admit now|wait` among the bench's arguments (cargo hands it `--bench` too), `wait` when
/// absent.
fn admit_mode(args: impl Iterator<Item = String>) -> Result<aterm_capacity::Admit, String> {
    let mut mode = aterm_capacity::Admit::Wait;
    let mut args = args;
    while let Some(arg) = args.next() {
        if arg == aterm_capacity::Admit::FLAG {
            let word = args.next().unwrap_or_default();
            mode = aterm_capacity::Admit::parse(&word)
                .ok_or_else(|| format!("--admit takes now or wait, not `{word}`"))?;
        }
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    #[test]
    fn admit_is_read_among_the_benchs_arguments() {
        use super::*;

        let args = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let mode = |w: &[&str]| admit_mode(args(w).into_iter());
        assert_eq!(mode(&["--bench"]), Ok(aterm_capacity::Admit::Wait));
        assert_eq!(
            mode(&["--bench", "--admit", "now"]),
            Ok(aterm_capacity::Admit::Now)
        );
        assert!(mode(&["--admit", "soon"]).is_err());
        assert!(mode(&["--admit"]).is_err());
    }
}
