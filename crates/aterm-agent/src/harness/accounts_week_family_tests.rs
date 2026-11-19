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
    let listed = candidates(
        &r,
        &s,
        &enrolled,
        Some("main"),
        ModelWeek::Unread,
        &wall_at(NOW),
        NOW,
    );
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

/// A ROW WITH A MODEL'S OWN WALL RANKS AFTER EVERY ROW WITH NONE FOR A SESSION WHOSE MODEL NOBODY
/// READ ([`Rank::key`], [`ModelWeek::Unread`]): moved at any other point (here a five-hour wall),
/// it may run Fable, Claude Code's default, which would meet Fable's wall at once, so of two rows
/// alike the one Fable's wall leaves open is taken; so does a launch that reads no model
/// ([`launch_row`]). The walled row is still a candidate: with no other, it is next. A session
/// whose model was read ranks by that model's wall alone
/// (`a_sessions_model_ranks_each_point_and_launch_by_that_models_wall_and_the_survey_names_it`).
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
        let ok: Vec<String> = candidates(
            &r,
            s,
            &enrolled,
            Some("main"),
            ModelWeek::Unread,
            &wall_at(NOW),
            NOW,
        )
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
    let listed = candidates(
        &r,
        &s,
        &enrolled,
        Some("main"),
        ModelWeek::Unread,
        &at_fable,
        NOW,
    );
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
            ModelWeek::Unread,
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

