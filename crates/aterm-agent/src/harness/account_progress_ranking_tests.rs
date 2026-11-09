// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;

fn sample(percent: u32) -> accounts::Sample {
    accounts::Sample {
        session_pct: Some(percent),
        session_reset: Some(NOW + 10_800),
        week_pct: Some(20),
        week_reset: Some(NOW + 259_200),
        at: NOW,
        ..Default::default()
    }
}

fn project(case: i64, raw_ranking: bool) -> [(&'static str, i64); 7] {
    let mut roster = Roster {
        rows: ["current", "alternate"]
            .map(|label| accounts::Row {
                label: label.into(),
                dir: Some(format!("/synthetic/{label}")),
                ..Default::default()
            })
            .into(),
    };
    let mut state = State {
        active: Some("current".into()),
        proven: true,
        ..Default::default()
    };
    accounts::note_sample(
        &mut state,
        "current",
        sample(if case == 9 { 95 } else { 10 }),
        NOW,
    );
    accounts::note_sample(
        &mut state,
        "alternate",
        sample(if matches!(case, 8 | 9 | 16 | 19) {
            10
        } else {
            95
        }),
        NOW,
    );
    let mut enrolled = BTreeSet::from(["current".into(), "alternate".into()]);
    match case {
        0 | 1 | 6 | 7 | 14 | 15 | 17 | 21 | 22 => {
            state.prepared_accounts.insert("current".into());
        }
        2 | 8 => state.note_lease("current", "foreign-machine", NOW + 3600, NOW - 20),
        3 => roster.rows[0].held_by = Some("foreign-machine".into()),
        10 | 13 => {
            state.unrecorded = true;
            state.home_accounts.insert("alternate".into());
            if case == 13 {
                state.home_accounts.insert("current".into());
            }
        }
        12 => state.note_lease("current", "foreign-machine", NOW, NOW - 20),
        _ => {}
    }
    if matches!(case, 7 | 11 | 20) {
        enrolled.remove("alternate");
    }
    if case == 17 {
        state.prepared_accounts.insert("alternate".into());
    }
    let logins = Logins::default();
    let me = Me {
        account: (case != 14).then_some("current"),
        left: match case {
            14 => Some("current"),
            15 => Some("superseded-other"),
            _ => None,
        },
        default_home: case != 21,
        route: crate::harness::accounts::AuthRoute::Stored.unless(case == 22),
    };
    let point = match case {
        0 | 16 => None,
        5 | 6 | 11 | 18 => Some(Point::Wall {
            window: "five_hour".into(),
            at: (case == 18).then_some(NOW - 1),
        }),
        _ => Some(Point::TurnEnd),
    };
    let advice = point.as_ref().map(|point| {
        accounts::advise(
            &roster,
            &state,
            &enrolled,
            &me,
            point,
            accounts::Mode::Early,
            NOW,
        )
    });
    match case {
        21 => assert_eq!(
            advice,
            Some(accounts::Advice::Stay(accounts::StayWhy::OtherHome))
        ),
        22 => assert_eq!(
            advice,
            Some(accounts::Advice::Stay(accounts::StayWhy::NotAStoredLogin))
        ),
        _ => {}
    }
    let moved = matches!(&advice, Some(accounts::Advice::Move { to, .. }) if to == "alternate");
    let mut progress = AccountProgress::unknown(NOW);
    progress.account = Some("current".into());
    progress.phase = if moved {
        ProgressPhase::Switching {
            to: "alternate".into(),
        }
    } else {
        ProgressPhase::Waiting
    };
    progress.next_at = Some(NOW + 20);
    let before = (
        roster.clone(),
        state.clone(),
        logins.clone(),
        enrolled.clone(),
        progress.phase.clone(),
    );
    let effective = point
        .as_ref()
        .map(|point| accounts::effective_ranking_point(&roster, &state, &me, point, NOW));
    if matches!(case, 21 | 22) {
        assert!(matches!(
            effective.as_ref(),
            Some(std::borrow::Cow::Borrowed(Point::TurnEnd))
        ));
    }
    let ranked = effective.as_ref().map(|point| {
        accounts::candidates(&roster, &state, &enrolled, me.label(), point.as_ref(), NOW)
    });
    if let Some(point @ Point::Wall { .. }) = point.as_ref() {
        assert_eq!(effective.as_ref().unwrap().as_ref(), point);
        assert!(matches!(
            effective.as_ref(),
            Some(std::borrow::Cow::Borrowed(_))
        ));
        assert!(!matches!(
            &advice,
            Some(accounts::Advice::Move {
                yields: Some(_),
                ..
            })
        ));
    }
    let candidates = ranked.as_ref().map_or(2, |rows| {
        rows.iter().filter(|(_, verdict)| verdict.is_ok()).count() as i64
    });
    let candidate_low_rejected = ranked.as_ref().is_some_and(|rows| {
        rows.iter().any(|(label, verdict)| {
            label == "alternate" && *verdict == Err(accounts::WhyNot::LowAheadOfAWall)
        })
    });
    enrich_inventory(
        &mut progress,
        &roster,
        &roster,
        &state,
        &logins,
        &enrolled,
        &me,
        point.as_ref(),
    );
    if raw_ranking {
        // The previous implementation ranked the raw trigger after advice had yielded.
        let raw = point
            .as_ref()
            .map(|point| accounts::candidates(&roster, &state, &enrolled, me.label(), point, NOW));
        progress.eligible = raw
            .as_ref()
            .map(|rows| rows.iter().filter(|(_, verdict)| verdict.is_ok()).count());
        for (label, verdict) in raw.into_iter().flatten() {
            if verdict == Err(accounts::WhyNot::LowAheadOfAWall) {
                progress.details.push(format!(
                    "{label}: {}",
                    accounts::WhyNot::LowAheadOfAWall.as_str()
                ));
            }
        }
    }
    let low_reason = format!("alternate: {}", accounts::WhyNot::LowAheadOfAWall.as_str());
    let low_rejected = progress.details.iter().any(|line| line == &low_reason);
    let unchanged = roster == before.0
        && state == before.1
        && logins == before.2
        && enrolled == before.3
        && progress.phase == before.4
        && progress.account.as_deref() == Some("current")
        && progress.observed_at == NOW
        && progress.next_at == Some(NOW + 20)
        && progress.known == Some(2);
    [
        ("eligible", progress.eligible.map_or(2, |n| n as i64)),
        ("candidates", candidates),
        ("moved", i64::from(moved)),
        ("low_rejected", i64::from(low_rejected)),
        ("candidate_low_rejected", i64::from(candidate_low_rejected)),
        ("unchanged", i64::from(unchanged)),
        ("phase", 1),
    ]
}

pub(crate) fn ranking_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "Case" => {
            next.insert("case", state["case"] + 1);
        }
        "Project" => next.extend(project(state["case"], false)),
        _ => unreachable!(),
    }
    next
}

