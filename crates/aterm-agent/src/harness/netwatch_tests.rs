// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const S: Duration = Duration::from_secs(1);

fn t0() -> (Instant, SystemTime) {
    (
        Instant::now() + Duration::from_secs(3600),
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000),
    )
}

/// One probe, measured at `t` on both clocks.
fn measured(st: &mut NetState, outcome: Outcome, t: Instant, w: SystemTime) {
    assert!(st.begin(t));
    st.finish(outcome, t, w);
}

/// Asks expire lapsed leases themselves while a stuck probe keeps the
/// worker from pruning, and start no second probe.
#[test]
fn asks_expire_churned_sessions_even_while_the_probe_is_stuck() {
    let (t, w) = t0();
    let mut st = NetState::default();
    let mut old = NetState::default();
    assert!(st.begin(t));
    assert!(old.begin(t));
    for n in 0..128_u32 {
        let elapsed = NET_LEASE * n;
        let at = t + elapsed;
        let wall = w + elapsed;
        assert_eq!(st.ask("live-peer", at, wall), Reach::Unknown);
        let who = format!("churn-{n}");
        assert_eq!(st.ask(&who, at, wall), Reach::Unknown);
        assert_eq!(st.waiters.len(), 2, "old leases retire at their boundary");
        assert!(st.waiters.contains_key("live-peer"));
        assert!(st.waiters.contains_key(&who));
        assert_eq!(st.flight, Some(t), "no second probe is started");

        old.waiters.insert("live-peer".to_string(), at);
        old.waiters.insert(who, at);
        assert_eq!(old.reach(at, wall), Reach::Unknown);
    }
    assert_eq!(old.waiters.len(), 129, "the old path grows with history");

    let at = t + NET_LEASE * 127;
    st.ask("new-peer", at + NET_LEASE - S, w + NET_LEASE * 128 - S);
    assert_eq!(st.waiters.len(), 3, "leases not yet expired stay live");
}

/// The schedule: parked while nobody waits, a probe at once for the first
/// waiter, then the gap's one quick probe, the Down ladder of 5, 10 and 15 s, and the
/// steady cadence after an Up, one in flight at a time.
#[test]
fn the_schedule_probes_at_once_then_3_5_10_15_while_someone_waits_and_parks_when_nobody_does() {
    let (t, w) = t0();
    let mut st = NetState::default();
    assert_eq!(st.next_probe(t, w), None, "nobody waits: parked");
    assert_eq!(st.ask("s-1", t, w), Reach::Unknown, "no measure yet");
    assert_eq!(st.next_probe(t, w), Some(t), "at once");
    assert!(st.begin(t));
    assert!(!st.begin(t), "one in flight at a time");
    assert_eq!(st.next_probe(t, w), None, "none while one flies");
    st.finish(Outcome::Down, t, w);
    let mut at = t;
    for pause in [3, 5, 10, 15, 15] {
        let due = st.next_probe(at, w).expect("a waiter");
        assert_eq!(due, at + pause * S, "after a Down: {pause} s");
        at = due;
        st.ask("s-1", at, w);
        measured(&mut st, Outcome::Down, at, w);
    }
    measured(&mut st, Outcome::Up, at, w);
    assert_eq!(st.next_probe(at, w), Some(at + 60 * S), "after an Up");
    assert_eq!(st.next_probe(at + 61 * S, w), None);
    assert_eq!(st.waiting(at + 61 * S), 0);
    st.ask("s-1", at + 50 * S, w);
    assert!(st.next_probe(at + 61 * S, w).is_some());
}

/// A measure that ended a gap and did not say Up is followed by one probe in
/// [`GAP_CADENCE`], whether the gap was the first ask or a sleep that made the last
/// measure stale; the Down ladder starts after it, and a Down or an Unknown after a fresh
/// measure climbs the ladder.
#[test]
fn a_failed_measure_after_a_gap_is_probed_again_within_seconds() {
    let (t, w) = t0();
    let mut st = NetState::default();
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    assert_eq!(
        st.next_probe(t, w),
        Some(t + 60 * S),
        "the gap's Up: steady"
    );
    let at = t + 60 * S;
    st.ask("s-1", at, w + 60 * S);
    measured(&mut st, Outcome::Down, at, w + 60 * S);
    assert_eq!(
        st.next_probe(at, w + 60 * S),
        Some(at + 5 * S),
        "a Down after a fresh Up: the ladder's first rung"
    );
    // The Mac sleeps an hour: the wall clock moves, the monotonic one does not.
    let (woke, woke_wall) = (at + S, w + 3660 * S);
    st.ask("s-1", woke, woke_wall);
    assert_eq!(
        st.reach(woke, woke_wall),
        Reach::Unknown,
        "PRECONDITION: stale"
    );
    assert_eq!(
        st.next_probe(woke, woke_wall),
        Some(woke),
        "at once after the sleep"
    );
    measured(&mut st, Outcome::Down, woke, woke_wall);
    assert_eq!(
        st.next_probe(woke, woke_wall),
        Some(woke + 3 * S),
        "the Down that ended the gap: once more within seconds"
    );
    let again = woke + 3 * S;
    st.ask("s-1", again, woke_wall + 3 * S);
    measured(&mut st, Outcome::Down, again, woke_wall + 3 * S);
    assert_eq!(
        st.next_probe(again, woke_wall + 3 * S),
        Some(again + 5 * S),
        "then the ladder from its first rung"
    );
    assert_eq!(
        st.reach(again, woke_wall + 3 * S),
        Reach::Down { since: woke },
        "one run of Downs since the gap"
    );
    let mut first = NetState::default();
    first.ask("s-1", t, w);
    assert!(first.begin(t));
    first.finish(Outcome::Unknown, t, w);
    assert_eq!(
        first.next_probe(t, w),
        Some(t + 3 * S),
        "an Unknown that ended the gap too"
    );
    measured(&mut first, Outcome::Unknown, t + 3 * S, w + 3 * S);
    assert_eq!(
        first.next_probe(t + 3 * S, w + 3 * S),
        Some(t + 8 * S),
        "then the ladder's first rung, as after a Down"
    );
}

