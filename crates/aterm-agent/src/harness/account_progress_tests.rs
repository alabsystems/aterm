// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::host_path::abs;

fn admitted_guidance(
    roster: &Roster,
    logins: &mut Logins,
    at: i64,
    label: &str,
    observe: impl FnOnce(&mut Logins) -> Vec<accounts::Event>,
) {
    let home = aterm_tempfile::tempdir().unwrap();
    let store = accounts::Store::at(home.path());
    let path = home.path().join("accounts.toml");
    std::fs::write(
        &path,
        accounts::append_rows_for(accounts::Cli::Claude, "", &roster.rows).unwrap(),
    )
    .unwrap();
    store.save_logins(logins).unwrap();
    store
        .transact_pool_logins_within(&[], &path, at, &mut None, |raw, _, logins| {
            assert_eq!(raw, roster);
            accounts::admit_workflow(logins, label)?;
            Ok(observe(logins))
        })
        .unwrap();
    *logins = store.load_logins(at).unwrap().0;
    assert!(accounts::current_workflow(roster.row(label).unwrap(), logins).is_some());
}

#[test]
fn known_signed_out_account_is_explained_without_a_sign_in_attempt() {
    let roster = Roster {
        rows: vec![accounts::Row {
            label: "current".into(),
            ..Default::default()
        }],
    };
    let mut state = State::default();
    accounts::note_auth(&mut state, "current", accounts::Auth::SignedOut, 90);
    let mut progress = AccountProgress::unknown(100);
    progress.phase = ProgressPhase::Waiting;
    progress
        .details
        .push("No eligible alternate account".into());
    enrich_inventory(
        &mut progress,
        &roster,
        &roster,
        &state,
        &Logins::default(),
        &BTreeSet::new(),
        &Me {
            account: Some("current"),
            left: None,
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        },
        Some(&Point::Wall {
            window: "seven_day".into(),
            at: Some(90),
        }),
    );
    assert_eq!(progress.known, Some(1));
    assert_eq!(progress.eligible, Some(0));
    assert_eq!(progress.phase, ProgressPhase::Waiting);
    assert!(progress.details[0].contains("current: signed out"));
    assert!(super::super::footer::account_progress_text(&progress).contains("current: signed out"));
}

