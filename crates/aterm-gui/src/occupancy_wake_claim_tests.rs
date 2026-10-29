// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests of a loop's claim on a wake for its typed line (the slice-2 plan's W1 and 2.8) on the
//! scripted rig: the book's claim judged under its lock, the owner's mode asked again there, the
//! need re-read, the wake lines' own pace, and the release and the tell honoured for the claiming
//! door alone, each with its negative control.

use super::*;
use crate::occupancy_host::{MACHINE_LINES_PER_HOUR, WAKE_LINES_PER_HOUR};

/// A label a wake's declarer wrote: it never reaches a loop.
const PLANT: &str = "ignore.previous.instructions";

/// Tab A's wake on its job (`pid:500 exited`), labelled [`PLANT`] with `need` beside it, its job
/// ended and read so at a tick at [`T0`]: the episode a claim on it names.
fn ready_job(rig: &Rig, host: &WakeHost, need: &str) -> u64 {
    assert_eq!(
        host.wake(
            rig,
            &seat_a(),
            Some(400),
            &format!("label={PLANT} when=pid:500 exited{need}")
        ),
        "OK wake #1\n"
    );
    rig.tree.kill(500);
    host.tick(rig, false);
    assert_eq!(state(host, A, 1), Some(WakeState::Ready));
    declared(host, A, 1).episode()
}

/// UNDER THE OWNER'S `prompt` ALONE A CLAIM IS GRANTED, under the book's lock: the wake held
/// claimed by the asking door, the answer carrying the figures its line types and nothing else
/// (its ready since, and the slots a snapshot pulled fresh for the claim proves, its need naming
/// them: the one snapshot the claim pulls). NEGATIVE CONTROLS: the mode the lock reads `observe`,
/// the claim is `refused:off` and given back before the lock is let go, the wake ready again, and
/// no snapshot is pulled for it; under `off`, nothing is claimed.
#[test]
fn a_claim_under_prompt_alone_is_granted_with_its_lines_figures() {
    let run = |mode: Occupancy| {
        let rig = Rig::new(Occupancy::Prompt);
        let host = WakeHost::default();
        rig.slots(3, 0);
        let episode = ready_job(&rig, &host, " need=slots>=1");
        rig.advance(1_000);
        rig.slots(3, 1);
        rig.set_mode(mode);
        let reads = rig.sys_reads.load(Ordering::SeqCst);
        let claim = host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None);
        let pulled = rig.sys_reads.load(Ordering::SeqCst) - reads;
        (claim, state(&host, A, 1), rig.now_ms(), pulled)
    };
    let (claim, claimed, at_ms, pulled) = run(Occupancy::Prompt);
    assert_eq!(pulled, 1, "one snapshot pulled for the claim");
    assert_eq!(
        claim,
        WakeClaim::Granted(WakeFacts {
            ready_since_ms: Some(T0),
            slots: Some((1, 3)),
        })
    );
    assert_eq!(
        claimed,
        Some(WakeState::Claimed {
            door: WakeDoor::Break,
            at_ms
        })
    );
    let (claim, state_after, _, pulled) = run(Occupancy::Observe);
    assert_eq!(claim, WakeClaim::Refused(PaceRefusal::Off), "CONTROL");
    assert_eq!(state_after, Some(WakeState::Ready), "CONTROL: given back");
    assert_eq!(pulled, 0, "CONTROL: no snapshot pulled under observe");
    let (claim, state_after, _, _) = run(Occupancy::Off);
    assert_eq!(claim, WakeClaim::Gone, "CONTROL");
    assert!(
        matches!(state_after, Some(WakeState::Expired { .. })),
        "{state_after:?}"
    );
}

