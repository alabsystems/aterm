// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_spec::{derive::Model, interp::State as ModelState, verify};

const NOW: i64 = 1_790_000_000;

#[derive(Clone, Copy)]
enum Fault {
    None,
    IgnoreIdentity,
    UnknownBecomesZero,
}

fn sample() -> accounts::Sample {
    accounts::Sample {
        session_pct: Some(95),
        session_reset: Some(NOW + 10_800),
        week_pct: Some(20),
        week_reset: Some(NOW + 259_200),
        at: NOW,
        ..Default::default()
    }
}

fn project(case: i64, fault: Fault) -> [(&'static str, i64); 9] {
    let labels = if matches!(case, 7..=10) {
        vec!["current", "alternate", "second"]
    } else {
        vec!["current", "alternate"]
    };
    let roster = Roster {
        rows: labels
            .iter()
            .map(|label| accounts::Row {
                label: (*label).into(),
                dir: Some(format!("/synthetic/{label}")),
                ..Default::default()
            })
            .collect(),
    };
    let mut state = State::default();
    let mut logins = Logins::default();
    accounts::note_auth(
        &mut state,
        "current",
        if case == 11 {
            accounts::Auth::SignedOut
        } else {
            accounts::Auth::Ok
        },
        NOW - 10,
    );
    logins.labels.insert(
        "current".into(),
        accounts::Binding {
            wrong_at: Some(NOW - 20),
            ..Default::default()
        },
    );
    for label in &labels {
        accounts::note_sample(&mut state, label, sample(), NOW);
    }
    let mut enrolled = labels
        .iter()
        .map(|label| (*label).to_owned())
        .collect::<BTreeSet<_>>();
    match case {
        2 | 3 => {
            state.prepared_accounts.insert("current".into());
        }
        5 => {
            state.prepared_accounts.insert("alternate".into());
        }
        6 => {
            enrolled.remove("alternate");
        }
        12 => {
            accounts::note_auth(&mut state, "alternate", accounts::Auth::SignedOut, NOW - 5);
        }
        13 => {
            accounts::note_wall(
                &mut state,
                "alternate",
                "five_hour",
                Some(NOW + 3600),
                NOW - 5,
                NOW,
            );
        }
        14 => state.note_lease("alternate", "foreign-machine", NOW + 3600, NOW - 5),
        _ => {}
    }
    let me = Me {
        account: Some("current"),
        left: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let point = match case {
        10 => None,
        2..=4 => Some(Point::TurnEnd),
        _ => Some(Point::Wall {
            window: "five_hour".into(),
            at: None,
        }),
    };
    let policy_labels = point
        .as_ref()
        .map(|point| {
            let effective = accounts::effective_ranking_point(&roster, &state, &me, point, NOW);
            accounts::candidates(
                &roster,
                &state,
                &enrolled,
                me.label(),
                effective.as_ref(),
                NOW,
            )
            .into_iter()
            .filter_map(|(label, verdict)| verdict.is_ok().then_some(label))
            .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let deny = match case {
        1 | 3 | 9 | 11 => BTreeSet::new(),
        7 => BTreeSet::from(["second".to_owned()]),
        8 | 10 => BTreeSet::from(["alternate".to_owned(), "second".to_owned()]),
        _ => BTreeSet::from(["alternate".to_owned()]),
    };
    let admitted = policy_labels.difference(&deny).count();
    let denied = policy_labels.intersection(&deny).count();
    let mut progress = AccountProgress::unknown(NOW);
    progress.account = Some("current".into());
    progress.phase = ProgressPhase::Waiting;
    progress.wall = Some("five_hour".into());
    progress.next_at = Some(NOW + 20);
    progress.details.push("captured recovery decision".into());
    let captured = (
        roster.clone(),
        state.clone(),
        logins.clone(),
        enrolled.clone(),
        progress.clone(),
    );
    let mut checked = Vec::new();
    match fault {
        Fault::IgnoreIdentity => enrich_inventory(
            &mut progress,
            &roster,
            &roster,
            &state,
            &logins,
            &enrolled,
            &me,
            point.as_ref(),
        ),
        Fault::None | Fault::UnknownBecomesZero => enrich_inventory_with_identity(
            &mut progress,
            &roster,
            &roster,
            &state,
            &logins,
            &enrolled,
            &me,
            point.as_ref(),
            |label| {
                checked.push(label.to_owned());
                !deny.contains(label)
            },
        ),
    }
    if matches!(fault, Fault::UnknownBecomesZero) {
        // Unsafe projection: turn an unjudged inventory into a zero-eligible decision.
        progress.eligible = Some(0);
    }
    let checked_set = checked.iter().cloned().collect::<BTreeSet<_>>();
    let checked_exactly_policy = checked_set == policy_labels && checked_set.len() == checked.len();
    let findings = progress
        .details
        .iter()
        .filter(|line| line.as_str() == IDENTITY_MISMATCH_FINDING)
        .count();
    let preserved = roster == captured.0
        && state == captured.1
        && logins == captured.2
        && enrolled == captured.3
        && progress.known == Some(roster.rows.len())
        && progress.account == captured.4.account
        && progress.phase == captured.4.phase
        && progress.observed_at == captured.4.observed_at
        && progress.wall == captured.4.wall
        && progress.next_at == captured.4.next_at
        && progress
            .details
            .iter()
            .any(|line| line == &captured.4.details[0]);
    [
        ("phase", 1),
        ("policy", i64::try_from(policy_labels.len()).unwrap()),
        ("checked", i64::try_from(checked.len()).unwrap()),
        ("check_set", i64::from(checked_exactly_policy)),
        (
            "eligible",
            progress.eligible.map_or(3, |n| i64::try_from(n).unwrap()),
        ),
        ("admitted", i64::try_from(admitted).unwrap()),
        ("denied", i64::try_from(denied).unwrap()),
        ("findings", i64::try_from(findings).unwrap()),
        ("preserved", i64::from(preserved)),
    ]
}

pub(crate) fn identity_transition(before: &ModelState, action: &str) -> ModelState {
    let mut after = before.clone();
    match action {
        "Case" => {
            after.insert("case", before["case"] + 1);
        }
        "Project" => after.extend(project(before["case"], Fault::None)),
        _ => unreachable!(),
    }
    after
}

fn admits(model: &Model, before: &ModelState, after: &ModelState, action: &str) -> bool {
    let (accepted, why) = verify::validate_transition_tiered(
        model,
        &[],
        before,
        after,
        Some(action),
        "account progress captured identity",
    );
    eprintln!("{} case={} {action}: {why}", model.name, before["case"]);
    accepted
}

#[test]
fn progress_counts_only_policy_and_captured_identity_candidates() {
    let model = aterm_models_harness::account_progress_identity_model();
    let coverage = verify::prove_and_catch_scalar(&model, "account progress captured identity");
    eprintln!("{}: {coverage:?}", model.name);
    if verify::checkers_required() {
        assert_eq!(coverage, verify::Covered::InterpreterAndTy);
    }
    let mut projections = 0;
    let mut fixtures = 0;
    for before in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &before) {
                let after = identity_transition(&before, action.name);
                assert!(
                    admits(&model, &before, &after, action.name),
                    "{before:?} -> {after:?}"
                );
                projections += usize::from(action.name == "Project");
                fixtures += usize::from(action.name == "Case");
            }
        }
    }
    assert_eq!(projections, 15);
    assert_eq!(fixtures, 14);
    let mut before = model.init_state();
    let mut ignored = before.clone();
    ignored.extend(project(0, Fault::IgnoreIdentity));
    assert!(!admits(&model, &before, &ignored, "Project"));
    assert!(!model.check_invariant("CountUsesCapturedIdentity", &ignored));
    assert!(!model.check_invariant("DenialFindingIsTruthful", &ignored));
    before.insert("case", 10);
    let mut zero = before.clone();
    zero.extend(project(10, Fault::UnknownBecomesZero));
    assert!(!admits(&model, &before, &zero, "Project"));
    assert!(!model.check_invariant("UnjudgedRemainsUnknown", &zero));
    eprintln!(
        "{}: {projections} production projections, {fixtures} fixture transitions, 2 executable negative controls; {coverage:?}",
        model.name
    );
}