#[test]
fn current_blocker_leads_only_when_recovery_has_no_eligible_alternate() {
    let roster = Roster {
        rows: ["current", "alternate"]
            .map(|label| accounts::Row {
                label: label.into(),
                dir: Some(format!("/synthetic/{label}")),
                ..Default::default()
            })
            .into(),
    };
    let mut state = State::default();
    let mut logins = Logins::default();
    logins.labels.insert(
        "current".into(),
        accounts::Binding {
            dir: roster.row("current").unwrap().dir.clone(),
            ..Default::default()
        },
    );
    accounts::note_login_end(&mut state, &mut logins, "current", 80);
    accounts::note_sign_in_task(&mut logins, "current", accounts::SignInAction::Login, 90);
    let point = Point::Wall {
        window: "seven_day".into(),
        at: Some(90),
    };
    let me = Me {
        account: Some("current"),
        left: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let generic = "synthetic supervisor reason";
    for (phase, judged, alternate_ready, prefer) in [
        (ProgressPhase::Waiting, true, false, true),
        (ProgressPhase::Limited, true, false, true),
        (ProgressPhase::Waiting, true, true, false),
        (ProgressPhase::Waiting, false, false, false),
        (
            ProgressPhase::SigningIn {
                label: "alternate".into(),
                route: None,
            },
            true,
            false,
            false,
        ),
        (
            ProgressPhase::Switching {
                to: "alternate".into(),
            },
            true,
            false,
            false,
        ),
        (
            ProgressPhase::Verifying {
                to: "alternate".into(),
            },
            true,
            false,
            false,
        ),
        (ProgressPhase::Ready, true, false, false),
        (ProgressPhase::Probing, true, false, false),
        (ProgressPhase::Watching, true, false, false),
        (ProgressPhase::Disabled, true, false, false),
        (ProgressPhase::Unknown, true, false, false),
    ] {
        let mut progress = AccountProgress::unknown(100);
        progress.phase = phase.clone();
        progress.wall = Some("weekly".into());
        progress.next_at = Some(160);
        progress.details.push(generic.into());
        let mut enrolled = BTreeSet::from(["current".into()]);
        if alternate_ready {
            enrolled.insert("alternate".into());
        }
        enrich_inventory(
            &mut progress,
            &roster,
            &roster,
            &state,
            &logins,
            &enrolled,
            &me,
            judged.then_some(&point),
        );
        assert_eq!(progress.phase, phase);
        assert_eq!(progress.known, Some(2));
        assert_eq!(
            progress.eligible,
            judged.then_some(usize::from(alternate_ready))
        );
        assert_eq!(progress.next_at, Some(160));
        assert_eq!(progress.wall.as_deref(), Some("weekly"));
        assert!(progress.details.iter().any(|line| line == generic));
        if prefer {
            assert!(progress.details[0].starts_with("current:"));
            assert!(
                super::super::footer::account_progress_text(&progress)
                    .contains(&progress.details[0])
            );
        } else {
            assert_eq!(progress.details[0], generic);
        }
    }
}

#[test]
fn current_login_failure_is_visible_without_becoming_a_rotation_candidate() {
    let roster = Roster {
        rows: vec![
            accounts::Row {
                label: "current".into(),
                dir: Some(abs("/synthetic/current")),
                ..Default::default()
            },
            accounts::Row {
                label: "alternate".into(),
                ..Default::default()
            },
        ],
    };
    let mut state = State::default();
    let mut logins = Logins::default();
    logins.labels.insert(
        "current".into(),
        accounts::Binding {
            dir: roster.row("current").unwrap().dir.clone(),
            ..Default::default()
        },
    );
    accounts::note_login_end(&mut state, &mut logins, "current", 80);
    admitted_guidance(&roster, &mut logins, 90, "current", |logins| {
        accounts::note_sign_in_task(logins, "current", accounts::SignInAction::Login, 85);
        accounts::note_sign_in_attempt(
            logins,
            "current",
            accounts::AttemptOutcome::NoRoute,
            "profile(none)",
            90,
        )
    });
    // `alternate`, the default login, is signed in here as `accounts::enrolled_for` holds it.
    let enrolled = BTreeSet::from(["current".into(), "alternate".into()]);
    let project = |state: &State| {
        let mut progress = AccountProgress::unknown(100);
        progress.phase = ProgressPhase::Waiting;
        enrich_inventory(
            &mut progress,
            &roster,
            &roster,
            state,
            &logins,
            &enrolled,
            &Me {
                account: Some("current"),
                left: None,
                default_home: true,
                route: crate::harness::accounts::AuthRoute::Stored,
            },
            Some(&Point::Wall {
                window: "seven_day".into(),
                at: Some(90),
            }),
        );
        progress
    };
    let failed = project(&state);
    assert_eq!((failed.known, failed.eligible), (Some(2), Some(1)));
    assert_eq!(failed.phase, ProgressPhase::Waiting);
    assert!(
        failed
            .details
            .contains(&accounts::SignInAction::Login.next_step(accounts::Cli::Claude, "current"))
    );
    assert!(
        failed
            .details
            .iter()
            .any(|line| { line.starts_with("current:") && line.contains("profile(none)") })
    );
    let recovered = project(&State::default());
    assert_eq!(recovered.eligible, failed.eligible);
    assert!(recovered.details.is_empty());
}

#[test]
fn pending_human_steps_are_visible_only_while_sign_in_is_needed() {
    let roster = Roster {
        rows: vec![accounts::Row {
            label: "alternate".into(),
            dir: Some(abs("/synthetic/alternate")),
            ..Default::default()
        }],
    };
    let state = State::default();
    let me = Me {
        account: Some("current"),
        left: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let point = Point::Wall {
        window: "seven_day".into(),
        at: Some(90),
    };
    let project = |logins: &Logins, enrolled: &BTreeSet<String>| {
        let mut progress = AccountProgress::unknown(100);
        enrich_inventory(
            &mut progress,
            &roster,
            &roster,
            &state,
            logins,
            enrolled,
            &me,
            Some(&point),
        );
        progress
    };
    let baseline = project(&Logins::default(), &BTreeSet::new());
    for action in [
        accounts::SignInAction::Browser,
        accounts::SignInAction::Login,
    ] {
        let mut logins = Logins::default();
        logins.labels.insert(
            "alternate".into(),
            accounts::Binding {
                dir: roster.row("alternate").unwrap().dir.clone(),
                ..Default::default()
            },
        );
        accounts::note_sign_in_task(&mut logins, "alternate", action, 90);
        assert_eq!(
            project(&logins, &BTreeSet::new()).details,
            baseline.details,
            "unadmitted task remains historical"
        );
        admitted_guidance(&roster, &mut logins, 90, "alternate", |logins| {
            accounts::note_sign_in_task(logins, "alternate", action, 90);
            Vec::new()
        });
        let progress = project(&logins, &BTreeSet::new());
        assert_eq!(
            (progress.known, progress.eligible),
            (baseline.known, baseline.eligible)
        );
        assert_eq!(progress.details[0].split(':').next(), Some("alternate"));
        let instruction = action.next_step(accounts::Cli::Claude, "alternate");
        assert!(progress.details.contains(&instruction));
        let ready = project(&logins, &BTreeSet::from(["alternate".into()]));
        assert_eq!(ready.eligible, Some(1));
        assert!(!ready.details.contains(&instruction));
        assert!(ready.details.is_empty());
    }
}

#[test]
fn inventory_keeps_the_real_session_and_does_not_invent_capacity_or_a_decision() {
    let roster = Roster {
        rows: vec![
            accounts::Row {
                label: "current".into(),
                ..Default::default()
            },
            accounts::Row {
                label: "missing".into(),
                dir: Some(abs("/synthetic/missing")),
                ..Default::default()
            },
            accounts::Row {
                label: "unlogged".into(),
                dir: Some(abs("/synthetic/login")),
                ..Default::default()
            },
        ],
    };
    let state = State::default();
    let mut logins = Logins::default();
    admitted_guidance(&roster, &mut logins, 80, "unlogged", |logins| {
        accounts::note_sign_in_attempt(
            logins,
            "unlogged",
            accounts::AttemptOutcome::NoRoute,
            "profile(none)",
            80,
        )
    });
    let mut progress = AccountProgress::unknown(100);
    progress.account = Some("current".into());
    progress.phase = ProgressPhase::Limited;
    enrich_inventory(
        &mut progress,
        &roster,
        &roster,
        &state,
        &logins,
        &BTreeSet::new(),
        &Me {
            account: Some("current"),
            left: None,
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        },
        Some(&Point::Wall {
            window: "seven_day".into(),
            at: Some(90),
        }),
    );
    assert_eq!((progress.known, progress.eligible), (Some(3), Some(0)));
    assert_eq!(progress.phase, ProgressPhase::Limited);
    assert_eq!(progress.next_at, None);
    assert!(!progress.details.is_empty());
    assert_eq!(progress.details[0].split(':').next(), Some("unlogged"));
    assert!(
        progress
            .details
            .iter()
            .all(|line| line.split(':').next() != Some("current"))
    );
    assert!(state.accounts.is_empty());

    progress = AccountProgress::unknown(100);
    enrich_inventory(
        &mut progress,
        &roster,
        &roster,
        &state,
        &logins,
        &BTreeSet::new(),
        &Me {
            account: Some("current"),
            left: None,
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        },
        None,
    );
    assert_eq!((progress.known, progress.eligible), (Some(3), None));
    assert!(
        progress
            .details
            .iter()
            .any(|line| line.starts_with("unlogged:"))
    );
}

/// A session whose turn end waits because this Mac may not keep its own account
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19: claims this Mac cannot record off its home
/// accounts, or another Mac's senior claim) says why first on the tab's account fact, ahead of
/// the other rows' reasons, and names the owner's pin where that keeps the session from
/// moving. Out of a wait the line goes last; with the claims leaving the account to this Mac,
/// nothing is said of it, nor of a preparation-only account, whose hold the execution gate
/// reports in its own fixed words.
///
/// Negative controls, in separate builds: with `own_claim_note`'s line filtered out of
/// `enrich_inventory_with_identity`, the first assertion failed with `left: Some("main")`,
/// `right: Some("pool-a")`: the walled home row's reason led. With `own_claim_note` saying any
/// other refusal of `lease_verdict` in its own words (`Err(other) => other.as_str()`, the arm
/// it had before the preparation hold reached `lease_verdict`), the last assertion failed:
/// `["pool-a: prepared: awaiting execution authority", "main: limited until
/// 2026-09-21T17:15:50Z"]`.
#[test]
fn a_wait_on_an_account_this_mac_may_not_keep_says_why_first() {
    const NOW: i64 = 1_790_000_000;
    let roster = Roster {
        rows: ["main", "pool-a"]
            .map(|label| accounts::Row {
                label: label.into(),
                dir: Some(format!("/synthetic/{label}")),
                ..Default::default()
            })
            .into(),
    };
    let enrolled = BTreeSet::from(["main".to_string(), "pool-a".to_string()]);
    let me = Me {
        account: Some("pool-a"),
        left: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let mut unclaimed = State {
        machine: "0a0b".into(),
        active: Some("pool-a".into()),
        proven: true,
        unrecorded: true,
        home_accounts: BTreeSet::from(["main".to_string()]),
        ..State::default()
    };
    let walled = accounts::note_wall(
        &mut unclaimed,
        "main",
        "five_hour",
        Some(NOW + 3 * 3600),
        NOW - 60,
        NOW - 60,
    );
    assert!(!walled.is_empty());
    let observe = |state: &State, phase: ProgressPhase, now| {
        let mut progress = AccountProgress::unknown(now);
        progress.account = Some("pool-a".into());
        progress.phase = phase;
        enrich_inventory(
            &mut progress,
            &roster,
            &roster,
            state,
            &Logins::default(),
            &enrolled,
            &me,
            Some(&Point::TurnEnd),
        );
        progress
    };
    let read = |state: &State, phase| observe(state, phase, NOW).details;
    let label = |line: &String| line.split(':').next().map(str::to_owned);
    let first = |details: &[String]| details.first().and_then(label);

    let waits = read(&unclaimed, ProgressPhase::Waiting);
    assert_eq!(first(&waits), Some("pool-a".to_string()));
    assert_eq!(
        observe(&unclaimed, ProgressPhase::Waiting, NOW).own_claim,
        Some(OwnClaimFinding {
            blocker: OwnClaimBlocker::CannotRecord,
            pinned: false,
        })
    );
    assert!(
        waits
            .iter()
            .skip(1)
            .any(|line| label(line).as_deref() == Some("main")),
        "the walled home row's reason follows: {waits:?}"
    );

    let pinned = State {
        pinned: true,
        ..unclaimed.clone()
    };
    let held = read(&pinned, ProgressPhase::Waiting);
    assert!(
        observe(&pinned, ProgressPhase::Waiting, NOW)
            .own_claim
            .unwrap()
            .pinned
    );
    assert!(
        held[0]
            .strip_prefix(waits[0].as_str())
            .is_some_and(|rest| rest.contains("pool-a")),
        "the pin is named after the cause: {held:?}"
    );

    let mut senior = unclaimed.clone();
    senior.unrecorded = false;
    senior.note_lease("pool-a", "1c1d", NOW + 3600, NOW - 60);
    let elsewhere = read(&senior, ProgressPhase::Waiting);
    let captured = observe(&senior, ProgressPhase::Waiting, NOW);
    assert_eq!(
        captured.own_claim,
        Some(OwnClaimFinding {
            blocker: OwnClaimBlocker::HeldElsewhere { until: NOW + 3600 },
            pinned: false,
        })
    );
    assert_eq!(captured.observed_at, NOW);
    assert!(
        observe(&senior, ProgressPhase::Waiting, NOW + 3600)
            .own_claim
            .is_none()
    );
    assert_eq!(first(&elsewhere), Some("pool-a".to_string()));
    assert!(elsewhere[0].contains(&super::super::usage::rfc3339_utc(NOW + 3600)));

    let ready = read(&unclaimed, ProgressPhase::Ready);
    assert_ne!(first(&ready), Some("pool-a".to_string()));
    assert_eq!(ready.last().and_then(label), Some("pool-a".to_string()));

    let mut recorded = unclaimed.clone();
    recorded.unrecorded = false;
    assert!(
        read(&recorded, ProgressPhase::Waiting)
            .iter()
            .all(|line| label(line).as_deref() != Some("pool-a")),
        "claims this Mac records leave its own account unremarked"
    );
    assert!(
        observe(&recorded, ProgressPhase::Waiting, NOW)
            .own_claim
            .is_none()
    );

    let mut prepared = unclaimed.clone();
    prepared.prepared_accounts.insert("pool-a".to_string());
    let gated = read(&prepared, ProgressPhase::Waiting);
    assert!(
        gated
            .iter()
            .all(|line| label(line).as_deref() != Some("pool-a")),
        "a preparation-only account is the execution gate's to report: {gated:?}"
    );
    assert!(
        observe(&prepared, ProgressPhase::Waiting, NOW)
            .own_claim
            .is_none()
    );

    for account in [None, Some("main")] {
        let mut changed = captured.clone();
        changed.account = account.map(str::to_owned);
        enrich_inventory(
            &mut changed,
            &roster,
            &roster,
            &senior,
            &Logins::default(),
            &enrolled,
            &me,
            Some(&Point::TurnEnd),
        );
        assert!(
            changed.own_claim.is_none(),
            "a stale binding cannot retain the own-account finding"
        );
    }
}