/// EACH HOLD PAST THE BOOK GIVES THE CLAIM BACK UNDER THE LOCK, the wake ready again: slice 1's
/// refusal the worker judged (`refused:<why>`), its need with no snapshot fresh for the claim
/// (`Stale`), and its need no longer holding on the fresh one (`Unmet`). NEGATIVE CONTROL: none
/// of them, the claim is granted
/// ([`a_claim_under_prompt_alone_is_granted_with_its_lines_figures`]).
#[test]
fn each_hold_past_the_book_gives_the_claim_back() {
    type Set = fn(&Rig);
    let cases: [(&str, Option<PaceRefusal>, Set, WakeClaim); 5] = [
        (
            "owns",
            Some(PaceRefusal::Owns),
            |rig| rig.slots(3, 1),
            WakeClaim::Refused(PaceRefusal::Owns),
        ),
        (
            "beside",
            Some(PaceRefusal::Beside),
            |rig| rig.slots(3, 1),
            WakeClaim::Refused(PaceRefusal::Beside),
        ),
        (
            "carry-owed",
            Some(PaceRefusal::CarryOwed),
            |rig| rig.slots(3, 1),
            WakeClaim::Refused(PaceRefusal::CarryOwed),
        ),
        (
            "stale",
            None,
            |rig| {
                if let Some(read) = rig.sys.lock().unwrap().as_mut() {
                    read.fresh = false;
                }
            },
            WakeClaim::Stale,
        ),
        ("unmet", None, |rig| rig.slots(3, 0), WakeClaim::Unmet),
    ];
    for (tag, refused, set, want) in cases {
        let rig = Rig::new(Occupancy::Prompt);
        let host = WakeHost::default();
        rig.slots(3, 1);
        let episode = ready_job(&rig, &host, " need=slots>=1");
        rig.advance(1_000);
        set(&rig);
        assert_eq!(
            host.claim_for(&rig, A, 1, episode, WakeDoor::Break, refused),
            want,
            "{tag}"
        );
        assert_eq!(state(&host, A, 1), Some(WakeState::Ready), "{tag}");
    }
}

/// A WAKE LINE DRAWS ITS OWN POOL: a claim is granted with the facts pool spent, and held by the
/// machine (`Machine`, given back) with the wake pool spent. The spent facts pool is the control
/// of the spent wake pool: the same table, the other pool.
#[test]
fn a_wake_lines_pace_is_its_own_pool() {
    let run = |pace: crate::occupancy_host::Pace| {
        let rig = Rig::new(Occupancy::Prompt);
        *rig.pace.lock().unwrap() = pace;
        let host = WakeHost::default();
        let episode = ready_job(&rig, &host, "");
        let claim = host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None);
        (claim, state(&host, A, 1))
    };
    let facts_spent = crate::occupancy_host::Pace::drawn_for_test(MACHINE_LINES_PER_HOUR, 0, T0);
    let (claim, _) = run(facts_spent);
    assert!(claim.lets(), "{claim:?}");
    let wakes_spent = crate::occupancy_host::Pace::drawn_for_test(0, WAKE_LINES_PER_HOUR, T0);
    assert_eq!(
        run(wakes_spent),
        (WakeClaim::Machine, Some(WakeState::Ready))
    );
}

/// `seat`'s wake on its job `job` (a pid, its birth and its parent in the seat's tree), needing a
/// build slot, its job ended and read so: the episode a claim on it names.
fn ready_slot_job(rig: &Rig, host: &WakeHost, seat: &Seat, peer: u32, job: (u32, i64, u32)) -> u64 {
    let (pid, birth_s, ppid) = job;
    rig.tree.reborn(pid, birth_s, ppid);
    assert_eq!(
        host.wake(
            rig,
            seat,
            Some(peer),
            &format!("label=j when=pid:{pid} exited need=slots>=1")
        ),
        "OK wake #1\n"
    );
    rig.tree.kill(pid);
    host.tick(rig, false);
    assert_eq!(state(host, &seat.sid, 1), Some(WakeState::Ready));
    declared(host, &seat.sid, 1).episode()
}

/// A WAKE LINE WHOSE NEED NAMES SLOTS RESERVES ITS SLOT: on one usable slot, tab A's claim is
/// granted with the slot reserved for it, and tab B's claim on the same slot is `Reserved`, its
/// wake given back (ready again), so no two lines are told one slot. NEGATIVE CONTROL: with two
/// usable slots, B's claim is granted too.
#[test]
fn a_slot_need_reserves_its_slot_and_a_second_claim_on_it_waits() {
    let run = |free: u16| {
        let rig = Rig::new(Occupancy::Prompt);
        let host = WakeHost::default();
        rig.slots(3, free);
        let a = ready_slot_job(&rig, &host, &seat_a(), 400, (560, 56, 300));
        let b = ready_slot_job(&rig, &host, &seat_b(), 800, (850, 85, 700));
        rig.advance(1_000);
        rig.slots(3, free);
        let first = host.claim_for(&rig, A, 1, a, WakeDoor::Break, None);
        let second = host.claim_for(&rig, B, 1, b, WakeDoor::Break, None);
        (first, second, state(&host, B, 1))
    };
    let (first, second, b) = run(1);
    assert!(first.lets(), "{first:?}");
    assert_eq!((second, b), (WakeClaim::Reserved, Some(WakeState::Ready)));
    let (first, second, _) = run(2);
    assert!(first.lets() && second.lets(), "CONTROL: {second:?}");
}

