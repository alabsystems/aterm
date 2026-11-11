// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.path(), dir.path().join("cycle")).unwrap();
        let (found, faults) = obligations(dir.path());
        assert!(found.is_empty());
        assert!(
            !faults.is_empty(),
            "directory symlink cycle does not recurse"
        );
    }
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
