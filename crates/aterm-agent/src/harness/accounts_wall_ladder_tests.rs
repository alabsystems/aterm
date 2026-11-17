// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WALL LADDER (FL-118b): an account whose sessions all wait at its usage wall is read once a
//! rung, 1, 2, 4 and 8 minutes after the wall and every 15 minutes after that, by the session that
//! has waited longest, and by no other.

use super::*;
use crate::harness::provider_read::{self, InUse, ProbeEvent};

/// When the stub's account has room again: past rung 4 (23 min), before rung 5 (38 min).
const LIFT_AT: i64 = NOW + 30 * MIN;

/// A session's turn on the wall at `now`: noted waiting, then, if it holds a due rung, its read
/// made, the panel showing the stub's figure for the session's window (full before
/// [`LIFT_AT`], room after) and lifting what that shows. Returns the rung read and by whom.
fn waiter_turn(
    s: &mut State,
    account: &str,
    sid: &str,
    now: i64,
    reads: &mut Vec<(String, String, u32, i64)>,
) -> Vec<ProbeEvent> {
    note_wall_wait(s, account, sid, now);
    let (_, events) = provider_read::captured(|| {
        let Ok(claim) = claim_wall_read(s, account, sid, now) else {
            return;
        };
        reads.push((account.to_string(), sid.to_string(), claim.rung, now));
        let read = claim.read_use().begin(panel_account(account), now, None);
        assert!(read.is_ok(), "the rung's use reads its own account");
        let pct = if now >= LIFT_AT { 40 } else { 100 };
        let sample = Sample {
            session_pct: Some(pct),
            at: now,
            ..Sample::default()
        };
        note_room(s, account, &sample, now);
    });
    events
}

fn resumes(s: &State, label: &str, written: i64, now: i64) -> bool {
    let r = three();
    let advice = advise_past_probe(
        &r,
        s,
        &all(&r),
        &me(label),
        &wall_at(written),
        Mode::Wall,
        now,
    );
    matches!(advice, Advice::Resume { .. })
}