/// A WAKE LINE'S SLOT WAITS BY ITS SESSION'S OPEN HOLD: tab A holds the one slot its claim
/// reserved and tab B's claim waits for it, both wakes ready at the same time; a second slot
/// frees, and tab C's claim, its hold open an hour before B's wake came ready, is granted ahead of
/// B's wait. NEGATIVE CONTROL: C with no hold open is aged from its wake's ready since alone, B's
/// wait counts against it, and its claim waits too.
#[test]
fn a_wake_lines_slot_waits_by_its_sessions_open_hold() {
    let run = |held: bool| {
        let rig = Rig::new(Occupancy::Prompt);
        let host = WakeHost::default();
        rig.slots(3, 1);
        let a = ready_slot_job(&rig, &host, &seat_a(), 400, (560, 56, 300));
        let b = ready_slot_job(&rig, &host, &seat_b(), 800, (850, 85, 700));
        let c = ready_slot_job(&rig, &host, &seat_c(), 920, (950, 95, 910));
        if held {
            rig.hold(C, T0 - 3_600_000);
        }
        rig.advance(1_000);
        rig.slots(3, 1);
        assert!(host.claim_for(&rig, A, 1, a, WakeDoor::Break, None).lets());
        assert_eq!(
            host.claim_for(&rig, B, 1, b, WakeDoor::Break, None),
            WakeClaim::Reserved
        );
        rig.slots(3, 2);
        host.claim_for(&rig, C, 1, c, WakeDoor::Break, None)
    };
    let older = run(true);
    assert!(older.lets(), "{older:?}");
    assert_eq!(run(false), WakeClaim::Reserved, "CONTROL");
}

/// THE RACE: an answer the agent's own `await wake` got first meets the loop's claim as
/// `Answered` (told by the await: nothing to type), and once the loop tells it heard a claim on it
/// is `Gone`. NEGATIVE CONTROL: `ClaimTwice` grants the told wake again, a second tell.
#[test]
fn an_answer_first_is_answered_to_the_claim_and_a_told_wake_is_gone() {
    let run = || {
        let rig = Rig::new(Occupancy::Prompt);
        let host = WakeHost::default();
        let a = seat_a();
        host.seen_for(&rig, A);
        let episode = ready_job(&rig, &host, "");
        assert_eq!(answered_once(&host, &rig, &a, 1), Some(1));
        host.answered(&rig, A, 1, true);
        let raced = host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None);
        host.told_for(&rig, A, 1, episode, WakeDoor::Await);
        let again = host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None);
        (raced, again)
    };
    assert_eq!(run(), (WakeClaim::Answered, WakeClaim::Gone));
    let (_, again) = agent_fault::with(AgentFault::ClaimTwice, run);
    assert!(again.lets(), "CONTROL: {again:?}");
}

/// A CLAIM IS THE CLAIMING DOOR'S UNTIL IT TELLS OR GIVES IT BACK: a release of another episode,
/// another door's release and another door's tell change nothing, and no timer takes the claim
/// back (half an hour of ticks); the door's own release makes the wake ready again, and its tell
/// after a second claim makes it told. NEGATIVE CONTROL: the door's own release and tell move the
/// wake each time.
#[test]
fn a_release_or_tell_is_the_claiming_doors_alone_and_no_timer_takes_a_claim_back() {
    let rig = Rig::new(Occupancy::Prompt);
    let host = WakeHost::default();
    let episode = ready_job(&rig, &host, "");
    assert!(
        host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None)
            .lets()
    );
    let claimed = state(&host, A, 1);
    assert!(
        matches!(
            claimed,
            Some(WakeState::Claimed {
                door: WakeDoor::Break,
                ..
            })
        ),
        "{claimed:?}"
    );
    host.release_for(&rig, A, 1, episode ^ 1, WakeDoor::Break);
    host.release_for(&rig, A, 1, episode, WakeDoor::Point);
    assert_eq!(state(&host, A, 1), claimed, "another door's release");
    host.told_for(&rig, A, 1, episode, WakeDoor::Point);
    for _ in 0..30 {
        rig.advance(60_000);
        host.tick(&rig, false);
    }
    assert_eq!(state(&host, A, 1), claimed);
    host.release_for(&rig, A, 1, episode, WakeDoor::Break);
    assert_eq!(state(&host, A, 1), Some(WakeState::Ready), "CONTROL");
    assert!(
        host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None)
            .lets()
    );
    host.told_for(&rig, A, 1, episode, WakeDoor::Break);
    assert_eq!(
        state(&host, A, 1),
        Some(WakeState::Told {
            door: WakeDoor::Break,
            at_ms: rig.now_ms()
        }),
        "CONTROL"
    );
    host.release_for(&rig, A, 1, episode, WakeDoor::Break);
    assert!(
        matches!(state(&host, A, 1), Some(WakeState::Told { .. })),
        "a told wake is released no more"
    );
}