/// While the API is not up the probe comes back within seconds: an Unknown (a timeout, a
/// TLS failure, an overrun) that did not end a gap climbs the same 5, 10 and 15 s ladder as
/// a Down, a Down among Unknowns keeps its rung, and only an Up steadies the schedule at
/// 60 s, after which the next miss starts the ladder again.
#[test]
fn an_unknown_climbs_the_down_ladder_and_only_an_up_steadies_the_schedule() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    let mut at = t + 60 * S;
    for (outcome, pause) in [
        (Outcome::Unknown, 5),
        (Outcome::Unknown, 10),
        (Outcome::Unknown, 15),
        (Outcome::Down, 15),
        (Outcome::Unknown, 15),
    ] {
        st.ask("s-1", at, on_wall(at));
        measured(&mut st, outcome, at, on_wall(at));
        let due = st.next_probe(at, on_wall(at)).expect("a waiter");
        assert_eq!(due, at + pause * S, "after {outcome:?}: {pause} s");
        at = due;
    }
    st.ask("s-1", at, on_wall(at));
    assert!(st.begin(at));
    let over = at + PROBE_BUDGET + S;
    st.finish(Outcome::Up, over, on_wall(over));
    assert_eq!(
        st.next_probe(over, on_wall(over)),
        Some(over + 15 * S),
        "an overrun is an Unknown: the ladder's top rung"
    );
    let up = over + 15 * S;
    st.ask("s-1", up, on_wall(up));
    measured(&mut st, Outcome::Up, up, on_wall(up));
    assert_eq!(
        st.next_probe(up, on_wall(up)),
        Some(up + 60 * S),
        "an Up: the steady cadence"
    );
    let next = up + 60 * S;
    st.ask("s-1", next, on_wall(next));
    measured(&mut st, Outcome::Unknown, next, on_wall(next));
    assert_eq!(
        st.next_probe(next, on_wall(next)),
        Some(next + 5 * S),
        "a miss after the Up: the ladder from its first rung"
    );
}

/// `landing` says when a new measure can next land, always later than now: nothing with no
/// lease, the next probe's due time while one is ahead, a poll while one is due and not
/// begun or in flight, and a flight's budget's end when that comes before the poll.
#[test]
fn the_landing_is_the_next_measures_earliest_instant_and_never_now() {
    let (t, w) = t0();
    let mut st = NetState::default();
    assert_eq!(st.landing(t, w), None, "no lease: no probe will run");
    st.ask("s-1", t, w);
    assert_eq!(
        st.landing(t, w),
        Some(t + REACH_POLL),
        "a probe due now: a poll, never now"
    );
    assert!(st.begin(t));
    assert_eq!(
        st.landing(t + 2 * S, w + 2 * S),
        Some(t + 3 * S),
        "in flight: the poll"
    );
    let end = t + PROBE_BUDGET;
    let near = end - Duration::from_millis(300);
    assert_eq!(
        st.landing(near, w + 9 * S),
        Some(end),
        "in flight near its budget's end: the end, where an overrun reads Unknown"
    );
    assert_eq!(
        st.landing(end + 2 * S, w + 12 * S),
        Some(end + 3 * S),
        "an overrun still in flight: the poll"
    );
    st.finish(Outcome::Down, t + S, w + S);
    assert_eq!(
        st.landing(t + S, w + S),
        Some(t + S + GAP_CADENCE),
        "the gap's Down: its next probe's due time"
    );
    let late = t + S + GAP_CADENCE + 2 * S;
    assert_eq!(
        st.landing(late, w + 6 * S),
        Some(late + REACH_POLL),
        "a probe due and not begun: the poll"
    );
    let at = t + 10 * S;
    st.ask("s-1", at, w + 10 * S);
    measured(&mut st, Outcome::Up, at, w + 10 * S);
    assert_eq!(
        st.landing(at, w + 10 * S),
        Some(at + STEADY_CADENCE),
        "after an Up: the steady cadence"
    );
    assert_eq!(
        st.landing(at + NET_LEASE, w + 10 * S + NET_LEASE),
        None,
        "the lease lapsed: no probe will run"
    );
}

/// A path that came back owes one probe once it has settled, not the steady cadence's
/// minute: due [`PATH_SETTLE`] after the return (the watch's first reading owes none),
/// brought into `next_probe` and `landing` only while a lease lives, and taken by the probe
/// that begins then; a miss of that probe is followed by the gap's one quick probe and then
/// the ladder, and a loss before it is due drops it.
#[test]
fn a_path_that_comes_back_is_probed_once_it_settles_not_a_minute_later() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    assert!(
        !st.path(true, t, w),
        "the watch's first reading owes no probe"
    );
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    assert_eq!(
        st.next_probe(t, w),
        Some(t + 60 * S),
        "PRECONDITION: steady"
    );
    assert!(!st.path(true, t + 5 * S, on_wall(t + 5 * S)), "no change");
    assert!(
        !st.path(false, t + 9 * S, on_wall(t + 9 * S)),
        "a loss owes none"
    );
    let back = t + 10 * S;
    assert!(st.path(true, back, on_wall(back)), "a return owes one");
    assert_eq!(
        st.next_probe(back, on_wall(back)),
        Some(back + PATH_SETTLE),
        "the return's probe, not t + 60 s"
    );
    assert_eq!(st.landing(back, on_wall(back)), Some(back + PATH_SETTLE));
    let due = back + PATH_SETTLE;
    assert!(st.begin(due));
    st.finish(Outcome::Down, due + S, on_wall(due + S));
    assert_eq!(
        st.next_probe(due + S, on_wall(due + S)),
        Some(due + S + GAP_CADENCE),
        "a miss of the return's probe: once more within seconds"
    );
    let again = due + S + GAP_CADENCE;
    measured(&mut st, Outcome::Down, again, on_wall(again));
    assert_eq!(
        st.next_probe(again, on_wall(again)),
        Some(again + 5 * S),
        "then the ladder from its first rung"
    );
    let up = again + 5 * S;
    measured(&mut st, Outcome::Up, up, on_wall(up));
    assert_eq!(
        st.next_probe(up, on_wall(up)),
        Some(up + 60 * S),
        "taken once"
    );

    let mut idle = NetState::default();
    assert!(!idle.path(false, t, w));
    assert!(idle.path(true, t + S, w + S));
    assert_eq!(idle.next_probe(t + S, w + S), None, "no lease: no probe");
    idle.ask("s-1", t + 2 * S, w + 2 * S);
    assert_eq!(
        idle.next_probe(t + 2 * S, w + 2 * S),
        Some(t + 2 * S),
        "no measure yet: at once, as before"
    );
    measured(&mut idle, Outcome::Up, t + 2 * S, w + 2 * S);
    assert_eq!(
        idle.next_probe(t + 2 * S, w + 2 * S),
        Some(t + 3 * S),
        "a probe that began before the return's was due left it owed"
    );
    assert!(!idle.path(false, t + 2 * S + S / 2, w + 2 * S));
    assert_eq!(
        idle.next_probe(t + 3 * S, w + 3 * S),
        Some(t + 62 * S),
        "a loss before the return's probe was due dropped it: the steady cadence alone"
    );
}

