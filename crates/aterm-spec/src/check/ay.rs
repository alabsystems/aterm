// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `ay` certificate bundles: hand-encoded SMT-LIB2 obligations under
//! `aterm-spec-models/proofs/ay`, each declaring the verdict it must get.
//!
//! An obligation states its verdict with the standard `(set-info :status
//! unsat|sat)`, so a file is held the moment it is written and none can be
//! left out of a list; `(set-info :aterm-cost minutes)` marks one only
//! `--full` solves. `ay` reads neither, so the declarations never change a
//! solve.
//!
//! One solve, one attributed verdict: every solve reports the build that
//! answered (`build.stamp=` on stderr), and the whole run must be answered by
//! ONE build, because a correct verdict from a solver that will not name
//! itself is not re-checkable evidence. `--no-proof` keeps a solve from
//! rewriting the tracked proof sidecars beside its input.

use std::path::{Path, PathBuf};

/// The solver's budget for one obligation that is not minutes-class: every
/// one answers in seconds, so this only bounds a regression.
const TIMEOUT: &str = "-t:120000";

/// What a solve must answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Want {
    /// The obligation is proved (its negation is unsatisfiable).
    Unsat,
    /// A control: the witness exists.
    Sat,
}

impl Want {
    const fn word(self) -> &'static str {
        match self {
            Want::Unsat => "unsat",
            Want::Sat => "sat",
        }
    }
}

/// One obligation and what it declares.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Obligation {
    pub(crate) path: PathBuf,
    pub(crate) want: Want,
    /// Minutes-class: solved only by `--full`, with no timeout.
    pub(crate) minutes: bool,
}

/// What an obligation's text declares: its verdict, and whether it is
/// minutes-class.
pub(crate) fn declared(text: &str) -> Result<(Want, bool), String> {
    let mut want = None;
    let mut minutes = false;
    for line in text.lines().map(str::trim) {
        let Some(info) = line
            .strip_prefix("(set-info ")
            .and_then(|rest| rest.split(')').next())
        else {
            continue;
        };
        match info.split_whitespace().collect::<Vec<_>>().as_slice() {
            [":status", "unsat"] => want = Some(Want::Unsat),
            [":status", "sat"] => want = Some(Want::Sat),
            [":status", other] => return Err(format!("declares `:status {other}`")),
            [":aterm-cost", "minutes"] => minutes = true,
            [":aterm-cost", other] => return Err(format!("declares `:aterm-cost {other}`")),
            _ => {}
        }
    }
    want.map(|want| (want, minutes))
        .ok_or_else(|| "declares no `(set-info :status unsat|sat)`".to_string())
}

/// Every obligation under `root`, sorted by path, or why a file declares
/// nothing it can be held to.
pub(crate) fn obligations(root: &Path) -> (Vec<Obligation>, Vec<String>) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if path.extension().is_some_and(|e| e == "smt2") {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    if let Err(e) = walk(root, &mut files) {
        return (Vec::new(), vec![format!("{}: {e}", root.display())]);
    }
    files.sort();
    if files.is_empty() {
        return (
            Vec::new(),
            vec![format!("{}: no .smt2 obligation was found", root.display())],
        );
    }
    let (mut found, mut faults) = (Vec::new(), Vec::new());
    for path in files {
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| declared(&text))
        {
            Ok((want, minutes)) => found.push(Obligation {
                path,
                want,
                minutes,
            }),
            Err(why) => faults.push(format!("{}: {why}", path.display())),
        }
    }
    (found, faults)
}

/// One solve's outcome: what it answered and which build answered it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Answer {
    pub(crate) verdict: String,
    pub(crate) stamp: Option<String>,
}

