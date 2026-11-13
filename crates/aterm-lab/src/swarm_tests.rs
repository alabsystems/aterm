// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn two_login_fixture(dir: &Path) -> (Roster, BTreeSet<String>, String, String) {
    let now = 1_793_150_000;
    let a = dir.join("login-a");
    let b = dir.join("login-b");
    write_enrolled_login(&a, LABEL_A, now).expect("enroll a");
    write_enrolled_login(&b, LABEL_B, now).expect("enroll b");
    let toml = format!(
        "[[account]]\nlabel = \"{LABEL_A}\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
         [[account]]\nlabel = \"{LABEL_B}\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
        a.display(),
        b.display(),
    );
    let roster = Roster::parse(&toml).expect("valid roster");
    let enrolled = accounts::enrolled(&roster);
    (
        roster,
        enrolled,
        a.to_string_lossy().into_owned(),
        b.to_string_lossy().into_owned(),
    )
}

/// Production candidate selection rotates `swarm-a -> swarm-b` and grows `rotations[]` when a
/// 100% usage-tap reading lands; a mutant candidate filter that always stays leaves
/// `rotations[]` empty and fails [`judge_evidence`].
#[test]
fn usage_tap_wall_grows_rotations_and_always_stay_control_fails() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
    let (roster, enrolled, login_a, login_b) = two_login_fixture(&root);
    let now = 1_793_150_100;

    let mut prod_state = accounts::State::default();
    let (moved, rotations) = simulate_tap_wall_step(
        &roster,
        &enrolled,
        &mut prod_state,
        "s-1",
        LABEL_A,
        now,
        CandidatePolicy::Production,
    );
    assert_eq!(moved.as_deref(), Some(LABEL_B));
    assert_eq!(rotations, 1, "rotations[] must grow on production policy");
    let proven_events = accounts::prove(&mut prod_state, LABEL_B, now + 5);
    assert!(
        proven_events
            .iter()
            .any(|e| e.kind == accounts::Kind::Proven),
        "{proven_events:?}"
    );

    let good = Evidence {
        enrolled: enrolled.clone(),
        rotations_before_wall: 0,
        rotations_after: prod_state.rotations.len(),
        active_after: prod_state.active.clone(),
        proven_after: prod_state.proven,
        a_limited: prod_state.accounts.get(LABEL_A).is_some_and(|a| {
            a.low_until(now).is_some() && a.sample.as_ref().and_then(|s| s.session_pct) == Some(100)
        }),
        launches: vec![
            (101, login_a.clone(), "--settings {}".into()),
            (
                202,
                login_b.clone(),
                format!("--settings {{}} --resume {CONVERSATION}"),
            ),
        ],
        turns_on_b: 1,
        ledger_proven_b: true,
    };
    assert_eq!(
        judge_evidence(&good, &login_a, &login_b),
        Vec::<String>::new()
    );

    // Negative control: candidate filter that always stays.
    let mut stay_state = accounts::State::default();
    let (stay_moved, stay_rotations) = simulate_tap_wall_step(
        &roster,
        &enrolled,
        &mut stay_state,
        "s-1",
        LABEL_A,
        now,
        CandidatePolicy::AlwaysStay,
    );
    assert_eq!(stay_moved, None);
    assert_eq!(stay_rotations, 0, "AlwaysStay never grows rotations[]");

    let stay_ev = Evidence {
        rotations_after: stay_state.rotations.len(),
        active_after: stay_state.active.clone(),
        proven_after: stay_state.proven,
        launches: vec![(101, login_a.clone(), "--settings {}".into())],
        turns_on_b: 0,
        ledger_proven_b: false,
        ..good
    };
    let bad = judge_evidence(&stay_ev, &login_a, &login_b);
    assert!(
        bad.iter().any(|m| m.contains("rotations[] did not grow")),
        "AlwaysStay control must fail judge on rotations[]: {bad:?}"
    );
}

#[test]
fn cli_parsing_covers_flags_and_negative_cases() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    assert_eq!(parse_args(&s(&["rotate"])).unwrap(), RotateOpts::default());
    assert_eq!(
        parse_args(&s(&["rotate", "--aterm", "/tmp/aterm", "--keep"])).unwrap(),
        RotateOpts {
            aterm: Some("/tmp/aterm".into()),
            keep: true,
        }
    );
    assert!(parse_args(&s(&[])).is_err());
    assert!(parse_args(&s(&["other"])).is_err());
    assert!(parse_args(&s(&["rotate", "--unknown"])).is_err());
}