/// Path changes cost at most one probe per [`GAP_CADENCE`]: returns that come in a storm
/// move the owed probe to the last one's settle, and one after a measure waits out the gap
/// cadence from it; only the probe that begins at the due time takes it.
#[test]
fn a_storm_of_path_changes_owes_one_probe_per_gap_cadence() {
    let (t, w) = t0();
    let ms = Duration::from_millis(1);
    let mut st = NetState::default();
    st.path(true, t, w);
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    let mut at = t + 30 * S;
    for _ in 0..10 {
        st.path(false, at, w + 30 * S);
        at += 50 * ms;
        st.path(true, at, w + 30 * S);
        at += 50 * ms;
    }
    let last = at - 50 * ms;
    assert_eq!(
        st.next_probe(at, w + 31 * S),
        Some(last + PATH_SETTLE),
        "one owed probe, once the storm has settled"
    );
    let due = last + PATH_SETTLE;
    measured(&mut st, Outcome::Up, due, w + 33 * S);
    let back = due + 500 * ms;
    st.path(false, due + 100 * ms, w + 33 * S);
    assert!(st.path(true, back, w + 33 * S));
    assert_eq!(
        st.next_probe(back, w + 33 * S),
        Some(due + GAP_CADENCE),
        "no sooner than the gap cadence after the last measure"
    );
    let early = due + 2 * S;
    assert!(st.begin(early));
    st.finish(Outcome::Up, early + ms, w + 35 * S);
    assert_eq!(
        st.next_probe(early + ms, w + 35 * S),
        Some(due + GAP_CADENCE),
        "a probe that began before the owed one was due does not take it"
    );
    measured(&mut st, Outcome::Up, due + GAP_CADENCE, w + 36 * S);
    assert_eq!(
        st.next_probe(due + GAP_CADENCE, w + 36 * S),
        Some(due + GAP_CADENCE + 60 * S),
        "the owed one taken: steady"
    );
}

/// A lost path reads Down at once, from the loss, and the measures taken while it is lost
/// keep that run (no edge); its return is no measure, so the Down stands until a probe
/// measures it (or the return grows stale), and a path that can carry traffic never reads
/// Up by itself. A probe that overruns while the path is lost reads Down too, and an Up
/// measured after the loss outweighs it.
#[test]
fn a_lost_path_reads_down_until_a_probe_measures_its_return() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    assert!(!st.path(true, t, w));
    assert_eq!(st.reach(t, w), Reach::Unknown, "a satisfied path is no Up");
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    assert_eq!(st.reach(t + S, on_wall(t + S)), Reach::Up { since: t });
    let lost = t + 5 * S;
    st.path(false, lost, on_wall(lost));
    assert_eq!(
        st.reach(lost, on_wall(lost)),
        Reach::Down { since: lost },
        "a lost path reads Down at once, not at the next probe"
    );
    assert_eq!(
        probed(
            &mut st,
            Outcome::Down,
            lost + 2 * S,
            S,
            on_wall(lost + 3 * S)
        ),
        None,
        "a Down measured while lost continues the loss's run: no edge"
    );
    assert_eq!(
        st.reach(lost + 3 * S, on_wall(lost + 3 * S)),
        Reach::Down { since: lost }
    );
    assert_eq!(
        probed(
            &mut st,
            Outcome::Unknown,
            lost + 8 * S,
            S,
            on_wall(lost + 9 * S)
        ),
        None,
        "an Unknown measured while lost does not outweigh the loss"
    );
    let stuck = lost + 20 * S;
    assert!(st.begin(stuck));
    assert_eq!(
        st.reach(stuck + PROBE_BUDGET + S, on_wall(stuck + PROBE_BUDGET + S)),
        Reach::Down { since: lost },
        "an overrun while the path is lost still reads Down"
    );
    st.finish(
        Outcome::Unknown,
        stuck + PROBE_BUDGET + S,
        on_wall(stuck + PROBE_BUDGET + S),
    );
    let back = lost + 40 * S;
    assert!(st.path(true, back, on_wall(back)));
    assert_eq!(
        st.reach(back, on_wall(back)),
        Reach::Down { since: lost },
        "the return is no measure: still Down"
    );
    assert_eq!(
        st.reach(back + FRESH, on_wall(back + FRESH)),
        Reach::Unknown,
        "an unmeasured return grows stale as a measure would"
    );
    let (due, took) = (back + PATH_SETTLE, Duration::from_millis(100));
    assert_eq!(
        probed(&mut st, Outcome::Up, due, took, on_wall(due)),
        Some(Edge {
            from: Some(Outcome::Down),
            to: Outcome::Up,
            held: due + took - lost,
            took,
        }),
        "the return measured: the edge from the loss"
    );
    assert_eq!(
        st.reach(due + S, on_wall(due + S)),
        Reach::Up { since: due + took }
    );

    let mut first = NetState::default();
    assert!(
        !first.path(false, t, w),
        "a first reading that cannot carry traffic"
    );
    assert_eq!(first.reach(t + S, w + S), Reach::Down { since: t });
    first.ask("s-1", t + S, w + S);
    measured(&mut first, Outcome::Up, t + 2 * S, w + 2 * S);
    assert_eq!(
        first.reach(t + 2 * S, w + 2 * S),
        Reach::Up { since: t + 2 * S },
        "an Up measured after the loss outweighs it"
    );
}

