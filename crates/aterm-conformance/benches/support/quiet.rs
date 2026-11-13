// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The quiet machine for a conformance matrix (`docs/DESIGN-adaptive-build-2026-10-24.md`,
//! QUIET-ADMIT). Only this crate's own matrices mount it: the other crates whose benches share the
//! runner beside it do not depend on aterm-capacity.

use std::process::ExitCode;

/// Run `matrix` after its own measured admission and fresh postcompile CPU observation.
/// An ancestor can cover resources, never supply this matrix's CPU evidence. Forced or
/// immediate admission cannot qualify an acceptance callback. A machine that stays busy through
/// its patience, or a timing hold that ends before the matrix does, runs it unqualified: each
/// reason is a `measure <target>: unqualified — <why>` line and the rows' own verdict stands
/// (a busy machine never blocks a release: the row is named, not counted red). Blind, unknown or
/// forced admission takes no row.
/// A ledger that cannot be used refuses the run before any row opens a window.
/// A bad `--admit` is a usage error (2); an interrupt that ends the wait is
/// COULD NOT RUN (3), no row taken.
pub fn run(target: &str, matrix: impl FnOnce() -> ExitCode) -> ExitCode {
    let (mode, _) = match parse_args(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(why) => {
            eprintln!("measure {target}: {why}");
            return ExitCode::from(2);
        }
    };
    if std::env::args().any(|arg| arg == "--list") {
        return matrix();
    }
    let need = aterm_capacity::Need::timing(format!("measure {target}")).runs_loaded();
    let mut quiet = match aterm_capacity::admit(need, mode) {
        Ok(claim) => claim,
        Err(e @ aterm_capacity::Error::Stopped { .. }) => {
            eprintln!(
                "measure {target}: COULD NOT RUN — its quiet claim was {e}; no row was taken"
            );
            return ExitCode::from(3);
        }
        Err(e) => {
            eprintln!(
                "measure {target}: COULD NOT RUN — cannot acquire its exclusive timing claim: {e}"
            );
            return ExitCode::from(3);
        }
    };
    // The benchmark executable is already compiled. This sample belongs to the matrix entry,
    // never to an ancestor's earlier build/admission window.
    let entry = match quiet.stated_entry() {
        Ok(entry) => entry,
        Err(why) => {
            eprintln!(
                "measure {target}: COULD NOT RUN — {why}; no acceptance callback was invoked"
            );
            return ExitCode::from(3);
        }
    };
    let window = entry.window;
    let evidence = quiet.record().timing;
    let expires = evidence
        .admitted_ms
        .zip(quiet.record().need.patience)
        .and_then(|(at, p)| at.checked_add(u64::try_from(p.hold.as_millis()).ok()?));
    eprintln!(
        "measure {target}: timing phase=compiled_benchmark_matrix_entry provider=sysprobe_core_ticks scope=sampled_global_cpu_window;not_continuous_quiet observed_ms={} interval_ns={} cores={} busy_ticks={} total_ticks={} entry_load1={:?} actual_admitted_ms={:?} expires_ms={expires:?}",
        window.observed_ms,
        window.interval_ns,
        window.cores,
        window.busy_ticks,
        window.total_ticks,
        evidence.entry_load1,
        evidence.admitted_ms
    );
    for why in &entry.unqualified {
        eprintln!("{}", unqualified(target, why));
    }
    let verdict = matrix();
    match quiet.timing_protection() {
        Ok(_) => {}
        Err(aterm_capacity::TimingIssue::Expired) => {
            eprintln!(
                "{}",
                unqualified(target, &aterm_capacity::Unqualified::Lapsed)
            );
        }
        Err(why) => {
            eprintln!(
                "measure {target}: COULD NOT RUN — timing protection ended before completion: {why}; matrix cannot qualify"
            );
            return ExitCode::from(3);
        }
    }
    verdict
}

/// The line that states why `target`'s matrix ran unqualified; verify's measure stage reads it.
fn unqualified(target: &str, why: &aterm_capacity::Unqualified) -> String {
    format!("measure {target}: unqualified — {why}")
}

/// Read admission mode and return the bench's own arguments without `--admit` or Cargo's
/// `--bench`. Admission defaults to waiting.
/// # Errors
/// An admission option lacks its mode or names a mode other than `now` or `wait`.
pub fn parse_args(
    args: impl Iterator<Item = String>,
) -> Result<(aterm_capacity::Admit, Vec<String>), String> {
    let mut mode = aterm_capacity::Admit::Wait;
    let mut remaining = Vec::new();
    let mut args = args;
    while let Some(arg) = args.next() {
        if arg == aterm_capacity::Admit::FLAG {
            let word = args.next().unwrap_or_default();
            mode = aterm_capacity::Admit::parse(&word)
                .ok_or_else(|| format!("--admit takes now or wait, not `{word}`"))?;
        } else if arg != "--bench" {
            remaining.push(arg);
        }
    }
    Ok((mode, remaining))
}

#[cfg(test)]
mod tests {
    #[test]
    fn admit_is_read_among_the_benchs_arguments() {
        use super::*;

        let args = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let mode = |w: &[&str]| parse_args(args(w).into_iter()).map(|(mode, _)| mode);
        assert_eq!(mode(&["--bench"]), Ok(aterm_capacity::Admit::Wait));
        assert_eq!(
            mode(&["--bench", "--admit", "now"]),
            Ok(aterm_capacity::Admit::Now)
        );
        assert!(mode(&["--admit", "soon"]).is_err());
        assert!(mode(&["--admit"]).is_err());
    }

    #[test]
    fn diagnostic_arguments_survive_admission_options_in_every_position() {
        use super::*;

        for flag in [
            "--recording-cost-diagnostic",
            "--recording-reference-diagnostic",
        ] {
            let expected: Vec<String> = [flag, "subject", "producer", "output"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            for (word, expected_mode) in [
                ("now", aterm_capacity::Admit::Now),
                ("wait", aterm_capacity::Admit::Wait),
            ] {
                for at in 0..=expected.len() {
                    let mut args = expected.clone();
                    args.insert(at, word.to_owned());
                    args.insert(at, "--admit".to_owned());
                    args.push("--bench".to_owned());
                    let (mode, remaining) = parse_args(args.into_iter()).unwrap();
                    assert_eq!(mode, expected_mode);
                    assert_eq!(remaining, expected);
                }
            }
        }
    }
}
