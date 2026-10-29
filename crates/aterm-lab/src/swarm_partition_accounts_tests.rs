// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn failed(ev: &Evidence) -> Vec<&'static str> {
    judge(ev)
        .into_iter()
        .filter(|c| !c.pass)
        .map(|c| c.name)
        .collect()
}

fn users(rows: &[(i64, &[&str])]) -> Vec<(i64, Vec<String>)> {
    rows.iter()
        .map(|(t, u)| (*t, u.iter().map(|m| (*m).to_owned()).collect()))
        .collect()
}

/// What F19 promises: b2 keeps its account through the cut, one user after the heal, every use
/// under the user's own claim, and a2 never takes b2's home account.
fn whole() -> Evidence {
    Evidence {
        scenarios: vec![
            Scenario {
                name: "lease",
                users: users(&[(0, &["b2"])]),
                unclaimed: Vec::new(),
                b2_stopped: None,
                a2_started: None,
                b2_end: "ok".to_owned(),
                holder_end: Some("m4".to_owned()),
            },
            Scenario {
                name: "home",
                users: users(&[(0, &["b2"])]),
                unclaimed: Vec::new(),
                b2_stopped: None,
                a2_started: None,
                b2_end: "ok".to_owned(),
                holder_end: Some("m4".to_owned()),
            },
        ],
    }
}

#[test]
fn what_f19_promises_passes_every_check() {
    assert_eq!(judge(&whole()).len(), 4);
    assert_eq!(failed(&whole()), Vec::<&str>::new());
}

#[test]
fn the_claims_leave_one_user_after_the_heal_and_home_use_blocks_a_cold_claim() {
    // The model on main's functions, run for real.
    let ev = run();
    let lease = &ev.scenarios[0];
    let home = &ev.scenarios[1];
    let heal = CUT_AT + CUT_FOR;
    // a2 last heard b2 the minute before the cut. Once that is over a lease's presence life old,
    // b2's lease is no claim a2 sees, so a2 claims it while cut off: each side settles what it
    // hears.
    let lapsed = CUT_AT - TAP_S + PRESENCE_TTL_S + TAP_S;
    assert_eq!(lease.a2_started, Some(lapsed), "{:?}", lease.users);
    // b2's own claim stays live to b2 through the cut. At the heal a2 hears b2 again, and its
    // next tap reads b2's senior claim: a2 yields at that turn end and b2 keeps the account.
    assert_eq!(lease.b2_stopped, None, "{:?}", lease.users);
    let (yielded, last) = lease.users.last().cloned().unwrap_or_default();
    assert_eq!(last, vec!["b2".to_owned()]);
    assert!(
        (heal..=heal + RECHECK_S).contains(&yielded),
        "{:?}",
        lease.users
    );
    assert_eq!(lease.holder_end.as_deref(), Some(B2));
    // b2's home claim lapses on no silence: a2 reads it held elsewhere throughout.
    assert_eq!(home.users, users(&[(0, &["b2"])]));
    assert_eq!(home.a2_started, None);
    assert_eq!(home.b2_end, "ok");
    assert_eq!(home.holder_end.as_deref(), Some(B2));
    assert_eq!(lease.unclaimed, Vec::new());
    assert_eq!(home.unclaimed, Vec::new());
    assert_eq!(failed(&ev), Vec::<&str>::new());
}

#[test]
fn two_users_after_the_heal_or_nobody_is_not_one_user() {
    let mut ev = whole();
    ev.scenarios[0]
        .users
        .push((3000, vec!["b2".to_owned(), "a2".to_owned()]));
    assert_eq!(
        failed(&ev),
        vec!["accounts: a split longer than the lease leaves one user per account"]
    );
    let mut ev = whole();
    ev.scenarios[0]
        .users
        .push((2400, vec!["b2".to_owned(), "a2".to_owned()]));
    ev.scenarios[0].users.push((2520, vec!["b2".to_owned()]));
    assert_eq!(
        failed(&ev),
        vec!["accounts: a split longer than the lease leaves one user per account"]
    );
    let mut ev = whole();
    ev.scenarios[0].users.push((3000, Vec::new()));
    assert_eq!(
        failed(&ev),
        vec!["accounts: a split longer than the lease leaves one user per account"]
    );
}

#[test]
fn a_use_with_no_claim_of_its_own_fails_claims_before_use() {
    let mut ev = whole();
    ev.scenarios[1]
        .unclaimed
        .push((A2_WANTS_AT, "a2".to_owned()));
    assert_eq!(
        failed(&ev),
        vec!["accounts: a Mac claims an account before it uses it"]
    );
}

#[test]
fn a_scenario_that_did_not_run_fails_its_checks() {
    let ev = Evidence::default();
    assert_eq!(failed(&ev).len(), 4);
}

/// The model's turn end is production's advice (`accounts::advise` at `Point::TurnEnd`), not a
/// rule of its own: under `accounts = "off"` a2 keeps the account it claimed while cut off, beside
/// b2, as production leaves it (the owner turned rotation off), and the plane says two users;
/// with a free row enrolled a2 moves there; under `accounts = "wall"` with no row free it holds.
///
/// NEGATIVE CONTROL: make `Client::turn` drop the session wherever `State::lease_verdict` errs (the
/// model's old rule), and the rotation-off run reports one user after the heal.
#[test]
fn the_turn_end_follows_production_advice_in_every_mode() {
    let heal = CUT_AT + CUT_FOR;
    let last = |s: &Scenario| {
        s.users
            .last()
            .map(|(t, u)| (*t, u.clone()))
            .unwrap_or_default()
    };

    let off = scenario_with(
        "lease",
        "shared",
        false,
        &A2Setup {
            mode: Mode::Off,
            enrolled: Vec::new(),
        },
    );
    let (_, users) = last(&off);
    assert_eq!(
        users,
        vec!["b2".to_owned(), "a2".to_owned()],
        "{:?}",
        off.users
    );
    let ev = Evidence {
        scenarios: vec![off, run().scenarios.remove(1)],
    };
    assert_eq!(
        failed(&ev),
        vec!["accounts: a split longer than the lease leaves one user per account"]
    );

    for setup in [
        A2Setup {
            mode: Mode::Early,
            enrolled: vec!["shared", "home-b"],
        },
        A2Setup {
            mode: Mode::Wall,
            enrolled: Vec::new(),
        },
    ] {
        let s = scenario_with("lease", "shared", false, &setup);
        let (left, users) = last(&s);
        assert_eq!(users, vec!["b2".to_owned()], "{setup:?}: {:?}", s.users);
        assert!(
            (heal..=heal + RECHECK_S).contains(&left),
            "{setup:?}: {:?}",
            s.users
        );
    }
}