/// A move of this Mac's routes that leaves the path usable (or that a host with no path
/// reading hears alone) owes one probe at the settle's end, spaced from the last measure,
/// instead of the steady minute; it changes no verdict, owes nothing while the path reads
/// lost or while no lease lives, and the owed probe once measured leaves the cadence's own.
#[test]
fn a_route_move_on_a_usable_path_owes_one_probe_and_changes_no_verdict() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    assert!(!st.moved(t), "no lease: no probe runs, so none is owed");
    assert_eq!(st.next_probe(t, w), None);
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    assert_eq!(st.next_probe(t, w), Some(t + STEADY_CADENCE));
    assert!(
        st.moved(t),
        "no path reading: the move alone owes the probe"
    );
    assert_eq!(
        st.next_probe(t, w),
        Some(t + GAP_CADENCE),
        "spaced from the last measure, not the steady minute"
    );
    let owed = t + GAP_CADENCE;
    measured(&mut st, Outcome::Up, owed, on_wall(owed));
    assert_eq!(
        st.next_probe(owed, on_wall(owed)),
        Some(owed + STEADY_CADENCE),
        "the owed probe measured: the cadence's own again"
    );

    let at = owed + 10 * S;
    assert!(st.moved(at));
    assert_eq!(
        st.next_probe(at, on_wall(at)),
        Some(at + PATH_SETTLE),
        "the settle's end"
    );
    assert_eq!(
        st.reach(at, on_wall(at)),
        Reach::Up { since: t },
        "a move changes no verdict"
    );
    assert_eq!(st.path_satisfied(), None, "nor the path's reading");

    assert!(!st.path(true, at, on_wall(at)));
    let usable = at + 5 * S;
    assert!(st.moved(usable), "a usable path that did not change");
    assert_eq!(st.path_satisfied(), Some(true));
    let lost = usable + S;
    st.path(false, lost, on_wall(lost));
    assert!(
        !st.moved(lost + S),
        "a lost path: its return owes the probe"
    );
    assert_eq!(
        st.next_probe(lost + S, on_wall(lost + S)),
        Some(owed + STEADY_CADENCE),
        "the loss dropped the move's probe and the move owed none"
    );
    assert_eq!(st.path_satisfied(), Some(false), "a move reads no path");
}

/// A session the probe does not measure reads the path alone: Unknown with no loss, Down
/// from the loss while the path is lost and while its return settles, then Unknown at once
/// (no FRESH hold, since nothing will measure it). A Down of the API host does not count;
/// a fresh Up measured since the loss lifts it (traffic left this Mac), and the loss holds
/// again once that Up is stale. Its looks fall on the lost path's poll and the settle's end.
#[test]
fn the_path_alone_reads_down_while_lost_and_settling_and_an_up_since_the_loss_lifts_it() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    assert_eq!(st.path_reach(t, w), Reach::Unknown, "no reading yet");
    assert_eq!(st.path_landing(t, w), None);
    assert!(!st.path(true, t, w));
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Down, t, w);
    assert_eq!(st.reach(t + S, on_wall(t + S)), Reach::Down { since: t });
    assert_eq!(
        st.path_reach(t + S, on_wall(t + S)),
        Reach::Unknown,
        "the API host's Down is not this session's"
    );
    assert_eq!(
        st.path_landing(t + S, on_wall(t + S)),
        None,
        "only a loss moves it"
    );

    let lost = t + 10 * S;
    st.path(false, lost, on_wall(lost));
    assert_eq!(
        st.path_reach(lost, on_wall(lost)),
        Reach::Down { since: lost }
    );
    assert_eq!(
        st.path_landing(lost + S, on_wall(lost + S)),
        Some(lost + S + LOST_POLL),
        "a lost path is looked at again on the poll"
    );
    measured(&mut st, Outcome::Down, lost + S, on_wall(lost + S));
    assert_eq!(
        st.path_reach(lost + 2 * S, on_wall(lost + 2 * S)),
        Reach::Down { since: lost },
        "a Down of the API host neither lifts nor moves the loss"
    );
    let up = lost + 2 * S;
    measured(&mut st, Outcome::Up, up, on_wall(up));
    assert_eq!(
        st.reach(up + S, on_wall(up + S)),
        Reach::Up { since: up },
        "PRECONDITION: the probe measured the API host up"
    );
    assert_eq!(
        st.path_reach(up + S, on_wall(up + S)),
        Reach::Unknown,
        "an Up measured since the loss lifts it: traffic left this Mac"
    );
    assert_eq!(
        st.path_landing(up + S, on_wall(up + S)),
        Some(up + S + LOST_POLL),
        "still lost: polled, since the Up goes stale"
    );
    let stale = up + FRESH;
    assert_eq!(
        st.path_reach(stale, on_wall(stale)),
        Reach::Down { since: lost },
        "a stale Up says nothing, and the loss holds again"
    );

    let back = stale + 10 * S;
    assert!(st.path(true, back, on_wall(back)));
    let settled = back + PATH_SETTLE;
    assert_eq!(
        st.path_reach(back, on_wall(back)),
        Reach::Down { since: lost },
        "the return settles first"
    );
    assert_eq!(
        st.path_landing(back, on_wall(back)),
        Some(settled),
        "the settle's end"
    );
    assert_eq!(
        st.path_reach(settled, on_wall(settled)),
        Reach::Unknown,
        "settled: Unknown at once, no FRESH hold"
    );
    assert_eq!(st.path_landing(settled, on_wall(settled)), None);

    let again = settled + 30 * S;
    st.path(false, again, on_wall(again));
    assert_eq!(
        st.path_reach(again + S, on_wall(again + S)),
        Reach::Down { since: again },
        "a later loss runs from itself"
    );

    let mut first = NetState::default();
    assert!(!first.path(false, t, w));
    assert_eq!(
        first.path_reach(t + S, on_wall(t + S)),
        Reach::Down { since: t },
        "a first reading that cannot carry traffic"
    );
    let mut instant = NetState::with_cadence(
        PROBE_BUDGET,
        Cadence {
            settle: Duration::ZERO,
            ..Cadence::default()
        },
    );
    instant.path(false, t, w);
    instant.path(true, t + S, on_wall(t + S));
    assert_eq!(
        instant.path_reach(t + S, on_wall(t + S)),
        Reach::Unknown,
        "no settle"
    );
    assert_eq!(instant.path_landing(t + S, on_wall(t + S)), None);
}