/// The verdict on `ay solve`'s stdout: its first line that is exactly `sat`,
/// `unsat` or `unknown` (a CHC certificate and the session comments follow).
pub(crate) fn verdict(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| {
            ["sat", "unsat", "unknown"]
                .iter()
                .any(|word| line.eq_ignore_ascii_case(word))
        })
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// The build that answered, from the `build.stamp=` the solver writes to
/// stderr on every solve.
pub(crate) fn stamp(stderr: &str) -> Option<String> {
    stderr.split_whitespace().find_map(|word| {
        word.strip_prefix("build.stamp=")
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

/// Solve one obligation.
// Skip: spawns the solver. Verification tooling.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn solve(ay: &Path, obligation: &Obligation) -> Result<Answer, String> {
    let mut cmd = aterm_uds::spawnfd::command(ay);
    cmd.arg("solve").arg("--no-proof").arg(&obligation.path);
    if !obligation.minutes {
        cmd.arg(TIMEOUT);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run {}: {e}", ay.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} failed ({}); its printed verdict is not a discharge\n--- stdout ---\n{}\n--- stderr ---\n{}",
            ay.display(),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(Answer {
        verdict: verdict(&String::from_utf8_lossy(&out.stdout)),
        stamp: stamp(&String::from_utf8_lossy(&out.stderr)),
    })
}

/// Judge one obligation's answer: the declared verdict, from an attributed
/// build.
pub(crate) fn judge(file: &str, want: Want, answer: &Answer) -> Result<(), String> {
    let Some(build) = &answer.stamp else {
        return Err(format!(
            "{file}: got `{}` with no build.stamp — the solver did not say which build answered",
            answer.verdict
        ));
    };
    if answer.verdict == want.word() {
        Ok(())
    } else {
        Err(format!(
            "{file}: got `{}`, want `{}` [ay {build}]",
            answer.verdict,
            want.word()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A printed verdict is not a successful solve when its process fails or
    /// dies afterwards. Only private scratch stand-ins run here, never ay.
    #[cfg(unix)]
    #[test]
    fn an_unsuccessful_solver_cannot_supply_an_attributed_verdict() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let solver = dir.path().join("solver");
        let obligation = Obligation {
            path: dir.path().join("unused.smt2"),
            want: Want::Unsat,
            minutes: false,
        };
        let run = |end: &str| {
            std::fs::write(
                &solver,
                format!(
                    "#!/bin/sh\nprintf 'unsat\\n'\nprintf 'c ay.session.start build.stamp=fixture\\n' >&2\n{end}\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&solver, std::fs::Permissions::from_mode(0o700)).unwrap();
            solve(&solver, &obligation)
        };
        let healthy = run("exit 0").expect("a successful attributed solve");
        assert_eq!(judge("control", Want::Unsat, &healthy), Ok(()));
        for end in ["exit 1", "kill -KILL $$"] {
            assert!(
                run(end).is_err(),
                "the solver's unsuccessful process must refuse its printed verdict: {end}"
            );
        }
    }

    #[test]
    fn an_empty_requested_proof_directory_is_not_a_discharge() {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let (found, faults) = obligations(dir.path());
        assert!(found.is_empty());
        assert!(
            !faults.is_empty(),
            "an empty proof walk must fail the requested check"
        );
    }

    #[test]
    fn an_obligation_declares_its_verdict_and_its_cost() {
        let proof = "; header\n(set-logic QF_LIA)\n(set-info :status unsat)\n(check-sat)\n";
        assert_eq!(declared(proof), Ok((Want::Unsat, false)));
        let control = "(set-logic HORN)\n(set-info :status sat)\n";
        assert_eq!(declared(control), Ok((Want::Sat, false)));
        let slow = "(set-info :status unsat)\n(set-info :aterm-cost minutes)\n";
        assert_eq!(declared(slow), Ok((Want::Unsat, true)));
        assert!(declared("(set-logic QF_BV)\n(check-sat)\n").is_err());
        assert!(declared("(set-info :status unknown)\n").is_err());
        assert!(declared("(set-info :status unsat)\n(set-info :aterm-cost hours)\n").is_err());
    }

    #[test]
    fn the_verdict_is_the_first_verdict_line() {
        assert_eq!(verdict("c ay.session.start x\nunsat\n(model)\n"), "unsat");
        assert_eq!(verdict("SAT\nunsat\n"), "sat");
        assert_eq!(verdict("unsat\n;; AY CHC Certificate: UNSAFE\n"), "unsat");
        assert_eq!(verdict("no verdict\n"), "");
    }

    #[test]
    fn a_verdict_counts_only_when_its_build_is_named() {
        let named = Answer {
            verdict: "unsat".into(),
            stamp: stamp("c ay.session.start build.stamp=8256@2026 x"),
        };
        assert_eq!(named.stamp.as_deref(), Some("8256@2026"));
        assert_eq!(judge("f.smt2", Want::Unsat, &named), Ok(()));
        assert!(judge("f.smt2", Want::Sat, &named).is_err());
        let anonymous = Answer {
            verdict: "unsat".into(),
            stamp: stamp("c ay.session.start"),
        };
        assert!(judge("f.smt2", Want::Unsat, &anonymous).is_err());
    }
}