#[test]
fn one_holder_reads_the_panel_once_per_rung_for_its_account() {
    let mut s = on_main();
    note_wall(&mut s, "main", SESSION_LIMIT, Some(NOW + DAY), NOW, NOW);
    let waiters = ["s-first", "s-second", "s-third"];
    for (k, sid) in waiters.iter().enumerate() {
        assert!(note_wall_wait(&mut s, "main", sid, NOW + 5 * k as i64));
    }
    let mut reads = Vec::new();
    let mut probes = Vec::new();
    let mut waiting: Vec<&str> = waiters.to_vec();
    let mut t = NOW;
    while t <= NOW + 2 * HOUR {
        // Every waiter takes its turn every 10 s, the holder last: no order of theirs reads twice.
        for sid in waiting.iter().rev() {
            probes.extend(waiter_turn(&mut s, "main", sid, t, &mut reads));
        }
        // A waiter whose advice is a resume leaves the wall.
        waiting.retain(|sid| {
            let gone = resumes(&s, "main", NOW, t);
            if gone {
                wall_wait_over(&mut s, "main", sid);
            }
            !gone
        });
        t += 10;
    }
    let rungs: Vec<(u32, i64)> = reads.iter().map(|(_, _, rung, at)| (*rung, *at)).collect();
    assert_eq!(
        rungs,
        [
            (0, NOW + MIN),
            (1, NOW + 2 * MIN),
            (2, NOW + 4 * MIN),
            (3, NOW + 8 * MIN),
            (4, NOW + 23 * MIN),
            (5, NOW + 38 * MIN),
        ],
        "one read a rung, each at its rung, until a read showed room"
    );
    assert!(
        reads.iter().all(|(_, sid, ..)| sid == "s-first"),
        "the one that waited longest reads, and no other: {reads:?}"
    );
    assert_eq!(
        probes,
        rungs
            .iter()
            .map(|(_, at)| ProbeEvent {
                at: *at,
                read: provider_read::Read::ClaudePanel,
                account: Some("main".into()),
                login: None,
                why: InUse::AtWall,
            })
            .collect::<Vec<_>>(),
        "each rung is one provider read, its reason at-wall"
    );
    assert!(waiting.is_empty(), "the lift resumed every waiter");
    assert!(s.wall_reads.is_empty(), "nothing waits: the ladder stopped");
    let lift = &s.accounts["main"].walls[SESSION_LIMIT];
    assert!(lift.cleared && lift.at == NOW + 38 * MIN, "{lift:?}");

    // CONTROL: a second session of the account, its rung due, is refused, and reads nothing.
    let mut s = on_main();
    note_wall(&mut s, "main", SESSION_LIMIT, Some(NOW + DAY), NOW, NOW);
    note_wall_wait(&mut s, "main", "s-first", NOW);
    note_wall_wait(&mut s, "main", "s-second", NOW + 5);
    let measured = Some("2.1.325");
    let due = |s: &State, sid: &str, now| wall_read_due(s, "main", sid, measured, measured, now);
    assert!(due(&s, "s-first", NOW + MIN));
    assert!(!due(&s, "s-second", NOW + MIN));
    let (refused, none) =
        provider_read::captured(|| claim_wall_read(&mut s, "main", "s-second", NOW + MIN).err());
    assert_eq!(refused, Some(WallNo::NotHolder));
    assert!(none.is_empty());
    assert!(claim_wall_read(&mut s, "main", "s-first", NOW + MIN).is_ok());
    assert_eq!(
        claim_wall_read(&mut s, "main", "s-first", NOW + MIN + 30).err(),
        Some(WallNo::NotDue { at: NOW + 2 * MIN }),
        "a rung is read once, whatever its read showed"
    );

    // CONTROL: on a build the panel was not measured on, nothing is due.
    assert!(!wall_read_due(
        &s,
        "main",
        "s-first",
        None,
        measured,
        NOW + 2 * MIN
    ));
    assert!(!wall_read_due(
        &s,
        "main",
        "s-first",
        measured,
        Some("2.1.324"),
        NOW + 2 * MIN
    ));
    assert!(wall_read_due(
        &s,
        "main",
        "s-first",
        measured,
        measured,
        NOW + 2 * MIN
    ));

    // Two walled accounts read [`PROBE_APART_S`] apart, never together, each once a rung at most.
    let mut s = on_main();
    for label in ["main", "work-2"] {
        note_wall(&mut s, label, SESSION_LIMIT, Some(NOW + DAY), NOW, NOW);
        note_wall_wait(&mut s, label, &format!("s-{label}"), NOW);
    }
    let mut reads = Vec::new();
    let mut t = NOW;
    while t <= NOW + 20 * MIN {
        for label in ["main", "work-2"] {
            waiter_turn(&mut s, label, &format!("s-{label}"), t, &mut reads);
        }
        t += 10;
    }
    for pair in reads.windows(2).filter(|pair| pair[0].0 != pair[1].0) {
        assert!(pair[1].3 - pair[0].3 >= PROBE_APART_S, "{reads:?}");
    }
    for label in ["main", "work-2"] {
        let rungs: Vec<u32> = reads
            .iter()
            .filter(|(of, ..)| of == label)
            .map(|(_, _, rung, _)| *rung)
            .collect();
        assert!(rungs.windows(2).all(|w| w[0] < w[1]), "{label}: {rungs:?}");
        assert!(!rungs.is_empty(), "{label}: {reads:?}");
    }
}