/// A loss of the path holds a session the probe does not measure for at most [`LOST_CAP`]
/// on either clock: past it the path alone reads Unknown and nothing is looked at again
/// until the next reading, so a path reading that never turns back (a monitor update
/// missed across a sleep) holds no session for ever. A loss read after it runs again.
#[test]
fn a_loss_of_the_path_alone_holds_for_at_most_the_cap_on_either_clock() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    st.path(true, t, w);
    let lost = t + S;
    st.path(false, lost, on_wall(lost));
    let edge = lost + LOST_CAP - S;
    assert_eq!(
        st.path_reach(edge, on_wall(edge)),
        Reach::Down { since: lost },
        "inside the cap the loss holds"
    );
    assert_eq!(st.path_landing(edge, on_wall(edge)), Some(edge + LOST_POLL));
    let past = lost + LOST_CAP;
    assert_eq!(
        st.path_reach(past, on_wall(past)),
        Reach::Unknown,
        "past the cap on the monotonic clock: the blind ladder again"
    );
    assert_eq!(st.path_landing(past, on_wall(past)), None);
    assert_eq!(
        st.path_reach(lost + S, on_wall(lost) + LOST_CAP),
        Reach::Unknown,
        "past the cap on the wall clock alone (a sleep the monotonic clock did not count)"
    );
    assert_eq!(st.path_landing(lost + S, on_wall(lost) + LOST_CAP), None);

    st.path(true, past, on_wall(past));
    let again = past + 10 * S;
    st.path(false, again, on_wall(again));
    assert_eq!(
        st.path_reach(again + S, on_wall(again + S)),
        Reach::Down { since: again },
        "a loss read after the cap runs from itself"
    );
}

/// A measured session's wait looks again on [`LOST_POLL`] while the path is lost, not at the
/// ladder's next rung (up to 15 s): the path's return owes a probe a few seconds on, and the
/// look after the return lands on that probe's due time, so the Up it measures is met then,
/// not at the rung. The poll only caps: a probe due sooner keeps its time, and with no
/// lease nothing lands, lost or not.
#[test]
fn a_measured_wait_looks_again_on_the_lost_poll_and_lands_on_the_returns_probe() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    assert!(!st.path(true, t, w));
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    // An upstream outage, the path satisfied: three misses climb to the ladder's top rung.
    for at in [t + 60 * S, t + 65 * S, t + 75 * S] {
        st.ask("s-1", at, on_wall(at));
        measured(&mut st, Outcome::Down, at, on_wall(at));
    }
    let p = t + 75 * S;
    assert_eq!(
        st.landing(p, on_wall(p)),
        Some(p + 15 * S),
        "PRECONDITION: the ladder's top rung"
    );
    let lost = p + S / 2;
    st.path(false, lost, on_wall(lost));
    assert_eq!(
        st.landing(lost, on_wall(lost)),
        Some(lost + LOST_POLL),
        "a lost path: the wait looks again on the poll, not at the rung"
    );
    let look = lost + LOST_POLL;
    assert_eq!(
        st.landing(look, on_wall(look)),
        Some(look + LOST_POLL),
        "still lost: the poll again"
    );
    let back = p + 6 * S;
    assert!(
        st.path(true, back, on_wall(back)),
        "the return owes a probe"
    );
    assert_eq!(
        st.landing(back, on_wall(back)),
        Some(back + PATH_SETTLE),
        "the return: the owed probe's due time, not the rung"
    );
    let due = back + PATH_SETTLE;
    st.ask("s-1", due, on_wall(due));
    measured(&mut st, Outcome::Up, due, on_wall(due));
    assert_eq!(st.reach(due, on_wall(due)), Reach::Up { since: due });
    assert_eq!(
        st.landing(due, on_wall(due)),
        Some(due + STEADY_CADENCE),
        "measured up: the steady cadence"
    );

    let mut soon = NetState::default();
    assert!(!soon.path(true, t, w));
    assert!(!soon.path(false, t, w));
    soon.ask("s-1", t, w);
    measured(&mut soon, Outcome::Down, t, w);
    assert_eq!(
        soon.landing(t, w),
        Some(t + GAP_CADENCE),
        "a probe due before the poll keeps its time"
    );
    let mut idle = NetState::default();
    assert!(!idle.path(false, t, w));
    assert_eq!(
        idle.landing(t, w),
        None,
        "no lease: nothing lands, lost or not"
    );
}

