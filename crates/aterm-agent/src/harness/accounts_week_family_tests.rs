// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WEEK'S WALLS AS ONE FAMILY (FL-206): a refill of the week, by a tap reading or a weekly
//! reset, refills Fable's bucket with it (`aterm_phase::reset_limit_names` names both the weekly
//! limit), so it lifts each of their walls last seen before its proof, one row each, by the one
//! rule both paths share ([`lift_refilled`]), and tells the peers each window of the family; a
//! read's room lifts each window by its own figure alone; and a model's own weekly wall walls
//! that model's sessions, never the account (owner, 2026-11-11: "no, keep the same account and
//! switch from fable to opus.").

use super::*;

/// The week's reset the walls and figures here name.
const WEEK_END: i64 = NOW + 3 * DAY;

/// Opus's own weekly bucket.
const OPUS: &str = "seven_day_opus";

/// A tap reading of the week alone, `pct_x10` used, served at `at`, by a process that read it
/// before (`refilled`: the figure it saw the week fall from).
fn week(pct_x10: u32, refilled: Option<u32>, at: i64) -> TapRead {
    TapRead {
        seven_day: win(pct_x10, WEEK_END, refilled),
        at,
        seen_before: true,
        ..TapRead::default()
    }
}

/// "main" with the week read full at `NOW - MIN`, then walled at `NOW` on the week and on
/// Fable's bucket.
fn week_and_fable_walled() -> State {
    let mut s = state();
    note_reading(&mut s, "main", &week(1000, None, NOW - MIN), NOW - MIN);
    for window in [WEEK_LIMIT, FABLE] {
        assert_eq!(
            note_wall(&mut s, "main", window, Some(WEEK_END), NOW, NOW).len(),
            1,
            "{window}"
        );
    }
    let main = &s.accounts["main"];
    assert!(
        main.limited_until(NOW).is_some(),
        "the week walls the account"
    );
    assert!(main.model_limited_until(FABLE, NOW).is_some());
    s
}

/// The windows the rows in `events` lift, each row's own, in the rows' order: the rows that lift a
/// wall and wake its waiters ([`WALL_LIFTED`], [`Event::wakes`]).
fn lifts(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter(|e| e.kind == Kind::Wall && e.wakes())
        .filter_map(|e| Some(e.why.split_once(WALL_LIFTED)?.0))
        .collect()
}

/// The windows whose walls stand unlifted on `label`.
fn standing<'a>(s: &'a State, label: &str) -> Vec<&'a str> {
    s.accounts[label]
        .walls
        .iter()
        .filter(|(_, wall)| !wall.cleared)
        .map(|(window, _)| window.as_str())
        .collect()
}

#[test]
fn a_weekly_refill_lifts_the_fable_wall_written_before_it() {
    let mut s = week_and_fable_walled();
    let refill = NOW + MIN;
    let events = note_reading(&mut s, "main", &week(50, Some(1000), refill), refill);
    assert_eq!(
        lifts(&events),
        [WEEK_LIMIT, FABLE],
        "one row a wall: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.why.starts_with(FABLE) && e.why.contains(WEEK_LIMIT)),
        "Fable's row names the week's refill as its proof: {events:?}"
    );
    let main = &s.accounts["main"];
    assert_eq!(main.windows[WEEK_LIMIT].epoch_at, Some(refill));
    assert_eq!(main.limited_until(refill), None);
    assert_eq!(main.model_limited_until(FABLE, refill), None);
    assert!(main.model_walls(refill).next().is_none());
    assert_eq!(
        main.lifted_at(FABLE, &s.machine),
        Some(refill),
        "a session moved onto Opus at Fable's limit reads the refill as Fable's lift"
    );
    assert_eq!(
        main.newest_lifts(&s.machine)
            .iter()
            .map(|lift| lift.window.as_str())
            .collect::<Vec<_>>(),
        [WEEK_LIMIT, FABLE],
        "each lift is told to the peers by its own window"
    );

    // CONTROL: the five-hour window's refill refills no week.
    let mut s = week_and_fable_walled();
    let events = note_reading(
        &mut s,
        "main",
        &five(win(1000, NOW + 2 * HOUR, None), NOW - MIN, true),
        NOW - MIN,
    );
    assert!(lifts(&events).is_empty());
    let events = note_reading(
        &mut s,
        "main",
        &five(win(50, NOW + 2 * HOUR, Some(1000)), refill, true),
        refill,
    );
    assert!(lifts(&events).is_empty(), "{events:?}");
    assert!(
        s.accounts["main"]
            .model_limited_until(FABLE, refill)
            .is_some()
    );
}