#[test]
fn the_wall_ladder_stops_when_nothing_waits() {
    let mut s = on_main();
    note_wall(&mut s, "main", SESSION_LIMIT, Some(NOW + DAY), NOW, NOW);
    note_wall_wait(&mut s, "main", "s-first", NOW);
    note_wall_wait(&mut s, "main", "s-second", NOW + 5);
    assert!(wall_wait_over(&mut s, "main", "s-first"));
    assert_eq!(
        s.wall_reads["main"].holder(),
        Some("s-second"),
        "the next that waited longest holds it"
    );
    assert!(wall_wait_over(&mut s, "main", "s-second"));
    assert!(s.wall_reads.is_empty(), "nothing waits: no ladder");
    let mut reads = Vec::new();
    let (_, probes) = provider_read::captured(|| {
        for t in (NOW..NOW + 2 * HOUR).step_by(10) {
            for sid in ["s-first", "s-second"] {
                if let Ok(claim) = claim_wall_read(&mut s, "main", sid, t) {
                    reads.push(claim.rung);
                }
            }
        }
    });
    assert!(reads.is_empty() && probes.is_empty(), "{reads:?}");

    // A session that ends leaves every ladder it waited on.
    note_wall_wait(&mut s, "main", "s-first", NOW);
    assert!(forget_session(&mut s, "s-first"));
    assert!(s.wall_reads.is_empty());

    // A waiter not seen at its wall for long is gone at the next one's note.
    note_wall_wait(&mut s, "main", "s-gone", NOW);
    note_wall_wait(&mut s, "main", "s-here", NOW + WALL_WAITER_KEEP_S);
    assert_eq!(s.wall_reads["main"].holder(), Some("s-here"));
    assert_eq!(s.wall_reads["main"].since, NOW + WALL_WAITER_KEEP_S);

    // A person typing in a waiting tab starts the ladder again at its first rung.
    let mut s = on_main();
    note_wall(&mut s, "main", SESSION_LIMIT, Some(NOW + DAY), NOW, NOW);
    note_wall_wait(&mut s, "main", "s-first", NOW);
    for rung in 0..3 {
        let at = wall_rung_at(NOW, rung);
        assert_eq!(
            claim_wall_read(&mut s, "main", "s-first", at)
                .map(|c| c.rung)
                .ok(),
            Some(rung)
        );
    }
    let typed = NOW + 5 * MIN;
    assert!(note_wall_person(&mut s, "main", "s-first", typed));
    assert_eq!(
        claim_wall_read(&mut s, "main", "s-first", typed + 30).err(),
        Some(WallNo::NotDue { at: typed + MIN })
    );
    assert_eq!(
        claim_wall_read(&mut s, "main", "s-first", typed + MIN)
            .map(|c| c.rung)
            .ok(),
        Some(0),
        "its first rung, a minute after the person typed"
    );
    assert!(
        !note_wall_person(&mut s, "main", "s-other", typed),
        "a tab that does not wait restarts nothing"
    );

    // The ladder is machine-local state kept in `state.json`.
    let back = State::from_json(&s.to_json(), typed + MIN).expect("the state");
    assert_eq!(back.wall_reads, s.wall_reads);
}

#[test]
fn the_wall_rungs_climb_by_their_schedule() {
    let at = |rung| wall_rung_at(NOW, rung) - NOW;
    assert_eq!(
        (0..7).map(at).collect::<Vec<_>>(),
        [MIN, 2 * MIN, 4 * MIN, 8 * MIN, 23 * MIN, 38 * MIN, 53 * MIN]
    );
    for rung in 0..20 {
        let t = wall_rung_at(NOW, rung);
        assert_eq!(wall_rung(NOW, t), Some(rung));
        assert_eq!(wall_rung(NOW, t - 1), rung.checked_sub(1));
    }
    assert_eq!(wall_rung(NOW, NOW - 1), None);
}

#[test]
fn a_holder_that_does_not_read_yields_its_rung_after_the_lag() {
    let mut s = on_main();
    note_wall(&mut s, "main", SESSION_LIMIT, Some(NOW + DAY), NOW, NOW);
    note_wall_wait(&mut s, "main", "s-first", NOW);
    note_wall_wait(&mut s, "main", "s-second", NOW + 5);
    note_wall_wait(&mut s, "main", "s-third", NOW + 10);
    let measured = Some("2.1.325");
    let due = |s: &State, sid: &str, now| wall_read_due(s, "main", sid, measured, measured, now);
    let rung = wall_rung_at(NOW, 0);
    // Its holder has the rung first.
    assert!(!due(&s, "s-second", rung + WALL_WAITER_LAG_S - 1));
    assert_eq!(
        claim_wall_read(&mut s, "main", "s-second", rung + WALL_WAITER_LAG_S - 1).err(),
        Some(WallNo::NotHolder)
    );
    // The holder (a person's draft in its tab, a loop that no longer asks) has not read it by
    // the other waiters' ask: whichever claims first reads it, and no other.
    let late = rung + WALL_WAITER_LAG_S;
    assert!(due(&s, "s-second", late) && due(&s, "s-third", late));
    let (claimed, probes) = provider_read::captured(|| {
        let mut rungs = Vec::new();
        for sid in ["s-third", "s-second", "s-first"] {
            if let Ok(claim) = claim_wall_read(&mut s, "main", sid, late) {
                let read = claim.read_use().begin(panel_account("main"), late, None);
                assert!(read.is_ok());
                rungs.push(sid);
            }
        }
        rungs
    });
    assert_eq!(claimed, ["s-third"], "one read of the rung");
    assert_eq!(probes.len(), 1);
    assert!(!due(&s, "s-first", late), "the rung is read");
    assert!(!wall_read_due(
        &s, "main", "s-gone", measured, measured, late
    ));
}