/// A probe stuck from before the path's return (a resolve has no timeout of its own) holds
/// no measured session at Down past its budget: once it overruns, the returned path's
/// standing loss yields to Unknown, as any overrun's verdict does, and the probe the return
/// owed runs as soon as the stuck one ends. A probe begun after the return that overruns
/// reads Unknown too, not Down until [`FRESH`]; a path still lost keeps its Down
/// (`a_lost_path_reads_down_until_a_probe_measures_its_return`).
#[test]
fn a_probe_stuck_from_before_a_paths_return_reads_unknown_past_its_budget() {
    let (t, w) = t0();
    let on_wall = |at: Instant| w + at.duration_since(t);
    let mut st = NetState::default();
    assert!(!st.path(true, t, w));
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    let began = t + 60 * S;
    st.ask("s-1", began, on_wall(began));
    assert!(st.begin(began));
    let lost = began + S;
    st.path(false, lost, on_wall(lost));
    let back = began + 4 * S;
    assert!(st.path(true, back, on_wall(back)));
    let inside = began + PROBE_BUDGET - S;
    assert_eq!(
        st.reach(inside, on_wall(inside)),
        Reach::Down { since: lost },
        "within its budget the return is unmeasured: Down"
    );
    assert_eq!(
        st.next_probe(inside, on_wall(inside)),
        None,
        "PRECONDITION: one in flight, so the return's probe waits for it"
    );
    let over = began + PROBE_BUDGET + S;
    assert_eq!(
        st.reach(over, on_wall(over)),
        Reach::Unknown,
        "past its budget: Unknown, not Down until FRESH"
    );
    let late = back + FRESH - S;
    st.ask("s-1", late, on_wall(late));
    assert_eq!(
        st.reach(late, on_wall(late)),
        Reach::Unknown,
        "still stuck, just short of FRESH after the return: Unknown"
    );
    let ended = late + S;
    st.finish(Outcome::Down, ended, on_wall(ended));
    assert_eq!(
        st.reach(ended, on_wall(ended)),
        Reach::Unknown,
        "its overrun measured Unknown"
    );
    assert_eq!(
        st.next_probe(ended, on_wall(ended)),
        Some(back + PATH_SETTLE),
        "the return's owed probe is due at once"
    );

    let mut after = NetState::default();
    assert!(!after.path(true, t, w));
    after.ask("s-1", t, w);
    measured(&mut after, Outcome::Up, t, w);
    after.path(false, t + 10 * S, on_wall(t + 10 * S));
    let back = t + 12 * S;
    assert!(after.path(true, back, on_wall(back)));
    let owed = back + PATH_SETTLE;
    after.ask("s-1", owed, on_wall(owed));
    assert_eq!(after.next_probe(owed, on_wall(owed)), Some(owed));
    assert!(after.begin(owed));
    let over = owed + PROBE_BUDGET + S;
    assert_eq!(
        after.reach(over, on_wall(over)),
        Reach::Unknown,
        "the return's own probe overran: Unknown"
    );
}

/// One probe that began at `began` and took `took`, its finish on the wall clock `w`.
fn probed(
    st: &mut NetState,
    outcome: Outcome,
    began: Instant,
    took: Duration,
    w: SystemTime,
) -> Option<Edge> {
    assert!(st.begin(began));
    st.finish(outcome, began + took, w)
}

/// `finish` says the verdict's edge iff the fresh outcome changed: the first measure from
/// nothing, a change with how long the old outcome held (from its run's first measure) and
/// how long the probe took, and nothing for a repeat; an overrun changes it to Unknown, and
/// the first measure after a sleep's gap is an edge from nothing even when it repeats.
#[test]
fn finish_says_the_edge_iff_the_fresh_outcome_changed() {
    let (t, w) = t0();
    let ms = Duration::from_millis(1);
    let mut st = NetState::default();
    st.ask("s-1", t, w);
    let m0 = t + 40 * ms;
    assert_eq!(
        probed(&mut st, Outcome::Down, t, 40 * ms, w + 40 * ms),
        Some(Edge {
            from: None,
            to: Outcome::Down,
            held: Duration::ZERO,
            took: 40 * ms,
        }),
        "the first measure"
    );
    let m1 = m0 + 3 * S;
    assert_eq!(
        probed(&mut st, Outcome::Down, m1 - 40 * ms, 40 * ms, w + 3 * S),
        None,
        "Down then Down"
    );
    let m2 = m1 + 15 * S;
    assert_eq!(
        probed(&mut st, Outcome::Up, m2 - 250 * ms, 250 * ms, w + 18 * S),
        Some(Edge {
            from: Some(Outcome::Down),
            to: Outcome::Up,
            held: m2 - m0,
            took: 250 * ms,
        }),
        "Down to Up, held since the run's first Down"
    );
    let m3 = m2 + 60 * S;
    assert_eq!(
        probed(&mut st, Outcome::Up, m3, 30 * ms, w + 78 * S),
        None,
        "Up then Up"
    );
    let over = PROBE_BUDGET + S;
    let m4 = m3 + 60 * S;
    assert_eq!(
        probed(&mut st, Outcome::Up, m4, over, w + 149 * S),
        Some(Edge {
            from: Some(Outcome::Up),
            to: Outcome::Unknown,
            held: m4 + over - m2,
            took: over,
        }),
        "an overrun reads Unknown"
    );
    let woke = m4 + over + S;
    assert_eq!(
        probed(&mut st, Outcome::Unknown, woke, 20 * ms, w + 3749 * S),
        Some(Edge {
            from: None,
            to: Outcome::Unknown,
            held: Duration::ZERO,
            took: 20 * ms,
        }),
        "a sleep's gap: the repeat is the first fresh measure"
    );
    assert_eq!(
        st.finish(Outcome::Down, woke + S, w + 3750 * S),
        Some(Edge {
            from: Some(Outcome::Unknown),
            to: Outcome::Down,
            held: S - 20 * ms,
            took: Duration::ZERO,
        }),
        "a finish with no probe in flight took nothing"
    );
}

/// A measure older than [`FRESH`] on either clock says nothing (the
/// monotonic clock stops while the Mac sleeps, the wall clock does not),
/// nor does one whose wall clock stepped far back.
#[test]
fn a_stale_measure_reads_unknown_on_either_clock() {
    let (t, w) = t0();
    let mut st = NetState::default();
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Down, t, w);
    assert_eq!(st.reach(t + 60 * S, w + 60 * S), Reach::Down { since: t });
    assert_eq!(st.reach(t + 121 * S, w + 60 * S), Reach::Unknown);
    assert_eq!(
        st.reach(t + 10 * S, w + 3600 * S),
        Reach::Unknown,
        "a sleep: the wall clock moved an hour, the monotonic 10 s"
    );
    assert_eq!(st.reach(t + 10 * S, w - 600 * S), Reach::Unknown);
    assert_eq!(
        st.reach(t + 10 * S, w - 2 * S),
        Reach::Down { since: t },
        "a time sync's step back is no sleep"
    );
    assert_eq!(st.next_probe(t + 10 * S, w + 3600 * S), Some(t + 10 * S));
}