/// A WEEK'S REFILL IS TOLD TO THE PEERS FOR FABLE'S BUCKET TOO, THOUGH NO FABLE WALL STOOD HERE:
/// the refill lifts this Mac's week wall and leaves Fable's lift beside it ([`tell_refilled`]), so
/// a peer that holds Fable's wall of the same reset hears it by the record it already applies
/// ([`note_peer_lift`], keyed by window), and a peer's Fable wall seen after the refill stands.
/// NEGATIVE CONTROL: a refill that lifts nothing here tells nothing.
#[test]
fn a_weekly_refill_tells_the_peers_fables_lift_though_no_fable_wall_stood_here() {
    let mut s = state();
    note_reading(&mut s, "main", &week(1000, None, NOW - MIN), NOW - MIN);
    note_wall(&mut s, "main", WEEK_LIMIT, Some(WEEK_END), NOW, NOW);
    let refill = NOW + MIN;
    let events = note_reading(&mut s, "main", &week(50, Some(1000), refill), refill);
    assert_eq!(
        lifts(&events),
        [WEEK_LIMIT],
        "one row: the wall lifted here"
    );
    let told = s.accounts["main"].newest_lifts(&s.machine);
    assert_eq!(
        told,
        [
            Lift {
                window: WEEK_LIMIT.to_owned(),
                at: refill,
                reset: Some(WEEK_END),
            },
            Lift {
                window: FABLE.to_owned(),
                at: refill,
                reset: Some(WEEK_END),
            },
        ]
    );
    assert_eq!(s.accounts["main"].model_limited_until(FABLE, refill), None);

    // A peer holding Fable's wall alone, written before the refill, lifts it by that lift.
    let peer = |seen_again: Option<i64>| {
        let mut other = State {
            machine: "0c0d".to_owned(),
            ..State::default()
        };
        note_wall(
            &mut other,
            "work",
            FABLE,
            Some(WEEK_END),
            NOW - HOUR,
            NOW - HOUR,
        );
        if let Some(at) = seen_again {
            note_wall(&mut other, "work", FABLE, Some(WEEK_END), at, at);
        }
        other.accounts.get_mut("work").expect("walled").pk = Some("ac-00000000000000a1".into());
        let lift = &told[1];
        let events = note_peer_lift(
            &mut other,
            &PeerLift {
                peer: "n-0a0b",
                pk: "ac-00000000000000a1",
                window: &lift.window,
                at: lift.at,
                reset: lift.reset,
            },
            refill + MIN,
        );
        (
            lifts(&events).len(),
            other.accounts["work"].model_limited_until(FABLE, refill + MIN),
        )
    };
    assert_eq!(peer(None), (1, None));
    assert!(
        peer(Some(refill + 5)).1.is_some(),
        "seen after the refill: it stands"
    );

    // NEGATIVE CONTROL.
    let mut s = state();
    note_reading(&mut s, "main", &week(1000, None, NOW - MIN), NOW - MIN);
    note_reading(&mut s, "main", &week(50, Some(1000), refill), refill);
    assert!(s.accounts["main"].newest_lifts(&s.machine).is_empty());
}