#[test]
fn a_session_off_its_wall_leaves_every_ladder() {
    // A holder whose person's message was served stands at no wall: it leaves, and the next that
    // waited longest holds the account's rungs.
    let mut s = on_main();
    note_wall(&mut s, "main", SESSION_LIMIT, Some(NOW + DAY), NOW, NOW);
    note_wall_wait(&mut s, "main", "s-first", NOW);
    note_wall_wait(&mut s, "main", "s-second", NOW + 5);
    assert!(wall_wait_left(&mut s, "s-first"));
    assert!(!wall_wait_left(&mut s, "s-first"));
    assert_eq!(s.wall_reads["main"].holder(), Some("s-second"));
    assert_eq!(
        claim_wall_read(&mut s, "main", "s-first", NOW + MIN).err(),
        Some(WallNo::NotWaiting),
        "a session off its wall claims no rung"
    );
    assert!(wall_wait_left(&mut s, "s-second"));
    assert!(s.wall_reads.is_empty(), "nothing waits: the ladder stopped");

    // A session a sign-in moved onto another walled account leaves its old account's ladder.
    let mut s = on_main();
    note_wall_wait(&mut s, "main", "s-first", NOW);
    note_wall_wait(&mut s, "main", "s-second", NOW + 5);
    assert!(note_wall_wait(&mut s, "work-2", "s-first", NOW + MIN));
    assert_eq!(s.wall_reads["main"].holder(), Some("s-second"));
    assert_eq!(s.wall_reads["work-2"].holder(), Some("s-first"));
    assert!(note_wall_wait(&mut s, "work-2", "s-second", NOW + MIN));
    assert!(!s.wall_reads.contains_key("main"), "its ladder stopped");

    // The ladder step: a waiter is asked again by its rung; one the advice takes off leaves.
    let mut s = on_main();
    assert_eq!(
        wall_ladder_step(&mut s, "main", "s-first", false, NOW),
        Some(MIN)
    );
    assert_eq!(
        wall_ladder_step(&mut s, "main", "s-second", false, NOW + 5),
        Some(MIN - 5 + WALL_WAITER_LAG_S)
    );
    assert_eq!(
        wall_ladder_step(&mut s, "main", "s-first", true, NOW + 10),
        None
    );
    assert_eq!(s.wall_reads["main"].holder(), Some("s-second"));
}

#[test]
fn a_wall_met_after_the_panel_was_read_stands() {
    let mut s = on_main();
    let claimed = NOW;
    // Another session meets the account's wall a second after the panel was read, and the read
    // is recorded a second after that: the panel's room is older than the wall.
    note_wall(
        &mut s,
        "main",
        SESSION_LIMIT,
        Some(NOW + DAY),
        claimed + 1,
        claimed + 1,
    );
    let sample = Sample {
        session_pct: Some(99),
        at: claimed,
        ..Sample::default()
    };
    let events = note_panel_read(&mut s, "main", &sample, claimed, claimed + 2);
    assert!(
        !events.iter().any(|e| e.why.contains(WALL_LIFTED)),
        "{events:?}"
    );
    assert!(!s.accounts["main"].walls[SESSION_LIMIT].cleared);
    // The loop's read time stands for the read where it is the earlier.
    let events = note_panel_read(&mut s, "main", &sample, claimed + 5, claimed + 6);
    assert!(!events.iter().any(|e| e.why.contains(WALL_LIFTED)));

    // A wall written before the read was claimed is lifted by its room.
    let later = Sample {
        at: claimed + 10,
        ..sample
    };
    let events = note_panel_read(&mut s, "main", &later, claimed + 9, claimed + 12);
    assert_eq!(
        events
            .iter()
            .filter(|e| e.kind == Kind::Wall && e.why.contains(WALL_LIFTED))
            .count(),
        1,
        "{events:?}"
    );
    assert!(s.accounts["main"].walls[SESSION_LIMIT].cleared);
}