/// An Up measured again keeps its `since`, so the loop sees no edge each
/// minute; a probe past its budget reads Unknown, in flight and after.
#[test]
fn since_is_the_runs_start_and_an_overrun_is_unknown() {
    let (t, w) = t0();
    let mut st = NetState::default();
    st.ask("s-1", t, w);
    measured(&mut st, Outcome::Up, t, w);
    measured(&mut st, Outcome::Up, t + 60 * S, w + 60 * S);
    assert_eq!(st.reach(t + 61 * S, w + 61 * S), Reach::Up { since: t });
    measured(&mut st, Outcome::Down, t + 120 * S, w + 120 * S);
    measured(&mut st, Outcome::Up, t + 135 * S, w + 135 * S);
    assert_eq!(
        st.reach(t + 136 * S, w + 136 * S),
        Reach::Up { since: t + 135 * S }
    );
    let began = t + 200 * S;
    assert!(st.begin(began));
    assert_eq!(
        st.reach(began + 5 * S, w + 205 * S),
        Reach::Up { since: t + 135 * S }
    );
    assert_eq!(st.reach(began + 11 * S, w + 211 * S), Reach::Unknown);
    st.finish(Outcome::Down, began + 30 * S, w + 230 * S);
    assert_eq!(st.reach(began + 30 * S, w + 230 * S), Reach::Unknown);
}

fn facts(env: &[&str], settings: &[(&str, Source)]) -> RouteFacts {
    RouteFacts {
        env: Some(env.iter().map(|s| (*s).to_string()).collect()),
        settings: settings
            .iter()
            .map(|(at, s)| ((*at).to_string(), s.clone()))
            .collect(),
    }
}

/// Every route key, in the environment or a settings `env` block, and every
/// unreadable or unparseable source makes the route custom.
#[test]
fn the_route_is_the_default_only_without_a_base_url_provider_socket_or_proxy() {
    let quiet = [("user", Source::Text("{\"model\":\"opus\"}".to_string()))];
    assert_eq!(
        route_of(&facts(&["HOME=/u", "NO_PROXY=x"], &quiet)),
        Route::Default
    );
    for key in ROUTE_KEYS
        .iter()
        .chain(&["CLAUDE_CODE_PROXY_RESOLVES_HOSTS"])
    {
        let env = format!("{key}=http://gw.example:8080");
        assert!(
            matches!(route_of(&facts(&[&env], &quiet)), Route::Custom(_)),
            "{key} in the environment"
        );
        let block = Source::Text(format!("{{\"env\":{{\"{key}\":\"1\"}}}}"));
        assert!(
            matches!(route_of(&facts(&[], &[("managed", block)])), Route::Custom(w) if w.contains("managed")),
            "{key} in a settings file"
        );
    }
    assert_eq!(
        route_of(&facts(&["HTTPS_PROXY="], &quiet)),
        Route::Default,
        "an empty value names nothing"
    );
    assert!(matches!(
        route_of(&facts(&[], &[("local", Source::Unreadable)])),
        Route::Custom(_)
    ));
    assert!(matches!(
        route_of(&facts(&[], &[("local", Source::Text("{env".to_string()))])),
        Route::Custom(_)
    ));
    assert!(matches!(
        route_of(&facts(
            &[],
            &[("local", Source::Text("{\"env\":[1]}".to_string()))]
        )),
        Route::Custom(_)
    ));
    assert_eq!(
        route_of(&facts(&[], &[("absent", Source::Missing)])),
        Route::Default
    );
    assert!(matches!(route_of(&RouteFacts::default()), Route::Custom(_)));
}

/// A launch's route variables are one list across its environment and settings, the rest of
/// either left out; two values for one of them, or a source not read, name no route.
#[test]
fn a_launchs_route_variables_are_one_list_or_the_reason_it_names_none() {
    let lab = Source::Text(
        "{\"env\":{\"ANTHROPIC_BASE_URL\":\"http://127.0.0.1:4100\",\"HTTP_PROXY\":\"http://127.0.0.1:9\",\
         \"NO_PROXY\":\"127.0.0.1\",\"DISABLE_AUTOUPDATER\":\"1\",\"PATH\":\"/bin\"}}"
            .to_string(),
    );
    assert_eq!(
        launch_env(&facts(
            &[
                "PATH=/usr/bin",
                "HTTP_PROXY=http://127.0.0.1:9",
                "HTTPS_PROXY="
            ],
            &[("user", lab.clone()), ("absent", Source::Missing)]
        )),
        Ok(vec![
            "HTTP_PROXY=http://127.0.0.1:9".to_string(),
            "ANTHROPIC_BASE_URL=http://127.0.0.1:4100".to_string(),
            "NO_PROXY=127.0.0.1".to_string(),
        ]),
        "PATH differs and is no route variable; a blank one names nothing"
    );
    let other =
        Source::Text("{\"env\":{\"ANTHROPIC_BASE_URL\":\"http://127.0.0.1:4200\"}}".to_string());
    let two = launch_env(&facts(&[], &[("user", lab.clone()), ("project", other)]));
    assert_eq!(
        two,
        Err(Unnamed::Conflict(
            "ANTHROPIC_BASE_URL is set differently in user and project".to_string()
        )),
        "two values for one variable are settled for the process"
    );
    for (settings, says) in [
        (Source::Unreadable, "local could not be read"),
        (
            Source::Text("{env".to_string()),
            "local could not be parsed",
        ),
    ] {
        let why = launch_env(&facts(&[], &[("user", lab.clone()), ("local", settings)]));
        assert_eq!(
            why,
            Err(Unnamed::Unread(says.to_string())),
            "a source not read now is read again"
        );
    }
    assert!(
        launch_env(&RouteFacts::default()).is_err_and(|why| !why.settled()),
        "an environment not read is read again"
    );
}