#[test]
fn a_fable_wall_met_after_the_refill_stands() {
    let mut s = week_and_fable_walled();
    let refill = NOW + MIN;
    note_reading(&mut s, "main", &week(50, Some(1000), refill), refill);
    let met = refill + 10;
    assert_eq!(
        note_wall(&mut s, "main", FABLE, Some(WEEK_END), met, met).len(),
        1,
        "a wall met after the lift's mark is recorded"
    );
    // The next reading still holds the refill's barrier: it lifts nothing met after it.
    let later = met + MIN;
    let events = note_reading(&mut s, "main", &week(60, None, later), later);
    assert!(lifts(&events).is_empty(), "{events:?}");
    let main = &s.accounts["main"];
    assert_eq!(
        main.model_limited_until(FABLE, later),
        Some(WEEK_END + RESET_GRACE_S)
    );
    assert_eq!(
        main.limited_until(later),
        None,
        "Fable's wall walls no account"
    );

    // A wall met before the refill and seen standing again after it stands: the refill did not
    // refill it. Its first sighting is older than the refill; its newest is not.
    let mut s = week_and_fable_walled();
    let served = NOW + MIN;
    assert_eq!(
        note_wall(
            &mut s,
            "main",
            FABLE,
            Some(WEEK_END),
            served + 5,
            served + 5
        ),
        [],
        "seen again: no new wall"
    );
    let events = note_reading(&mut s, "main", &week(50, Some(1000), served), served + 20);
    assert_eq!(lifts(&events), [WEEK_LIMIT], "{events:?}");
    assert_eq!(
        s.accounts["main"].model_limited_until(FABLE, served + 20),
        Some(WEEK_END + RESET_GRACE_S)
    );
    let events = note_reading(&mut s, "main", &week(60, None, served + MIN), served + MIN);
    assert!(
        lifts(&events).is_empty(),
        "nor does a later reading: {events:?}"
    );

    // A wall met after the refill's figures were served, though before they were recorded,
    // stands too: the refill is the proof, and the wall is newer than it.
    let mut s = state();
    note_reading(&mut s, "main", &week(1000, None, NOW - MIN), NOW - MIN);
    let served = NOW + MIN;
    note_wall(
        &mut s,
        "main",
        FABLE,
        Some(WEEK_END),
        served + 5,
        served + 5,
    );
    let events = note_reading(&mut s, "main", &week(50, Some(1000), served), served + 20);
    assert!(lifts(&events).is_empty(), "{events:?}");
    assert_eq!(
        s.accounts["main"].windows[WEEK_LIMIT].epoch_at,
        Some(served)
    );
    assert!(
        s.accounts["main"]
            .model_limited_until(FABLE, served + 20)
            .is_some()
    );
}

#[test]
fn a_model_wall_leaves_the_account_a_rotation_candidate() {
    let r = roster(&[
        ("main", Class::Preferred, None),
        ("work-2", Class::Preferred, Some(at!("/logins/work-2"))),
    ]);
    let enrolled = all(&r);
    let mut s = on_main();
    s.pinned = true;
    note_wall(
        &mut s,
        "work-2",
        FABLE,
        Some(WEEK_END),
        NOW - MIN,
        NOW - MIN,
    );
    let work = &s.accounts["work-2"];
    assert_eq!(work.limited_until(NOW), None);
    assert_eq!(
        work.model_limited_until(FABLE, NOW),
        Some(WEEK_END + RESET_GRACE_S),
        "a Fable session reads Fable's reset"
    );
    assert_eq!(work.model_limited_until("seven_day_opus", NOW), None);
    assert_eq!(
        work.model_limited_until(WEEK_LIMIT, NOW),
        None,
        "no model's"
    );
    assert_eq!(
        model_walls_clause(Some(work), NOW),
        format!(
            " \u{b7} Fable week walled until {}",
            crate::harness::usage::rfc3339_utc(WEEK_END + RESET_GRACE_S)
        )
    );
    let row = r.row("work-2").expect("listed");
    assert_eq!(condition(row, &s, true, NOW), Condition::Usable);

    // Fable's wall on the pinned `active` row ends no pin: the row serves every other model.
    note_wall(&mut s, "main", FABLE, Some(WEEK_END), NOW, NOW);
    assert!(s.pinned, "the owner's pin stands");
    assert_eq!(s.active.as_deref(), Some("main"));
    let main = &s.accounts["main"];
    assert_eq!(
        main.walled_at(),
        None,
        "a model's wall is no wall of the account"
    );

    // At the five-hour wall of `main`, `work-2` is next.
    note_wall(
        &mut s,
        "main",
        SESSION_LIMIT,
        Some(NOW + 2 * HOUR),
        NOW,
        NOW,
    );
    assert!(!s.pinned, "an account's own wall ends the pin");
    let listed = candidates(&r, &s, &enrolled, Some("main"), &wall_at(NOW), NOW);
    assert!(
        listed
            .iter()
            .any(|(label, rank)| label == "work-2" && rank.is_ok()),
        "{listed:?}"
    );
    let advice = advise(
        &r,
        &s,
        &enrolled,
        &me("main"),
        &wall_at(NOW),
        Mode::Wall,
        NOW,
    );
    assert!(
        matches!(&advice, Advice::Move { to, .. } if to == "work-2"),
        "{advice:?}"
    );
    assert_eq!(
        launch_row(&r, &s, &enrolled, NOW).map(|row| row.label.as_str()),
        Some("work-2"),
        "`next:` names it"
    );

    // CONTROL: the week's wall on `work-2` takes it out of rotation.
    note_wall(&mut s, "work-2", WEEK_LIMIT, Some(WEEK_END), NOW, NOW);
    let advice = advise(
        &r,
        &s,
        &enrolled,
        &me("main"),
        &wall_at(NOW),
        Mode::Wall,
        NOW,
    );
    assert!(!matches!(advice, Advice::Move { .. }), "{advice:?}");
    assert_ne!(
        launch_row(&r, &s, &enrolled, NOW).map(|row| row.label.as_str()),
        Some("work-2")
    );
}