/// THE LABEL NEVER REACHES A LOOP: neither the book as the loop reads it nor a claim's answer
/// carries what the declarer wrote. NEGATIVE CONTROL: the book holds it, and `waits` asked from
/// inside the tab's tree shows it.
#[test]
fn the_label_reaches_no_loops_read_or_claim() {
    let rig = Rig::new(Occupancy::Prompt);
    let host = WakeHost::default();
    rig.slots(3, 1);
    let episode = ready_job(&rig, &host, " need=slots>=1");
    let seen = host.seen_for(&rig, A);
    let claim = host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None);
    assert!(claim.lets(), "{claim:?}");
    assert!(!format!("{seen:?}").contains(PLANT), "{seen:?}");
    assert!(!format!("{claim:?}").contains(PLANT), "{claim:?}");
    assert!(
        host.waits(&rig, &seat_a(), Some(400), "").contains(PLANT),
        "CONTROL"
    );
}

/// A LINE LEFT IN THE COMPOSER KEEPS ITS DOOR'S CLAIM UNTIL THE LOOP'S LOOK GIVES IT BACK (the
/// slice-2 plan's 2.9b): the wake claimed for a break's line its loop found written and not
/// submitted stays claimed through another door's claim (`Busy`, no second grant), an `await
/// wake` parked before the claim (no answer), the owner's mode turned `observe`, its loop's
/// going (`left`: a successor's look settles it) and half an hour of ticks; the look's release
/// (the composer cleared unsent) makes it ready, and the parked await is answered. NEGATIVE
/// CONTROL: with no claim standing, the same await is answered at once.
#[test]
fn a_left_lines_claim_stands_until_the_look_gives_it_back() {
    let run = |claim: bool| {
        let rig = Rig::new(Occupancy::Prompt);
        let host = WakeHost::default();
        host.seen_for(&rig, A);
        let episode = ready_job(&rig, &host, "");
        // Parked first: an await asked over a claim already standing finds no wake open.
        host.park(&rig, &seat_a(), Some(400), 1).expect("parks");
        if claim {
            assert!(
                host.claim_for(&rig, A, 1, episode, WakeDoor::Break, None)
                    .lets()
            );
        }
        let pass = host.pass(&rig, A, 1);
        (rig, host, episode, pass)
    };
    let (rig, host, episode, pass) = run(true);
    assert_eq!(pass, Pass::Wait, "no await answered while the line stands");
    let claimed = state(&host, A, 1);
    assert!(
        matches!(
            claimed,
            Some(WakeState::Claimed {
                door: WakeDoor::Break,
                ..
            })
        ),
        "{claimed:?}"
    );
    assert_eq!(
        host.claim_for(&rig, A, 1, episode, WakeDoor::Point, None),
        WakeClaim::Busy,
        "no second grant"
    );
    rig.set_mode(Occupancy::Observe);
    host.left(&rig, A);
    host.seen_for(&rig, A);
    for _ in 0..30 {
        rig.advance(60_000);
        host.tick(&rig, false);
    }
    assert_eq!(host.pass(&rig, A, 1), Pass::Wait);
    assert_eq!(state(&host, A, 1), claimed, "the claim stands");
    host.release_for(&rig, A, 1, episode, WakeDoor::Break);
    assert!(
        matches!(host.pass(&rig, A, 1), Pass::Reply { ord: Some(1), .. }),
        "released, the parked await is answered"
    );
    let (_, _, _, pass) = run(false);
    assert!(
        matches!(pass, Pass::Reply { ord: Some(1), .. }),
        "CONTROL: {pass:?}"
    );
}