/// The lab's route seam admits exactly the loopback base URL it names, for
/// `ANTHROPIC_BASE_URL` alone; unread (`None`, every shipped binary), it admits
/// nothing, so the stub's URL still names a route there.
#[test]
fn the_lab_route_seam_admits_only_its_own_loopback_url_and_nothing_unread() {
    let stub = "http://127.0.0.1:57232";
    assert!(
        !lab_default_route(None, "ANTHROPIC_BASE_URL", stub),
        "a build that reads no seam admits nothing"
    );
    for seam in [stub, "http://localhost:9", "http://[::1]:65535"] {
        assert!(
            lab_default_route(Some(seam), "ANTHROPIC_BASE_URL", seam),
            "{seam}"
        );
        assert!(
            lab_default_route(Some(seam), "ANTHROPIC_BASE_URL", &format!(" {seam}\n")),
            "{seam}, the value trimmed"
        );
    }
    for seam in [
        "http://gw.example:57232",
        "http://10.0.0.1:57232",
        "http://127.0.0.1",
        "http://127.0.0.1:",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:57232/",
        "http://127.0.0.1:57232/v1",
        "https://127.0.0.1:57232",
        "http://127.0.0.1.gw.example:57232",
        "http://localhost.gw.example:57232",
        "",
    ] {
        assert!(
            !lab_default_route(Some(seam), "ANTHROPIC_BASE_URL", seam),
            "a seam that is no loopback base URL: {seam:?}"
        );
    }
    assert!(!lab_default_route(
        Some(stub),
        "ANTHROPIC_BASE_URL",
        "http://127.0.0.1:57233"
    ));
    assert!(!lab_default_route(Some(stub), "ANTHROPIC_BASE_URL", ""));
    for key in ROUTE_KEYS.iter().filter(|k| **k != "ANTHROPIC_BASE_URL") {
        assert!(!lab_default_route(Some(stub), key, stub), "{key}");
    }
    assert!(!lab_default_route(Some(stub), "anthropic_base_url", stub));
    assert!(
        lab_route_seam().is_none(),
        "the tests run with no {LAB_ROUTE_SEAM}"
    );
    assert!(
        names_a_route("ANTHROPIC_BASE_URL", stub),
        "with the seam unread the stub's URL names a route"
    );
    assert!(matches!(
        route_of(&facts(
            &[],
            &[(
                "user",
                Source::Text(format!("{{\"env\":{{\"ANTHROPIC_BASE_URL\":\"{stub}\"}}}}"))
            )]
        )),
        Route::Custom(_)
    ));
}

/// The settings reader: the `env` block's values as text and whether an
/// `apiKeyHelper` is named, with the route read through it.
#[test]
fn one_reader_takes_a_settings_files_env_block_and_its_api_key_helper() {
    let text = r#"{"model":"opus","apiKeyHelper":"/usr/local/bin/key",
            "env":{"B":7,"A":"x","C":null,"D":true,"HTTPS_PROXY":"http://gw.example:8080"}}"#;
    let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
    assert_eq!(
        settings_of(text),
        Some(Settings {
            env: vec![
                pair("A", "x"),
                pair("B", "7"),
                pair("C", ""),
                pair("D", "true"),
                pair("HTTPS_PROXY", "http://gw.example:8080"),
            ],
            api_key_helper: true,
            ..Settings::default()
        })
    );
    assert_eq!(
        settings_route(text),
        SettingsEnv::Names("HTTPS_PROXY".to_string())
    );
    assert_eq!(settings_of("{}"), Some(Settings::default()));
    assert_eq!(settings_route("{}"), SettingsEnv::Quiet);
    for empty in ["", " \n\t", "\u{feff}", "\u{feff} \n"] {
        assert_eq!(settings_of(empty), Some(Settings::default()), "{empty:?}");
    }
    assert_eq!(
        settings_route("\u{feff}{\"env\":{\"HTTPS_PROXY\":\"http://gw.example:8080\"}}"),
        SettingsEnv::Names("HTTPS_PROXY".to_string())
    );
    for helper in ["null", "false", "\"\""] {
        let quiet = format!("{{\"apiKeyHelper\":{helper}}}");
        assert_eq!(settings_of(&quiet), Some(Settings::default()), "{helper}");
    }
    for helper in ["\"key.sh\"", "true", "[\"key.sh\"]", "{}", "1"] {
        let named = format!("{{\"apiKeyHelper\":{helper}}}");
        assert!(
            settings_of(&named).is_some_and(|s| s.api_key_helper),
            "{helper}"
        );
    }
    assert_eq!(
        settings_of(r#"{"env":{"apiKeyHelper":"key.sh"}}"#),
        Some(Settings {
            env: vec![pair("apiKeyHelper", "key.sh")],
            api_key_helper: false,
            ..Settings::default()
        })
    );
    for unparsed in [
        "{env",
        "[1]",
        "\"text\"",
        "{\"env\":[1]}",
        "{\"env\":\"x\"}",
    ] {
        assert_eq!(settings_of(unparsed), None, "{unparsed}");
        assert_eq!(
            settings_route(unparsed),
            SettingsEnv::Unparsed,
            "{unparsed}"
        );
    }
}

/// The same reader says whether a document sets a `statusLine` (any value
/// but `null`) and `disableAllHooks` to `true`.
#[test]
fn one_reader_says_whether_a_document_sets_a_status_line_or_turns_hooks_off() {
    let read = |text: &str| settings_of(text).expect("a settings document");
    for set in [
        r#"{"statusLine":{"type":"command","command":"echo mine"}}"#,
        r#"{"statusLine":"x"}"#,
        r#"{"statusLine":{}}"#,
    ] {
        let s = read(set);
        assert!(s.status_line && !s.hooks_off, "{set}");
    }
    assert!(read(r#"{"disableAllHooks":true}"#).hooks_off);
    for quiet in [
        "{}",
        r#"{"statusLine":null}"#,
        r#"{"disableAllHooks":false}"#,
        r#"{"disableAllHooks":"true"}"#,
        r#"{"env":{"statusLine":"x","disableAllHooks":"true"}}"#,
    ] {
        let s = read(quiet);
        assert!(!s.status_line && !s.hooks_off, "{quiet}");
    }
}