/// A ROW WITH A MODEL'S OWN WALL RANKS AFTER EVERY ROW WITH NONE ([`Rank::key`]): a session moved
/// at any other point (here a five-hour wall) runs a model the ranking does not know, and Fable,
/// Claude Code's default, would meet Fable's wall at once, so of two rows alike the one Fable's
/// wall leaves open is taken. The walled row is still a candidate: with no other, it is next.
/// NEGATIVE CONTROL: with no model wall, the label orders them and `work-2` is first.
#[test]
fn a_row_with_a_model_wall_ranks_after_every_row_with_none() {
    let r = roster(&[
        ("main", Class::Preferred, None),
        ("work-2", Class::Preferred, Some(at!("/logins/work-2"))),
        ("work-3", Class::Preferred, Some(at!("/logins/work-3"))),
    ]);
    let enrolled = all(&r);
    let mut s = state();
    note_wall(
        &mut s,
        "main",
        SESSION_LIMIT,
        Some(NOW + 2 * HOUR),
        NOW,
        NOW,
    );
    let next = |s: &State| {
        let ok: Vec<String> = candidates(&r, s, &enrolled, Some("main"), &wall_at(NOW), NOW)
            .into_iter()
            .filter(|(_, rank)| rank.is_ok())
            .map(|(label, _)| label)
            .collect();
        let advice = advise(
            &r,
            s,
            &enrolled,
            &me("main"),
            &wall_at(NOW),
            Mode::Wall,
            NOW,
        );
        (ok, advice)
    };
    let (ok, advice) = next(&s);
    assert_eq!(ok, ["work-2", "work-3"], "NEGATIVE CONTROL");
    assert!(
        matches!(&advice, Advice::Move { to, .. } if to == "work-2"),
        "{advice:?}"
    );
    note_wall(
        &mut s,
        "work-2",
        FABLE,
        Some(WEEK_END),
        NOW - MIN,
        NOW - MIN,
    );
    let (ok, advice) = next(&s);
    assert_eq!(ok, ["work-3", "work-2"]);
    assert!(
        matches!(&advice, Advice::Move { to, .. } if to == "work-3"),
        "{advice:?}"
    );
    assert_eq!(
        launch_row(&r, &s, &enrolled, NOW).map(|row| row.label.as_str()),
        Some("work-3")
    );
}