fn admits(
    model: &aterm_spec::derive::Model,
    before: &aterm_spec::interp::State,
    after: &aterm_spec::interp::State,
    action: &str,
) -> bool {
    let (accepted, why) = aterm_spec::verify::validate_transition_tiered(
        model,
        &[("Buggy", 0)],
        before,
        after,
        Some(action),
        "account recovery projection",
    );
    eprintln!("{} case={} {action}: {why}", model.name, before["case"]);
    accepted
}

#[test]
fn inventory_refines_the_effective_advice_candidates() {
    let model = aterm_models_harness::account_progress_ranking_model();
    let coverage =
        aterm_spec::verify::prove_and_catch_tiered(&model, "account recovery projection")
            .expect("scalar account recovery projection is executable");
    eprintln!("{}: {coverage:?}", model.name);
    if aterm_spec::verify::checkers_required() {
        assert_eq!(coverage, aterm_spec::verify::Covered::InterpreterAndTy);
    }
    let mut projects = 0;
    let mut fixtures = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = ranking_transition(&state, action.name);
                assert!(
                    admits(&model, &state, &next, action.name),
                    "{}: {state:?} -> {next:?}",
                    action.name
                );
                projects += usize::from(action.name == "Project");
                fixtures += usize::from(action.name == "Case");
            }
        }
    }
    assert_eq!(projects, 23);
    assert_eq!(fixtures, 22);
    let mut before = model.init_state();
    before.insert("case", 1);
    let mut obsolete = before.clone();
    obsolete.extend(project(1, true));
    assert!(!admits(&model, &before, &obsolete, "Project"));
    assert!(!model.check_invariant("ProjectionMatchesCandidates", &obsolete));
    assert!(!model.check_invariant("SelectedDestinationCounted", &obsolete));
    assert!(!model.check_invariant("ExplanationsMatchCandidates", &obsolete));
    eprintln!(
        "{}: {projects} production projections, {fixtures} fixture transitions, 1 executable negative control; {coverage:?}",
        model.name
    );
}