/// A SESSION'S MODEL RANKS EACH POINT AND LAUNCH BY THAT MODEL'S OWN WALL (FL-264): the model
/// its launch names, or Claude Code's default (Fable) where it names none
/// ([`ModelWeek::of_launch`], [`LaunchModel::week`]), rides on [`Me`], so at a five-hour wall,
/// an early turn end, a yield and a worker's launch a row walled on that model ranks after every
/// row that can serve it ([`Rank::model_walled`], [`ModelWeek::demotes`]). For a Fable session,
/// which goes on with Opus there, it is never excluded: with no other row it is next (owner,
/// 2026-11-11: "no, keep the same account and switch from fable to opus."). For a session of a
/// model with no fallback (Opus) it is no candidate, and with no other row the session waits
/// ([`ModelWeek::excludes`]). A session of another model reads that model's week, one of a model
/// with no week of its own reads none, and a name whose weeks this build does not know reads as
/// unread. A fresh `active` walled on the session's model is passed over where the session stays
/// on a row of its own that serves the model, or for a row with its room the commit takes
/// ([`begin_rotation_for`] with the model), and followed where the commit would take no such row
/// (a low one) and the session's own row serves it not. At a model's own wall the point names the
/// model ([`ModelWeek::at`]), and FL-206's exclusion stands. CONTROLS: a session whose model
/// nobody read ranks as FL-206 left it (any model's wall demotes a row; a Fable-walled `active`
/// is followed and holds the commit).
#[test]
fn a_sessions_model_ranks_each_point_and_launch_by_that_models_wall_and_the_survey_names_it() {
    use crate::harness::profile::Bucket;
    use crate::supervise::policy::turn_end::LaunchModel;

    // 0. The launch's model, as the host reads it off the line.
    let fable = LaunchModel::Unnamed.week();
    let opus_line = LaunchModel::of_argv(&[
        "claude".to_owned(),
        "--model".to_owned(),
        "claude-opus-5-5".to_owned(),
    ]);
    let opus = opus_line.week();
    let haiku = ModelWeek::of_launch(Some("haiku"));
    let unread = ModelWeek::Unread;
    assert_eq!(fable, ModelWeek::CLAUDE_DEFAULT);
    assert_eq!(
        fable,
        ModelWeek::FallsBack(Bucket::Fable),
        "a launch naming no model admits the fallback"
    );
    assert_eq!(opus, ModelWeek::Of(Bucket::SevenDayOpus));
    assert_eq!(haiku, ModelWeek::Bucketless);
    assert_eq!(LaunchModel::Unread.week(), unread);
    for (named, expected) in [
        ("fable", ModelWeek::FallsBack(Bucket::Fable)),
        ("claude-fable-5", ModelWeek::FallsBack(Bucket::Fable)),
        ("default", ModelWeek::Of(Bucket::Fable)),
        ("sonnet[1m]", ModelWeek::Of(Bucket::SevenDaySonnet)),
        ("claude-haiku-4-5", ModelWeek::Bucketless),
        ("opusplan", unread),
        ("claude-nova-1-0", unread),
        ("us.anthropic.claude-opus-5-5-v1:0", unread),
    ] {
        assert_eq!(ModelWeek::of_launch(Some(named)), expected, "{named}");
    }
    // What else pins the model, as Claude Code chooses it: the model a turn was served by, then
    // the `--model`, then `ANTHROPIC_MODEL` or a setting, else the default; a pin that cannot be
    // read is unread, never the default.
    {
        use crate::harness::netwatch::Source;
        use crate::supervise::policy::turn_end::ModelPins;
        let env = |vars: &[&str]| vars.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let doc = |text: &str| vec![("settings.json".to_owned(), Source::Text(text.to_owned()))];
        let unnamed = LaunchModel::Unnamed;
        let sonnet_line = LaunchModel::Named("sonnet".to_owned());
        let sonnet = ModelWeek::Of(Bucket::SevenDaySonnet);
        let pinned = |launch: &LaunchModel, env: &[String], settings: &[(String, Source)]| {
            launch.week_under(&ModelPins::of(env, settings))
        };
        let none: &[(String, Source)] = &[];
        let opus_env = env(&["ANTHROPIC_MODEL=claude-opus-5-5"]);
        assert_eq!(
            pinned(&unnamed, &opus_env, none),
            opus,
            "an environment pin"
        );
        assert_eq!(
            pinned(&unnamed, &[], &doc(r#"{"model": "opus"}"#)),
            opus,
            "a setting"
        );
        assert_eq!(
            pinned(
                &unnamed,
                &[],
                &doc(r#"{"env": {"ANTHROPIC_MODEL": "opus"}}"#)
            ),
            opus,
            "a setting's environment"
        );
        let fable_setting = doc(r#"{"model": "fable"}"#);
        assert_eq!(
            pinned(&unnamed, &opus_env, &fable_setting),
            opus,
            "the environment over a setting"
        );
        assert_eq!(
            pinned(&sonnet_line, &opus_env, &fable_setting),
            sonnet,
            "the `--model` over both"
        );
        assert_eq!(
            pinned(&unnamed, &[], &doc(r#"{"model": "claude-fable-5"}"#)),
            fable,
            "a Fable setting keeps the fallback the launch admits"
        );
        for (vars, settings, why) in [
            (
                env(&["ANTHROPIC_DEFAULT_FABLE_MODEL=claude-opus-5-5"]),
                Vec::new(),
                "a remap of the default",
            ),
            (
                opus_env.clone(),
                doc(r#"{"env": {"ANTHROPIC_MODEL": "sonnet"}}"#),
                "two pins that differ",
            ),
            (
                Vec::new(),
                vec![("settings.json".to_owned(), Source::Unreadable)],
                "a document not read",
            ),
            (Vec::new(), doc("{not json"), "a document not parsed"),
            (
                Vec::new(),
                doc(r#"{"model": 4}"#),
                "a model that is no string",
            ),
        ] {
            assert_eq!(pinned(&unnamed, &vars, &settings), unread, "{why}");
        }
        let remap_opus = env(&["ANTHROPIC_DEFAULT_OPUS_MODEL=claude-sonnet-5"]);
        assert_eq!(
            pinned(&opus_line, &remap_opus, none),
            opus,
            "no remap moves a full id"
        );
        assert_eq!(
            pinned(&LaunchModel::Named("opus".to_owned()), &remap_opus, none),
            unread,
            "a remapped alias"
        );
        assert_eq!(
            pinned(
                &unnamed,
                &[],
                &[("settings.json".to_owned(), Source::Missing)]
            ),
            fable,
            "CONTROL: nothing pins it, the default"
        );
        let served = |launch: &LaunchModel, model: &str| {
            launch.week_under(&ModelPins {
                served: Some(model.to_owned()),
                ..ModelPins::of(&env(&["ANTHROPIC_MODEL=sonnet"]), none)
            })
        };
        assert_eq!(
            served(&unnamed, "claude-opus-5-5"),
            opus,
            "a `/model` switch"
        );
        assert_eq!(
            served(&sonnet_line, "claude-fable-5"),
            ModelWeek::Of(Bucket::Fable),
            "Fable on a line that admits no fallback"
        );
        assert_eq!(
            served(&LaunchModel::Unread, "claude-opus-5-5"),
            unread,
            "CONTROL: nobody read the launch"
        );
    }

    assert_eq!(
        ModelWeek::default(),
        unread,
        "a Me nobody read the model of"
    );

    let r = roster(&[
        ("main", Class::Preferred, None),
        ("work-2", Class::Preferred, Some(at!("/logins/work-2"))),
        ("work-3", Class::Preferred, Some(at!("/logins/work-3"))),
        ("work-4", Class::Pool, Some(at!("/logins/work-4"))),
    ]);
    let enrolled = all(&r);
    // `work-2` Fable-walled, `work-3` Opus-walled, `work-4` walled on no model.
    let models_walled = |mut s: State| {
        note_wall(
            &mut s,
            "work-2",
            FABLE,
            Some(WEEK_END),
            NOW - MIN,
            NOW - MIN,
        );
        note_wall(&mut s, "work-3", OPUS, Some(WEEK_END), NOW - MIN, NOW - MIN);
        s
    };
    let five_hour_walled = |s: &mut State, label: &str| {
        note_wall(
            s,
            label,
            SESSION_LIMIT,
            Some(NOW + 2 * HOUR),
            NOW - MIN,
            NOW - MIN,
        );
    };
    // Every row but `work-2` (and the session's own) walled on the five-hour window.
    let only_work_2 = |s: &State| {
        let mut s = s.clone();
        for label in ["work-3", "work-4"] {
            five_hour_walled(&mut s, label);
        }
        s
    };
    let me_on = |model: ModelWeek| Me {
        model,
        ..me("main")
    };
    let ok = |s: &State, model: ModelWeek, point: &Point| -> Vec<String> {
        candidates(&r, s, &enrolled, Some("main"), model, point, NOW)
            .into_iter()
            .filter(|(_, verdict)| verdict.is_ok())
            .map(|(label, _)| label)
            .collect()
    };
    let ask = |s: &State, model: ModelWeek, point: &Point, mode: Mode| {
        advise(&r, s, &enrolled, &me_on(model), point, mode, NOW)
    };
    let to = |advice: &Advice| match advice {
        Advice::Move { to, .. } => Some(to.clone()),
        _ => None,
    };

    // 1. At the five-hour wall each model ranks by its own week.
    let mut at_wall = models_walled(on_main());
    note_wall(
        &mut at_wall,
        "main",
        SESSION_LIMIT,
        Some(NOW + 2 * HOUR),
        NOW,
        NOW,
    );
    let five = wall_at(NOW);
    assert_eq!(ok(&at_wall, fable, &five), ["work-3", "work-4", "work-2"]);
    assert_eq!(
        ok(&at_wall, opus, &five),
        ["work-2", "work-4"],
        "Opus has no fallback: its walled row is none"
    );
    assert!(
        candidates(&r, &at_wall, &enrolled, Some("main"), opus, &five, NOW)
            .iter()
            .any(|(label, verdict)| label == "work-3"
                && *verdict
                    == Err(WhyNot::Limited {
                        until: WEEK_END + RESET_GRACE_S
                    })),
        "until its Opus week ends"
    );
    assert_eq!(
        ok(&at_wall, haiku, &five),
        ["work-2", "work-3", "work-4"],
        "a model of no week of its own: no model's wall demotes a row"
    );
    assert_eq!(
        ok(&at_wall, unread, &five),
        ["work-4", "work-2", "work-3"],
        "CONTROL: unread, any model's wall demotes a row (FL-206)"
    );
    for (model, expected) in [(fable, "work-3"), (opus, "work-2"), (unread, "work-4")] {
        let advice = ask(&at_wall, model, &five, Mode::Wall);
        assert_eq!(
            to(&advice).as_deref(),
            Some(expected),
            "{model:?}: {advice:?}"
        );
    }
    // The only usable row walled on a Fable session's model is next, never excluded.
    let only = only_work_2(&at_wall);
    let advice = ask(&only, fable, &five, Mode::Wall);
    assert_eq!(to(&advice).as_deref(), Some("work-2"), "{advice:?}");
    let listed = candidates(&r, &only, &enrolled, Some("main"), fable, &five, NOW);
    assert!(
        listed
            .iter()
            .any(|(label, verdict)| label == "work-2" && verdict.is_ok()),
        "{listed:?}"
    );
    // An Opus session, which nothing serves on an Opus-walled row, waits for the first row to
    // open rather than move onto the only one left, and the commit takes no such move.
    let mut only_work_3 = at_wall.clone();
    for label in ["work-2", "work-4"] {
        five_hour_walled(&mut only_work_3, label);
    }
    let advice = ask(&only_work_3, opus, &five, Mode::Wall);
    assert!(
        matches!(advice, Advice::Wait { until } if until > NOW && until < WEEK_END),
        "{advice:?}"
    );
    assert_eq!(
        begin_rotation_for(
            &mut only_work_3.clone(),
            &r,
            &enrolled,
            Some("main"),
            mover("main"),
            "work-3",
            Some(SESSION_LIMIT),
            opus,
            NOW,
        )
        .map(|_| ()),
        Err(Lost::NotACandidate)
    );
    let advice = ask(&only_work_3, unread, &five, Mode::Wall);
    assert_eq!(
        to(&advice).as_deref(),
        Some("work-3"),
        "CONTROL: unread, the walled row is demoted, never excluded (FL-206): {advice:?}"
    );

    // 2. An early turn end off a walled `main`.
    let advice = ask(&at_wall, fable, &Point::TurnEnd, Mode::Early);
    assert!(
        matches!(&advice, Advice::Move { to, ahead: true, .. } if to == "work-3"),
        "{advice:?}"
    );
    let advice = ask(&only, fable, &Point::TurnEnd, Mode::Early);
    assert_eq!(to(&advice).as_deref(), Some("work-2"), "{advice:?}");

    // 3. A yield: `main` is no row this Mac may run, so the turn end moves off it.
    let held = Roster {
        rows: r
            .rows
            .iter()
            .map(|row| Row {
                held_by: (row.label == "main").then(|| "another Mac".to_owned()),
                ..row.clone()
            })
            .collect(),
    };
    let base = models_walled(on_main());
    let yielded = |s: &State| {
        advise(
            &held,
            s,
            &enrolled,
            &me_on(fable),
            &Point::TurnEnd,
            Mode::Wall,
            NOW,
        )
    };
    let advice = yielded(&base);
    assert!(
        matches!(&advice, Advice::Move { to, yields: Some(_), .. } if to == "work-3"),
        "{advice:?}"
    );
    let advice = yielded(&only_work_2(&base));
    assert!(
        matches!(&advice, Advice::Move { to, yields: Some(_), .. } if to == "work-2"),
        "{advice:?}"
    );

    // 4. A fresh `active` walled on the session's model is passed over for a row with its room,
    //    and the commit judges the same.
    let mut followed = at_wall.clone();
    followed.active = Some("work-2".to_owned());
    followed.proven = true;
    let advice = ask(&followed, fable, &five, Mode::Wall);
    assert!(
        matches!(&advice, Advice::Move { to, rotates: true, .. } if to == "work-3"),
        "{advice:?}"
    );
    let commit = |model: ModelWeek| {
        begin_rotation_for(
            &mut followed.clone(),
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "work-3",
            Some(SESSION_LIMIT),
            model,
            NOW,
        )
        .map(|_| ())
    };
    assert_eq!(commit(fable), Ok(()), "the commit rotates past it too");
    assert_eq!(
        commit(unread),
        Err(Lost::Held {
            active: "work-2".to_owned()
        }),
        "CONTROL: unread, the fresh `active` holds the move"
    );
    let advice = ask(&followed, unread, &five, Mode::Wall);
    assert!(
        matches!(&advice, Advice::Move { to, rotates: false, .. } if to == "work-2"),
        "CONTROL: unread, `active` is followed: {advice:?}"
    );
    let advice = ask(&followed, opus, &five, Mode::Wall);
    assert_eq!(
        to(&advice).as_deref(),
        Some("work-2"),
        "Fable's wall is no Opus session's: {advice:?}"
    );
    let advice = ask(&only_work_2(&followed), fable, &five, Mode::Wall);
    assert_eq!(
        to(&advice).as_deref(),
        Some("work-2"),
        "the only usable row is followed: {advice:?}"
    );
    // Rows with Fable's room that are only low rank first, but the commit takes no low row off a
    // fresh `active`, so the Fable-walled `active` is followed, never left for a wait.
    let mut low_rest = followed.clone();
    for label in ["work-3", "work-4"] {
        low(&mut low_rest, label);
    }
    assert_eq!(
        ok(&low_rest, fable, &five),
        ["work-3", "work-4", "work-2"],
        "the low rows with Fable's room are first"
    );
    assert_eq!(
        begin_rotation_for(
            &mut low_rest.clone(),
            &r,
            &enrolled,
            Some("work-2"),
            mover("main"),
            "work-3",
            Some(SESSION_LIMIT),
            fable,
            NOW,
        )
        .map(|_| ()),
        Err(Lost::Held {
            active: "work-2".to_owned()
        }),
        "the commit refuses the low row"
    );
    let advice = ask(&low_rest, fable, &five, Mode::Wall);
    assert!(
        matches!(&advice, Advice::Move { to, rotates: false, .. } if to == "work-2"),
        "a row the commit refuses is no row to pass `active` over for: {advice:?}"
    );

    // 5. A worker's launch that names no model runs Fable, and starts as its tab would move.
    let start = |s: &State, model: ModelWeek| {
        worker_row(&r, s, &enrolled, "main", Mode::Wall, model, NOW).map(|row| row.label.clone())
    };
    let default = ModelWeek::of_launch(None);
    assert_eq!(start(&at_wall, default).as_deref(), Some("work-3"));
    assert_eq!(
        start(&at_wall, unread).as_deref(),
        Some("work-4"),
        "CONTROL"
    );
    assert_eq!(
        start(&only_work_2(&at_wall), default).as_deref(),
        Some("work-2")
    );
    let mut on_work_2 = models_walled(state());
    on_work_2.active = Some("work-2".to_owned());
    on_work_2.proven = true;
    assert_eq!(
        start(&on_work_2, default),
        None,
        "a Fable worker does not follow a Fable-walled `active` off a row that runs"
    );
    assert_eq!(
        start(&on_work_2, unread).as_deref(),
        Some("work-2"),
        "CONTROL: unread, it follows `active`"
    );

    // 5a. A session whose own row serves its model stays there: a Fable-walled `active` is no
    //     row to follow off it, whether another row has Fable's room or none does, and with the
    //     rotation bound spent.
    let only_mine = only_work_2(&on_work_2);
    for (s, why) in [
        (&on_work_2, "another row with Fable's room"),
        (&only_mine, "no other row with Fable's room"),
    ] {
        let mut spent = s.clone();
        spent.rotations = vec![NOW - MIN; 2 * r.rows.len()];
        for (s, bound) in [(s.clone(), "open"), (spent, "spent")] {
            for mode in [Mode::Wall, Mode::Early] {
                let advice =
                    advise_past_probe(&r, &s, &enrolled, &me_on(fable), &Point::TurnEnd, mode, NOW);
                assert!(
                    matches!(advice, Advice::Stay(StayWhy::NothingDue)),
                    "{why}, bound {bound}, {mode:?}: {advice:?}"
                );
                assert_eq!(
                    worker_row(&r, &s, &enrolled, "main", mode, fable, NOW),
                    None,
                    "{why}, bound {bound}, {mode:?}"
                );
            }
        }
    }
    let advice = ask(&only_mine, unread, &Point::TurnEnd, Mode::Wall);
    assert!(
        matches!(&advice, Advice::Move { to, ahead: true, rotates: false, .. } if to == "work-2"),
        "CONTROL: unread, it follows `active`: {advice:?}"
    );

    // 5b. A worker the launch put on a row with Fable's room past a Fable-walled `active` is not
    //     moved back onto it at its first turn end: the tab's rule and the launch's agree.
    let mut past = followed.clone();
    five_hour_walled(&mut past, "work-4");
    assert_eq!(start(&past, default).as_deref(), Some("work-3"));
    let on_work_3 = |model: ModelWeek| Me {
        model,
        ..me("work-3")
    };
    for mode in [Mode::Wall, Mode::Early] {
        let advice = advise_past_probe(
            &r,
            &past,
            &enrolled,
            &on_work_3(fable),
            &Point::TurnEnd,
            mode,
            NOW,
        );
        assert!(
            matches!(advice, Advice::Stay(StayWhy::NothingDue)),
            "{mode:?}: {advice:?}"
        );
        assert_eq!(
            worker_row(&r, &past, &enrolled, "work-3", mode, fable, NOW),
            None,
            "{mode:?}"
        );
    }
    let advice = advise(
        &r,
        &past,
        &enrolled,
        &on_work_3(unread),
        &Point::TurnEnd,
        Mode::Wall,
        NOW,
    );
    assert!(
        matches!(&advice, Advice::Move { to, rotates: false, .. } if to == "work-2"),
        "CONTROL: unread, it follows `active` back: {advice:?}"
    );

    // 5c. An Opus session follows no `active` walled on Opus, and an Opus worker starts on no row
    //     walled on Opus: nothing serves it there.
    let mut opus_active = models_walled(state());
    opus_active.active = Some("work-3".to_owned());
    opus_active.proven = true;
    let advice = ask(&opus_active, opus, &Point::TurnEnd, Mode::Wall);
    assert!(
        matches!(advice, Advice::Stay(StayWhy::NothingDue)),
        "{advice:?}"
    );
    assert_eq!(start(&opus_active, opus), None);
    assert_eq!(
        start(&opus_active, unread).as_deref(),
        Some("work-3"),
        "CONTROL: unread, it follows `active`"
    );
    assert_eq!(
        worker_row(&r, &opus_active, &enrolled, "work-3", Mode::Wall, opus, NOW)
            .map(|row| row.label.as_str()),
        Some("main"),
        "a worker of a row walled on its model moves off it as at a wall"
    );

    // 6. At Fable's own wall the point names the model: FL-206 excludes the Fable-walled row,
    //    and Opus's wall demotes nothing there.
    let at_fable = Point::Wall {
        window: FABLE.to_owned(),
        at: Some(NOW),
    };
    let listed = candidates(
        &r,
        &at_wall,
        &enrolled,
        Some("main"),
        unread,
        &at_fable,
        NOW,
    );
    assert!(
        listed
            .iter()
            .any(|(label, verdict)| label == "work-2"
                && matches!(verdict, Err(WhyNot::Limited { .. }))),
        "{listed:?}"
    );
    assert_eq!(ok(&at_wall, unread, &at_fable), ["work-3", "work-4"]);

    // 7-8. The fleet's survey and the assistant's facts name the model's wall.
    the_survey_and_the_facts_name_a_model_wall();
}

/// Steps 7 and 8 of
/// `a_sessions_model_ranks_each_point_and_launch_by_that_models_wall_and_the_survey_names_it`, on
/// records laid on disk: a Fable wall on `work-2` crosses the survey's wire as its row's
/// `model_walls` (`crate::harness::cli::survey_accounts`, the survey's account facts,
/// [`crate::survey_facts::SwarmSurveyFacts::to_survey_reply`]) and reaches the assistant's
/// account facts ([`crate::ai::facts::ClaudeAccounts::model_walls`]) and its row's words, while
/// neither the row's condition, its `walled_until` nor its `limited_until` says a wall. An older
/// member, which sends no `model_walls=`, and an entry that does not parse read as no model wall,
/// the reply still read. CONTROL: `main`, walled on no model, names none.
fn the_survey_and_the_facts_name_a_model_wall() {
    use crate::survey_facts::SwarmSurveyFacts;

    let tmp = tmp("fl264-survey");
    let aterm_state = tmp.path().join("aterm-state");
    let harness = aterm_state
        .join("harness")
        .join(crate::harness::cli::HARNESS);
    let config = tmp.path().join("config").join("aterm.toml");
    std::fs::create_dir_all(config.parent().expect("a config dir")).expect("config dir");
    crate::test_config::write(&config, "[harness]\n").expect("aterm.toml");
    let work_2 = tmp.path().join("logins").join("work-2");
    std::fs::create_dir_all(&work_2).expect("work-2's login dir");
    std::fs::write(work_2.join("a-login-file"), "{}").expect("a login made in it");
    let mut text = String::new();
    for (label, dir) in [("main", None), ("work-2", work_2.to_str())] {
        text = append_row(&text, label, Class::Preferred, dir).expect("the row admits");
    }
    let roster_path = roster_beside_for(&config, Cli::Claude).expect("the roster's path");
    std::fs::write(&roster_path, text).expect("accounts.toml");
    let store = Store::at(&harness);
    store
        .transact(NOW, &mut None, |state| {
            // `work-2` serves this Mac, so the assistant's facts read its row.
            state.active = Some("work-2".to_owned());
            state.proven = true;
            Ok(note_wall(
                state,
                "work-2",
                FABLE,
                Some(WEEK_END),
                NOW - MIN,
                NOW - MIN,
            ))
        })
        .expect("the records");
    let fable_wall = BTreeMap::from([(FABLE.to_owned(), WEEK_END + RESET_GRACE_S)]);

    // 7. The survey's row, its account fact and the wire.
    let rows = crate::harness::cli::survey_accounts(Cli::Claude, (&config, &harness, None), NOW)
        .expect("the pool reads")
        .rows;
    let row = |label: &str| {
        rows.iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("{label}: {rows:?}"))
    };
    assert_eq!(row("work-2").model_walls, fable_wall);
    assert_eq!(row("main").model_walls, BTreeMap::new(), "CONTROL");
    assert!(
        !matches!(row("work-2").condition, Condition::Limited { .. }),
        "a model's wall is no wall of the account: {:?}",
        row("work-2")
    );
    assert_eq!(row("work-2").condition, row("main").condition);

    let facts =
        SwarmSurveyFacts::collect_at(&aterm_state, None, Some(&config), 0, false, None, NOW);
    let fact = |label: &str| {
        facts
            .accounts
            .iter()
            .find(|fact| fact.pool == "claude" && fact.id == label)
            .unwrap_or_else(|| panic!("{label}: {:?}", facts.accounts))
    };
    assert_eq!(fact("work-2").model_walls, fable_wall);
    assert_eq!(fact("work-2").walled_until, None);
    assert_eq!(fact("main").model_walls, BTreeMap::new(), "CONTROL");
    let wire = facts.to_survey_reply(
        4242,
        Some("m2.local"),
        u64::try_from(NOW).expect("a time") * 1000,
        1,
        (aterm_types::fleet_access::FleetAccess::Full, false),
    );
    let fable_token = format!(" model_walls={FABLE}:{} ", WEEK_END + RESET_GRACE_S);
    let account_lines = |wire: &str| {
        wire.lines()
            .filter(|line| line.starts_with("account pool=claude "))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let lines = account_lines(&wire);
    assert!(
        lines
            .iter()
            .any(|line| line.contains(" id=work-2 ") && line.contains(&fable_token)),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains(" id=main ") && line.contains(" model_walls=- ")),
        "{lines:?}"
    );
    let read = |wire: &str| {
        SwarmSurveyFacts::from_survey_reply(wire)
            .expect("the reply reads")
            .accounts
    };
    assert_eq!(read(&wire), facts.accounts, "the wire carries it");

    // An older member sends no `model_walls=`: its rows read as walled on no model, every other
    // field as it was.
    let with_model_walls = |token: &str| {
        wire.lines()
            .map(|line| {
                line.split(' ')
                    .filter_map(|word| {
                        if !word.starts_with("model_walls=") {
                            Some(word.to_owned())
                        } else if token.is_empty() {
                            None
                        } else {
                            Some(format!("model_walls={token}"))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let unwalled: Vec<_> = facts
        .accounts
        .iter()
        .cloned()
        .map(|fact| crate::survey_facts::SurveyAccountFact {
            model_walls: BTreeMap::new(),
            ..fact
        })
        .collect();
    assert_eq!(read(&with_model_walls("")), unwalled, "an older member");
    assert_eq!(read(&with_model_walls("junk")), unwalled, "a bad entry");
    let one_bad = format!("{OPUS}:soon,{FABLE}:{}", WEEK_END + RESET_GRACE_S);
    let kept = read(&with_model_walls(&one_bad));
    assert!(
        kept.iter()
            .filter(|fact| fact.pool == "claude")
            .all(|fact| fact.model_walls == fable_wall),
        "a bad entry is dropped, the rest kept: {kept:?}"
    );

    // 8. The assistant's account facts of the row serving this Mac, and its row's words.
    let b = crate::ai::testing::machine(&["claude"]);
    let home = b.home.display().to_string();
    let var = |name: &str| (name == "HOME").then(|| home.clone());
    let m = crate::ai::facts::Machine {
        now: NOW,
        home: Some(b.home.clone()),
        var: &var,
        pkg: Some(b.pkg.clone()),
        exclude: Vec::new(),
        records: crate::ai::store::Records::default(),
        harness_state: Some(harness.clone()),
        roster: Some(roster_path.clone()),
        accounts_mode: None,
        config: None,
    };
    let f = crate::ai::facts::gather(&m, &crate::ai::connector::CLAUDE);
    let view = f.accounts.as_ref().expect("the accounts view");
    assert_eq!(view.label, "work-2", "{view:?}");
    assert_eq!(view.model_walls, fable_wall);
    assert_eq!(view.limited_until, None, "CONTROL: no wall of the account");
    let words = crate::ai::state::assess(&f, true, NOW);
    assert!(
        words.fact.contains(&format!(
            "Fable week walled until {}",
            crate::ai::state::when(WEEK_END + RESET_GRACE_S)
        )),
        "{words:?}"
    );
    assert!(words.state.usable(), "{words:?}");
}

/// The journal event `note` is, as the `aterm` executable's sink makes it
/// (`crates/aterm/src/alab_journal.rs`'s `event`, which `aterm`'s own tests hold to every word a
/// note carries): the one step of a note's way to the fleet this crate cannot call.
#[cfg(unix)]
fn journal_event(note: &crate::harness::journal::Note) -> aterm_db::alab::journal::Event {
    use aterm_db::alab::journal::{Event, Kind, ModelBucket, Pk, Res, Source, Window, Word};
    Event {
        t: note.t,
        kind: Kind::parse(note.kind).expect("a journal kind"),
        account: note
            .account
            .map(|key| Pk::new(key.as_str()).expect("a key")),
        cli: Word::new(note.cli).expect("a CLI word"),
        sub: note
            .sub
            .map(|sub| Word::new(sub.as_str()).expect("a workspace")),
        window: note.window.map(|w| Window::parse(w).expect("a window")),
        model: note
            .model
            .map(|m| ModelBucket::parse(m).expect("a model's bucket")),
        pct_x10: note.pct_x10,
        reset: note.reset,
        res: note.res.map(|r| Res::parse(r).expect("a resolution")),
        until: note.until,
        plan: note.plan.map(|p| Word::new(p).expect("a plan word")),
        src: Source::parse(note.src).expect("a source"),
        why: note.why.map(|w| Word::new(w).expect("a reason")),
        to: note.to.map(|key| Pk::new(key.as_str()).expect("a key")),
        tab: note.tab.map(|tab| Word::new(tab.as_str()).expect("a tab")),
        harness: note.harness.map(|h| Word::new(h).expect("a harness word")),
        op: None,
        via: None,
        program: None,
    }
}

/// THE FLEET'S MODEL WALLS (FL-269): a peer's wall of Fable's own week reaches this Mac with its
/// model, by the way every Mac's walls take: the peer's store notes it ([`note_wall`]), its
/// journal says it with Fable's bucket ([`crate::harness::journal::observe`]), its spool and
/// flusher book it into the account's `ai/usage` row under `model_walls` and not in `wall`,
/// `ai/limits` folds it so beside the account's own wall, and rotation reads it into
/// [`Fleet::model_walls`] (`aterm_db::alab::journal::fold_limits_for_test`, [`Fleet::of`]).
/// There it demotes its account for a Fable session (Claude Code's default, with its fallback,
/// and one with none) and for a session of any model while that is unread, and for no Opus and
/// no Haiku one ([`peer_wall`], [`Rank::peer_walled`]), as this Mac's own reading of it would
/// ([`ModelWeek::demotes`]); a peer's wall of the account's own (its five hours) demotes it for
/// both, and the limits row's `wall` names that one while `model_walls` still holds Fable's; an
/// older build's rows, whose `wall` says `7d-model` and names no model (its `ai/usage` row, and
/// the `ai/limits` row its materializer folded of it), wall no account and no model. A peer's
/// wall demotes and never excludes (SF-09). The peer's clearing of Opus's wall clears no other
/// model's; a later peer's earlier end of Fable's week folds under the latest, whichever row
/// comes last, and its Sonnet wall stands apart, so a session whose model is unread stays
/// demoted by Fable's wall once Sonnet's lapsed.
/// NEGATIVE CONTROLS (2026-11-17): with [`peer_wall`] judging every session as one whose model
/// is unread (its `week` ignored, so any model's wall demotes the account, as before FL-269),
/// the Opus session's order puts `b` last. With the book's `wall-cleared` clearing every
/// `7d-model` wall of its window, the peer's usage row loses Fable's; with `limits::fold` keeping
/// the last reading's end of a bucket, b's Fable wall ends a day early; with [`peer_wall`] taking
/// the earliest model wall for an unread model, b is not demoted once Sonnet's lapsed; with
/// `Rotation::limits` taking every `wall` but `none` as the account's, the older build's
/// `ai/limits` row walls c.
#[cfg(unix)]
#[test]
fn a_peers_model_wall_demotes_its_account_for_that_models_sessions_and_no_other() {
    use aterm_db::alab::journal::{Spool, fold_limits_for_test};
    use aterm_json::json;

    const PEER: &str = "0000000000000000000000000000beef";
    // A second peer, whose rows sort after the first's.
    const LATER: &str = "0000000000000000000000000000f00d";
    const OLDER: &str = "00000000000000000000000000000ced";
    let sonnet = crate::harness::profile::Bucket::SevenDaySonnet.id();
    let key = |label: &str| format!("ac-{}", label.repeat(16));
    let ms = |t: i64| u64::try_from(t).expect("a time") * 1000;

    // THE PEERS: two Macs, each with its own spool.
    let named = |name: &str| State {
        machine: name.to_owned(),
        ..State::default()
    };
    let (mut peer, mut later) = (named("peer-mac"), named("later-mac"));
    let (spool_dir, later_dir) = (tmp("fl269-peer"), tmp("fl269-later"));
    let spool = Spool::at(spool_dir.path().join("alab"));
    let later_spool = Spool::at(later_dir.path().join("alab"));
    // What `mac`'s journal says of what `change` did to its store, each note spooled.
    let journal = |mac: &mut State, spool: &Spool, change: &dyn Fn(&mut State) -> Vec<Event>| {
        let before = mac.clone();
        let mut events = change(mac);
        mac.accounts.get_mut("b").expect("noted").pk = Some(key("b"));
        let notes = crate::harness::journal::observe(Cli::Claude, &before, mac, &mut events, NOW);
        for note in &notes {
            spool.append(&journal_event(note)).expect("spooled");
        }
        notes
    };
    let wall_on = |mac: &mut State, spool: &Spool, window: &str, until: i64| {
        let notes = journal(mac, spool, &|s: &mut State| {
            note_wall(s, "b", window, Some(until), NOW - MIN, NOW - MIN)
        });
        let walls: Vec<_> = notes.iter().filter(|note| note.kind == "wall").collect();
        assert_eq!(walls.len(), 1, "one wall note: {notes:?}");
        *walls[0]
    };
    // The peer walls b on Fable's week, and its journal says so with Fable's bucket.
    let fable_note = wall_on(&mut peer, &spool, FABLE, WEEK_END);
    assert_eq!(
        (fable_note.window, fable_note.model, fable_note.until),
        (
            Some("7d-model"),
            Some(FABLE),
            Some(u64::try_from(WEEK_END).unwrap())
        ),
        "the wall names its model"
    );
    // It walls b on Opus's week too, then reads Opus's week with room: that clearing names Opus
    // and clears Opus's wall alone.
    assert_eq!(wall_on(&mut peer, &spool, OPUS, WEEK_END).model, Some(OPUS));
    let cleared = journal(&mut peer, &spool, &|s: &mut State| {
        note_model_room(s, "b", OPUS, NOW - 30, NOW)
    });
    let cleared: Vec<_> = cleared
        .iter()
        .filter(|note| note.kind.starts_with("wall"))
        .map(|note| (note.kind, note.model))
        .collect();
    assert_eq!(cleared, [("wall-cleared", Some(OPUS))]);
    // The later peer walls b on Fable's week a day sooner, and on Sonnet's: the fold keeps
    // Fable's latest end, whichever Mac's row comes last, and Sonnet's apart.
    wall_on(&mut later, &later_spool, FABLE, WEEK_END - DAY);
    wall_on(&mut later, &later_spool, sonnet, WEEK_END - DAY);

    // AN OLDER BUILD's row of c: `wall` says `7d-model` and names no model.
    let older_row = json!({
        "r": 1, "n": 1, "account": key("c"), "wall": "7d-model", "wall_until": ms(WEEK_END),
        "src": "cli-output", "read_at": ms(NOW - MIN), "at": ms(NOW), "sessions_7d": 0,
    });
    let older_key = format!("{}.{OLDER}", key("c"));
    // And the `ai/limits` row of c that build's materializer folded of it, as rotation reads the
    // file where that build wrote it last: its `wall` still says `7d-model`.
    let c_key = key("c");
    let older_limits = json!({
        "account": key("c"), "wall": "7d-model", "wall_until": ms(WEEK_END), "readers": 1,
        "read_at": ms(NOW - MIN),
    });
    let fold = || {
        fold_limits_for_test(
            &[(PEER, &spool), (LATER, &later_spool)],
            &[(older_key.as_str(), older_row.clone())],
            &[(c_key.as_str(), older_limits.clone())],
            u64::try_from(NOW).unwrap(),
        )
        .expect("the peer's records fold")
    };
    let fable_entry = json!([{"bucket": FABLE, "until": ms(WEEK_END)}]);
    let b_models = json!([
        {"bucket": FABLE, "until": ms(WEEK_END)},
        {"bucket": sonnet, "until": ms(WEEK_END - DAY)},
    ]);
    let folded = fold();
    let usage = &folded.usage[&format!("{}.{PEER}", key("b"))];
    assert_eq!(
        (usage["wall"].as_str(), &usage["model_walls"]),
        (Some("none"), &fable_entry),
        "the peer's usage row: no wall of the account's, Fable's under its bucket: {usage:?}"
    );
    let limits_b = &folded.limits[&key("b")];
    assert_eq!(
        (limits_b["wall"].as_str(), &limits_b["model_walls"]),
        (Some("none"), &b_models),
        "the limits row's wall stays the account's: {limits_b:?}"
    );
    let limits_c = &folded.limits[&key("c")];
    assert_eq!(
        (
            limits_c["wall"].as_str(),
            limits_c.get("wall_until"),
            limits_c.get("model_walls")
        ),
        (Some("none"), None, None),
        "an older row's bare `7d-model` walls no account and no model: {limits_c:?}"
    );

    // THIS MAC: a, b and c, b's week ending first; the fleet as rotation reads it.
    let r = roster(&[
        ("a", Class::Preferred, Some("/logins/a")),
        ("b", Class::Preferred, Some("/logins/b")),
        ("c", Class::Preferred, Some("/logins/c")),
    ]);
    let enrolled = all(&r);
    let mut here = state();
    for (label, days) in [("b", 1), ("a", 2), ("c", 3)] {
        let week = Sample {
            week_reset: Some(NOW + days * DAY),
            ..sample(10, 20)
        };
        assert_eq!(note_sample(&mut here, label, week, NOW).len(), 1);
        here.accounts.get_mut(label).expect("noted").pk = Some(key(label));
    }
    let fleet = Fleet::of(&folded.rotation);
    assert_eq!(
        (fleet.walls.get(&key("b")), fleet.walls.get(&key("c"))),
        (None, None),
        "no peer wall of either account's own, nor of the older build's `ai/limits` row: {fleet:?}"
    );
    assert_eq!(
        fleet.model_walls.get(&key("b")),
        Some(
            &[
                (FABLE.to_owned(), WEEK_END),
                (sonnet.to_owned(), WEEK_END - DAY)
            ]
            .into_iter()
            .collect()
        ),
        "{fleet:?}"
    );
    assert_eq!(fleet.model_walls.get(&key("c")), None, "{fleet:?}");
    let fable = ModelWeek::CLAUDE_DEFAULT;
    let fable_only = ModelWeek::Of(crate::harness::profile::Bucket::Fable);
    let opus = ModelWeek::of_model("opus");
    let haiku = ModelWeek::of_model("haiku");
    let ranked = |s: &State, week: ModelWeek| {
        let verdicts = candidates(&r, s, &enrolled, None, week, &wall(), NOW);
        let order: Vec<String> = verdicts
            .iter()
            .filter(|(_, verdict)| verdict.is_ok())
            .map(|(label, _)| label.clone())
            .collect();
        let demoted: Vec<String> = verdicts
            .iter()
            .filter(|(_, verdict)| verdict.as_ref().is_ok_and(|rank| rank.peer_walled))
            .map(|(label, _)| label.clone())
            .collect();
        (order, demoted)
    };
    let s = State {
        fleet: fleet.clone(),
        ..here.clone()
    };
    for week in [fable, fable_only, ModelWeek::Unread] {
        assert_eq!(
            ranked(&s, week),
            (vec!["a".into(), "c".into(), "b".into()], vec!["b".into()]),
            "a {week:?} session takes b after every row the peer's Fable wall leaves, and still \
             takes it"
        );
    }
    for week in [opus, haiku] {
        assert_eq!(
            ranked(&s, week),
            (vec!["b".into(), "a".into(), "c".into()], vec![]),
            "a {week:?} session takes b first: Fable's wall leaves it this model's room"
        );
    }
    assert_eq!(peer_wall(&s, fable, "b", NOW), Some(WEEK_END));
    assert_eq!(peer_wall(&s, opus, "b", NOW), None);
    assert_eq!(peer_wall(&s, ModelWeek::Unread, "c", NOW), None);
    assert_eq!(
        peer_wall(&s, ModelWeek::Unread, "b", WEEK_END - DAY),
        Some(WEEK_END),
        "Sonnet's wall lapsed, Fable's still demotes b for a session whose model is unread"
    );

    // AN ACCOUNT-WIDE WALL on the peer too: b's five hours. The limits row's `wall` names it and
    // `model_walls` still holds Fable's; every session takes b after the rest.
    let session_end = NOW + 2 * HOUR;
    let five_note = wall_on(&mut peer, &spool, SESSION_LIMIT, session_end);
    assert_eq!((five_note.window, five_note.model), (Some("5h"), None));
    let folded = fold();
    let limits_b = &folded.limits[&key("b")];
    assert_eq!(
        (
            limits_b["wall"].as_str(),
            limits_b["wall_until"].as_u64(),
            &limits_b["model_walls"]
        ),
        (Some("5h"), Some(ms(session_end)), &b_models),
        "the account's wall in `wall`, Fable's in `model_walls`: {limits_b:?}"
    );
    let s = State {
        fleet: Fleet::of(&folded.rotation),
        ..here.clone()
    };
    for week in [fable, opus, haiku, ModelWeek::Unread] {
        assert_eq!(
            ranked(&s, week),
            (vec!["a".into(), "c".into(), "b".into()], vec!["b".into()]),
            "a {week:?} session takes b after the rest: the account's own wall walls every model"
        );
    }
    assert_eq!(peer_wall(&s, opus, "b", NOW), Some(session_end));
    assert_eq!(
        peer_wall(&s, fable, "b", NOW),
        Some(WEEK_END),
        "the later of the two"
    );
    // Once both lapse, nothing demotes b.
    assert_eq!(peer_wall(&s, fable, "b", WEEK_END), None);
}

/// A WEEK NO MODEL'S BUCKET NAMES (FL-269): a wall of `seven_day_other` (Claude Code's weekly
/// row of a model this build names no bucket of) walls the whole account on this Mac
/// ([`Account::limited_until`] skips only the models' own weeks), so the fleet reads a peer's
/// as every session's too. The peer's journal names it `seven_day_other`, its usage row states
/// it under that word in `model_walls` (no account-wide `wall`, which has no word for it), and
/// rotation reads it into [`Fleet::model_walls`], where [`peer_wall`] takes it for a session of
/// every model, Fable's, Opus's and Haiku's included, and for one whose model is unread. It
/// demotes and never excludes (SF-09).
/// NEGATIVE CONTROLS, run: with [`peer_wall`] judging a bucket outside [`MODEL_BUCKETS`] as a
/// read model's own (no `model_week(bucket).is_none()` arm), a Fable session (Claude Code's
/// default) takes b first: `` a peer's wall of a week no model names walls every session:
/// FallsBack(Fable) ``; with the journal naming only the models' own weeks
/// (`journal::model_word` giving `model_week(name)`), the note names no model:
/// `` left: [(Some("7d-model"), None)] ``.
#[cfg(unix)]
#[test]
fn a_peers_wall_of_a_week_no_models_bucket_names_demotes_its_account_for_every_session() {
    use aterm_db::alab::journal::{Spool, fold_limits_for_test};
    use aterm_json::json;
    const PEER: &str = "0000000000000000000000000000beef";
    let other = crate::harness::profile::Bucket::Other.id();
    let key = |label: &str| format!("ac-{}", label.repeat(16));
    let ms = |t: i64| u64::try_from(t).expect("a time") * 1000;

    // The peer walls b on `seven_day_other`, and its journal names that word.
    let mut peer = State {
        machine: "peer-mac".to_owned(),
        ..State::default()
    };
    let spool_dir = tmp("fl269-other");
    let spool = Spool::at(spool_dir.path().join("alab"));
    let before = peer.clone();
    let mut events = note_wall(&mut peer, "b", other, Some(WEEK_END), NOW - MIN, NOW - MIN);
    assert!(
        peer.accounts["b"].limited_until(NOW).is_some(),
        "this Mac's own reading of such a wall walls the account"
    );
    peer.accounts.get_mut("b").expect("noted").pk = Some(key("b"));
    let notes = crate::harness::journal::observe(Cli::Claude, &before, &mut peer, &mut events, NOW);
    let walls: Vec<_> = notes
        .iter()
        .filter(|note| note.kind == "wall")
        .map(|note| (note.window, note.model))
        .collect();
    assert_eq!(walls, [(Some("7d-model"), Some(other))], "{notes:?}");
    for note in &notes {
        spool.append(&journal_event(note)).expect("spooled");
    }
    let folded = fold_limits_for_test(&[(PEER, &spool)], &[], &[], u64::try_from(NOW).unwrap())
        .expect("the peer's records fold");
    let entry = json!([{"bucket": other, "until": ms(WEEK_END)}]);
    let usage = &folded.usage[&format!("{}.{PEER}", key("b"))];
    assert_eq!(
        (usage["wall"].as_str(), &usage["model_walls"]),
        (Some("none"), &entry),
        "{usage:?}"
    );
    assert_eq!(folded.limits[&key("b")]["model_walls"], entry);

    // THIS MAC: a, b and c, b's week ending first.
    let r = roster(&[
        ("a", Class::Preferred, Some("/logins/a")),
        ("b", Class::Preferred, Some("/logins/b")),
        ("c", Class::Preferred, Some("/logins/c")),
    ]);
    let enrolled = all(&r);
    let mut s = state();
    for (label, days) in [("b", 1), ("a", 2), ("c", 3)] {
        let week = Sample {
            week_reset: Some(NOW + days * DAY),
            ..sample(10, 20)
        };
        assert_eq!(note_sample(&mut s, label, week, NOW).len(), 1);
        s.accounts.get_mut(label).expect("noted").pk = Some(key(label));
    }
    s.fleet = Fleet::of(&folded.rotation);
    assert_eq!(
        s.fleet.model_walls.get(&key("b")),
        Some(&[(other.to_owned(), WEEK_END)].into_iter().collect()),
        "{:?}",
        s.fleet
    );
    for week in [
        ModelWeek::CLAUDE_DEFAULT,
        ModelWeek::Of(crate::harness::profile::Bucket::Fable),
        ModelWeek::of_model("opus"),
        ModelWeek::of_model("haiku"),
        ModelWeek::Unread,
    ] {
        let verdicts = candidates(&r, &s, &enrolled, None, week, &wall(), NOW);
        let order: Vec<&str> = verdicts
            .iter()
            .filter(|(_, verdict)| verdict.is_ok())
            .map(|(label, _)| label.as_str())
            .collect();
        assert_eq!(
            order,
            ["a", "c", "b"],
            "a peer's wall of a week no model names walls every session: {week:?}"
        );
        assert_eq!(peer_wall(&s, week, "b", NOW), Some(WEEK_END), "{week:?}");
    }
}