/// A session at Fable's own wall moves onto no row where Fable's wall stands too and follows no
/// such `active`, but rotates past it to a row Fable's wall leaves open, as the commit judges the
/// same move ([`begin_rotation_for`] with the window it leaves); with no row open to Fable, it
/// waits until the first Fable wall ends: a model's wall leaves its account a candidate for every
/// other session, never for one at that model's own wall. An Opus session at Opus's wall, which
/// has no fallback, moves past an Opus-walled `active` alike. (At Fable's wall the turn end goes
/// on with Opus on the same account before it takes this move: `turn_end`'s `account_first`.)
/// NEGATIVE CONTROL: at the five-hour wall, the Fable-walled `active` is followed.
#[test]
fn a_session_at_a_model_wall_moves_onto_no_row_walled_on_that_model() {
    let r = roster(&[
        ("main", Class::Preferred, None),
        ("work-2", Class::Preferred, Some(at!("/logins/work-2"))),
        ("work-3", Class::Preferred, Some(at!("/logins/work-3"))),
    ]);
    let enrolled = all(&r);
    let at_fable = Point::Wall {
        window: FABLE.to_owned(),
        at: Some(NOW),
    };
    let first_end = WEEK_END - DAY + RESET_GRACE_S;
    let mut s = on_main();
    for (label, end) in [("main", WEEK_END), ("work-2", WEEK_END - DAY)] {
        note_wall(&mut s, label, FABLE, Some(end), NOW - MIN, NOW - MIN);
    }
    let advice = advise(&r, &s, &enrolled, &me("main"), &at_fable, Mode::Wall, NOW);
    assert!(
        matches!(&advice, Advice::Move { to, .. } if to == "work-3"),
        "{advice:?}"
    );
    let listed = candidates(&r, &s, &enrolled, Some("main"), &at_fable, NOW);
    assert!(
        listed
            .iter()
            .any(|(label, why)| label == "work-2"
                && *why == Err(WhyNot::Limited { until: first_end })),
        "{listed:?}"
    );

    s.active = Some("work-2".to_owned());
    let advice = advise(&r, &s, &enrolled, &me("main"), &at_fable, Mode::Wall, NOW);
    assert!(
        matches!(&advice, Advice::Move { to, rotates: true, .. } if to == "work-3"),
        "the Fable-walled `active` is not followed, and the move rotates past it: {advice:?}"
    );
    let commit = |s: &State, window: &str| {
        begin_rotation_for(
            &mut s.clone(),
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "work-3",
            Some(window),
            NOW,
        )
        .map(|_| ())
    };
    assert_eq!(commit(&s, FABLE), Ok(()), "the commit judges it so");
    assert_eq!(
        commit(&s, SESSION_LIMIT),
        Err(Lost::Held {
            active: "work-2".to_owned()
        }),
        "off another window, the fresh `active` holds the move"
    );
    // Opus's wall alike, with an Opus-walled `active`.
    let mut opus = on_main();
    opus.active = Some("work-2".to_owned());
    for label in ["main", "work-2"] {
        note_wall(&mut opus, label, OPUS, Some(WEEK_END), NOW - MIN, NOW - MIN);
    }
    let at_opus = Point::Wall {
        window: OPUS.to_owned(),
        at: Some(NOW),
    };
    let advice = advise(&r, &opus, &enrolled, &me("main"), &at_opus, Mode::Wall, NOW);
    assert!(
        matches!(&advice, Advice::Move { to, .. } if to == "work-3"),
        "{advice:?}"
    );
    assert_eq!(commit(&opus, OPUS), Ok(()));
    // CONTROL: at the five-hour wall, the Fable-walled `active` is followed.
    let advice = advise(
        &r,
        &s,
        &enrolled,
        &me("main"),
        &wall_at(NOW),
        Mode::Wall,
        NOW,
    );
    assert!(
        matches!(&advice, Advice::Move { to, .. } if to == "work-2"),
        "{advice:?}"
    );

    s.active = Some("main".to_owned());
    note_wall(
        &mut s,
        "work-3",
        FABLE,
        Some(WEEK_END),
        NOW - MIN,
        NOW - MIN,
    );
    let advice = advise(&r, &s, &enrolled, &me("main"), &at_fable, Mode::Wall, NOW);
    assert_eq!(
        advice,
        Advice::Wait { until: first_end },
        "the first Fable wall to end"
    );
}

/// A WEEKLY RESET USED LIFTS BOTH WEEKLY BUCKETS AND AN OPUS WALL STANDS: a weekly or a full
/// reset refills the week's family, so it lifts each of their walls last seen before its outcome,
/// the barrier it writes, by the rule a tap's refill lifts by, so the next reading of the same
/// reset lifts nothing it kept. A Fable reset and a session reset refill their claim's window
/// alone. NEGATIVE CONTROL: a wall met at the outcome or after it stands, through the next
/// reading too.
#[test]
fn a_weekly_reset_used_lifts_both_weekly_buckets_and_an_opus_wall_stands() {
    use alab_policy::vocab::ResetKind;
    let written = NOW - HOUR;
    let claim = |s: &mut State, window: &str, until: i64| {
        assert_eq!(
            claim_reset(s, "main", (window, until), "s-one", 41, START, NOW - MIN),
            ResetClaimed::Taken
        );
    };
    let walled = |window: &str| {
        let mut s = on_main();
        for wall in [WEEK_LIMIT, FABLE, OPUS, SESSION_LIMIT] {
            let end = if wall == SESSION_LIMIT {
                NOW + 2 * HOUR
            } else {
                WEEK_END
            };
            note_wall(&mut s, "main", wall, Some(end), written, written);
        }
        let until = if window == SESSION_LIMIT {
            NOW + 2 * HOUR
        } else {
            WEEK_END
        };
        claim(&mut s, window, until);
        (s, until)
    };
    let used = |kind| ResetDone::Used {
        kind,
        left: Some(0),
    };

    let (mut s, until) = walled(WEEK_LIMIT);
    let rows = reset_outcome(
        &mut s,
        "main",
        until,
        "s-one",
        &used(ResetKind::Weekly),
        NOW,
    );
    assert_eq!(rows[0].kind, Kind::ResetUsed);
    assert_eq!(
        rows[0].refilled,
        [WEEK_LIMIT],
        "the journal's refill is the claim's"
    );
    assert_eq!(
        lifts(&rows[1..]),
        [FABLE],
        "the claim's own wall is told by the reset's row: {rows:?}"
    );
    assert_eq!(rows[1].sid.as_deref(), Some("s-one"));
    assert_eq!(
        standing(&s, "main"),
        [SESSION_LIMIT, OPUS],
        "Opus's wall stands"
    );
    let main = &s.accounts["main"];
    assert_eq!(main.model_limited_until(FABLE, NOW), None);
    assert_eq!(
        main.model_limited_until(OPUS, NOW),
        Some(WEEK_END + RESET_GRACE_S)
    );
    assert_eq!(
        main.lifted_at(FABLE, &s.machine),
        Some(NOW),
        "proved by the outcome"
    );
    assert_eq!(
        main.weeks_said.get(FABLE).map(|said| said.pct),
        Some(None),
        "Fable's week reads refilled"
    );

    // The claim on Fable's own wall: a weekly reset refills the week with it.
    let (mut s, until) = walled(FABLE);
    let rows = reset_outcome(
        &mut s,
        "main",
        until,
        "s-one",
        &used(ResetKind::Weekly),
        NOW,
    );
    assert_eq!(lifts(&rows[1..]), [WEEK_LIMIT]);
    assert_eq!(standing(&s, "main"), [SESSION_LIMIT, OPUS]);

    // A full reset refills the five-hour window and the week's family; Opus's wall stands.
    let (mut s, until) = walled(SESSION_LIMIT);
    let rows = reset_outcome(&mut s, "main", until, "s-one", &used(ResetKind::Full), NOW);
    assert_eq!(lifts(&rows[1..]), [WEEK_LIMIT, FABLE]);
    assert_eq!(standing(&s, "main"), [OPUS]);
    assert_eq!(s.accounts["main"].limited_until(NOW), None);

    // CONTROL: a Fable reset gives back only what Fable used: its own wall alone; and a session
    // reset only the five-hour window, whatever its claim names.
    let (mut s, until) = walled(FABLE);
    let rows = reset_outcome(&mut s, "main", until, "s-one", &used(ResetKind::Fable), NOW);
    assert!(lifts(&rows[1..]).is_empty(), "{rows:?}");
    assert_eq!(standing(&s, "main"), [SESSION_LIMIT, WEEK_LIMIT, OPUS]);
    let (mut s, until) = walled(WEEK_LIMIT);
    let rows = reset_outcome(
        &mut s,
        "main",
        until,
        "s-one",
        &used(ResetKind::Session),
        NOW,
    );
    assert!(lifts(&rows[1..]).is_empty(), "{rows:?}");
    assert_eq!(standing(&s, "main"), [SESSION_LIMIT, OPUS, FABLE]);

    // The reset's lift and the next reading's agree: with the week's figure held, the outcome's
    // barrier is the refill a reading of the same reset reads, and the reset lifted already
    // every wall that barrier lifts.
    let held = || {
        let mut s = on_main();
        note_reading(
            &mut s,
            "main",
            &week(1000, None, written - MIN),
            written - MIN,
        );
        note_wall(&mut s, "main", WEEK_LIMIT, Some(WEEK_END), written, written);
        claim(&mut s, WEEK_LIMIT, WEEK_END);
        s
    };
    let mut s = held();
    note_wall(
        &mut s,
        "main",
        FABLE,
        Some(WEEK_END),
        NOW - MIN + 1,
        NOW - MIN + 1,
    );
    let rows = reset_outcome(
        &mut s,
        "main",
        WEEK_END,
        "s-one",
        &used(ResetKind::Weekly),
        NOW,
    );
    assert_eq!(
        lifts(&rows[1..]),
        [FABLE],
        "met after the claim, before the outcome"
    );
    assert_eq!(s.accounts["main"].windows[WEEK_LIMIT].epoch_at, Some(NOW));
    let events = note_reading(&mut s, "main", &week(50, None, NOW + MIN), NOW + MIN);
    assert!(lifts(&events).is_empty(), "{events:?}");

    // NEGATIVE CONTROL: a wall met at the outcome stands, and so does one met after it, through
    // the next reading of the same reset.
    let mut s = held();
    note_wall(&mut s, "main", FABLE, Some(WEEK_END), NOW, NOW);
    let rows = reset_outcome(
        &mut s,
        "main",
        WEEK_END,
        "s-one",
        &used(ResetKind::Weekly),
        NOW,
    );
    assert!(lifts(&rows[1..]).is_empty(), "{rows:?}");
    note_wall(&mut s, "main", FABLE, Some(WEEK_END), NOW + 5, NOW + 5);
    let events = note_reading(&mut s, "main", &week(50, None, NOW + MIN), NOW + MIN);
    assert!(lifts(&events).is_empty(), "{events:?}");
    assert_eq!(standing(&s, "main"), [FABLE]);
}

/// A model's own wall is lifted by that bucket's own figure below full alone: room in the week of
/// all models or the five-hour window, or in another model's bucket, lifts none of it
/// ([`note_room`], [`note_model_room`]; a session's own read reports a model's bucket with room
/// only below full). A wall written after the read, or seen standing again after it, stands.
/// NEGATIVE CONTROL: the week's room lifts the week's wall, and Fable's stands beside it.
#[test]
fn note_room_lifts_a_model_wall_only_on_that_buckets_own_figure() {
    let mut s = state();
    note_wall(&mut s, "main", FABLE, Some(WEEK_END), NOW, NOW);
    note_wall(&mut s, "main", OPUS, Some(WEEK_END), NOW, NOW);
    let read = Sample {
        session_pct: Some(10),
        week_pct: Some(40),
        at: NOW + MIN,
        ..Sample::default()
    };
    // The session's and the week's room are not Fable's, nor is Opus's own.
    assert!(lifts(&note_room(&mut s, "main", &read, NOW + 2 * MIN)).is_empty());
    let mut opus = s.clone();
    assert_eq!(
        lifts(&note_model_room(
            &mut opus,
            "main",
            OPUS,
            NOW + MIN,
            NOW + 2 * MIN
        )),
        [OPUS]
    );
    assert!(
        opus.accounts["main"]
            .model_limited_until(FABLE, NOW + 2 * MIN)
            .is_some(),
        "Opus's room is not Fable's"
    );
    // A wall written after the read stands, and so does one seen standing again after it.
    let mut after = s.clone();
    assert!(
        lifts(&note_model_room(
            &mut after,
            "main",
            FABLE,
            NOW - 1,
            NOW + 2 * MIN
        ))
        .is_empty()
    );
    let mut again = s.clone();
    note_wall(
        &mut again,
        "main",
        FABLE,
        Some(WEEK_END),
        NOW + MIN + 5,
        NOW + MIN + 5,
    );
    assert!(
        lifts(&note_model_room(
            &mut again,
            "main",
            FABLE,
            NOW + MIN,
            NOW + 2 * MIN
        ))
        .is_empty()
    );
    // Fable's own figure below full lifts Fable's wall, and no other's.
    assert_eq!(
        lifts(&note_model_room(
            &mut s,
            "main",
            FABLE,
            NOW + MIN,
            NOW + 2 * MIN
        )),
        [FABLE]
    );
    let main = &s.accounts["main"];
    assert_eq!(main.model_limited_until(FABLE, NOW + 2 * MIN), None);
    assert_eq!(
        main.walls[FABLE].seen,
        Some(NOW + MIN),
        "the read is its proof"
    );
    assert!(main.model_limited_until(OPUS, NOW + 2 * MIN).is_some());

    // NEGATIVE CONTROL.
    let mut s = state();
    for window in [WEEK_LIMIT, FABLE] {
        note_wall(&mut s, "main", window, Some(WEEK_END), NOW, NOW);
    }
    let events = note_room(&mut s, "main", &read, NOW + 2 * MIN);
    assert_eq!(lifts(&events), [WEEK_LIMIT]);
    assert!(
        s.accounts["main"]
            .model_limited_until(FABLE, NOW + 2 * MIN)
            .is_some()
    );
}
