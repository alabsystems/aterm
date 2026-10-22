// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The attached clock: a live call whose time went back (two host clocks interleaved) or
//! predates the epoch is recorded at the task's logical time `max(floor, t)` and counted,
//! never dropped or refused, and the attached run then equals the unattached engine fed the
//! logical times. Keys interleaved while paused queue in logical order and all reach the
//! glow after a resume or a commit. A decoded or replayed log whose own times go back is
//! still refused as `TimeWentBack`.

use std::time::Duration;

use aterm_aos::effects::NoEffects;
use aterm_aos::log::{EventLog, Tail};
use aterm_aos::replay::replay;
use aterm_aos::task::state_digest;
use aterm_time::Instant;

use super::test_fixtures::{
    BUILD, Call, LANE, Rng, TAU, attached, burst, cfg, draw, geom, hosted, ms, play, random_stream,
    replay_log,
};
use super::{
    MomentumCode, MomentumEvent, MomentumHost, MomentumObservation, MomentumSchema,
    PrebuiltMomentum, time_of,
};
use crate::cursor_momentum::{AdmitRefusal, MomentumGlow};

/// A call's time, if it has one.
fn call_time(c: &Call) -> Option<Duration> {
    match *c {
        Call::Key { at, .. } | Call::Tick { at, .. } | Call::Query { at, .. } => Some(at),
        Call::Reset => None,
    }
}

/// `c` stamped at `at`.
fn restamped(c: Call, at: Duration) -> Call {
    match c {
        Call::Key { tau, .. } => Call::Key { at, tau },
        Call::Tick {
            cur,
            geom,
            cfg,
            prefix,
            ..
        } => Call::Tick {
            cur,
            at,
            geom,
            cfg,
            prefix,
        },
        Call::Query { lane, .. } => Call::Query { at, lane },
        Call::Reset => Call::Reset,
    }
}

/// The stream as a host reading two clocks stamps it: about a third of the calls up to
/// 6 ms early.
fn skewed(calls: &[Call], r: &mut Rng) -> Vec<Call> {
    calls
        .iter()
        .map(|&c| match call_time(&c) {
            Some(at) if r.below(3) == 0 => restamped(c, at.saturating_sub(ms(r.below(7)))),
            _ => c,
        })
        .collect()
}

/// Each call at the running maximum of the times before it, and how many calls that moved.
fn logical(calls: &[Call]) -> (Vec<Call>, u64) {
    let (mut floor, mut moved) = (Duration::ZERO, 0u64);
    let out = calls
        .iter()
        .map(|&c| match call_time(&c) {
            Some(at) if at < floor => {
                moved += 1;
                restamped(c, floor)
            }
            Some(at) => {
                floor = at;
                c
            }
            None => c,
        })
        .collect();
    (out, moved)
}

fn nanos(d: Duration) -> i128 {
    i128::try_from(d.as_nanos()).expect("a stream time fits i128 ns")
}

/// Random in-profile streams, a third of their calls stamped up to 6 ms early, played into
/// an attached host: nothing is refused, the host sees exactly what an unattached engine
/// fed the logical times sees, every moved call is counted, the log holds the logical
/// times, and its replay lands on the live state and chain. Negative control: the skew
/// moved some calls in every stream.
#[test]
fn interleaved_host_clocks_are_recorded_at_the_logical_time_and_never_refused() {
    let t0 = Instant::now();
    for seed in 1..=6u64 {
        let raw = skewed(
            &random_stream(seed, 300, true),
            &mut Rng::new(seed ^ 0xC10C),
        );
        let (logical, moved) = logical(&raw);
        assert!(moved > 0, "seed {seed}: fixture: some call went back");
        let mut direct = MomentumGlow::default();
        let want = play(&mut direct, t0, &logical);
        let mut host = attached(t0, MomentumCode::Shipped);
        let got = play(&mut host, t0, &raw);
        assert_eq!(got, want, "seed {seed}");
        assert_eq!(host.aos_status().clamped, moved, "seed {seed}");
        let h = hosted(&host);
        let recorded: Vec<i128> = h
            .boundary
            .log()
            .expect("attached")
            .inputs()
            .iter()
            .filter_map(time_of)
            .collect();
        let want_times: Vec<i128> = logical.iter().filter_map(call_time).map(nanos).collect();
        assert_eq!(
            recorded, want_times,
            "seed {seed}: the log holds logical times"
        );
        let run = replay_log(&host, MomentumCode::Shipped);
        assert!(
            run.observations
                .iter()
                .all(|(_, o)| !matches!(o, MomentumObservation::Refused(_))),
            "seed {seed}: a replayed input was refused"
        );
        assert_eq!(
            run.state_digest,
            state_digest(h.boundary.schema(), h.state())
        );
        assert_eq!(run.observation_chain, h.chain.chain);
    }
}

/// The first calls after attach can predate the epoch (the host read the event's time
/// before it read the epoch): each is recorded at the epoch, counted, and applied there, as
/// a direct engine fed the epoch shows. A later call on time moves nothing.
#[test]
fn a_call_before_the_epoch_is_recorded_at_the_epoch_and_counted() {
    let t0 = Instant::now();
    let c = cfg(true);
    let mut host = attached(t0, MomentumCode::Shipped);
    let mut direct = MomentumGlow::default();
    host.on_key(t0 - ms(3), TAU);
    direct.on_key(t0, TAU);
    assert_eq!(
        draw(&mut host, Some((4, 10)), t0 - ms(1), geom(), &c, 1),
        draw(&mut direct, Some((4, 10)), t0, geom(), &c, 1)
    );
    assert_eq!(
        host.next_change_deadline(t0 - ms(2), LANE),
        direct.next_change_deadline(t0, LANE)
    );
    assert_eq!(host.aos_status().clamped, 3);
    host.on_key(t0 + ms(5), TAU);
    assert_eq!(host.aos_status().clamped, 3, "a call on time moves nothing");
    let times: Vec<i128> = hosted(&host)
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .iter()
        .filter_map(time_of)
        .collect();
    assert_eq!(times, [0, 0, 0, nanos(ms(5))]);
    let line = host.aos_status().to_string();
    assert!(line.ends_with(" clamped=3"), "{line}");
}

/// Keys interleaved while paused queue at the logical time (the latest queued key's time is
/// the floor), so after a resume, and after a commit, every one reaches the glow (none is
/// refused as a time that went back) and the log holds them in order. The two that went
/// back are counted.
#[test]
fn keys_interleaved_while_paused_are_all_delivered_after_a_resume_and_after_a_commit() {
    let t0 = Instant::now();
    let c = cfg(true);
    for commit in [false, true] {
        let mut host = attached(t0, MomentumCode::Shipped);
        let (last, _) = burst(&mut host, t0, 6, &c, &mut Vec::new());
        host.aos_pause(last, LANE).expect("pauses");
        let stamped = [5u64, 2, 7, 1];
        for k in stamped {
            host.on_key(last + ms(k), TAU);
        }
        assert_eq!(hosted(&host).boundary.queued(), 4);
        assert_eq!(host.aos_status().clamped, 2, "commit={commit}");
        let delivered = if commit {
            host.aos_repair(MomentumCode::Shipped)
                .expect("commits")
                .delivered
        } else {
            host.aos_resume()
        };
        assert_eq!(
            delivered,
            vec![Ok(MomentumObservation::Keyed); 4],
            "commit={commit}"
        );
        let base = nanos(last - t0);
        let inputs = hosted(&host).boundary.log().expect("attached").inputs();
        let queued: Vec<i128> = inputs[inputs.len() - 4..]
            .iter()
            .filter_map(time_of)
            .collect();
        assert_eq!(
            queued,
            [5, 5, 7, 7].map(|k| base + nanos(ms(k))),
            "commit={commit}"
        );
        assert_eq!(hosted(&host).state().clock(), Some(base + nanos(ms(7))));
    }
}

/// A log whose own times go back, encoded and decoded, replays with each late input
/// refused as `TimeWentBack` against the latest time before it. Negative control: the
/// same times sent live are recorded at the logical time, and that log replays with no
/// refusal.
#[test]
fn a_decoded_log_whose_times_go_back_replays_as_time_went_back() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    let key = |at: i128| MomentumEvent::Key {
        at,
        tau_bits: TAU.to_bits(),
    };
    let (five, two, one, six) = (nanos(ms(5)), nanos(ms(2)), nanos(ms(1)), nanos(ms(6)));
    let (schema, identity, header, start) = {
        let b = &hosted(&host).boundary;
        let start = b
            .checkpoint()
            .expect("attached")
            .restore(b.schema(), b.identity())
            .expect("the ancestor restores");
        (
            b.schema().clone(),
            b.identity().clone(),
            b.log().expect("attached").header().clone(),
            start,
        )
    };
    let mut crafted = EventLog::<MomentumSchema>::new(header, 16);
    for ev in [
        key(five),
        key(two),
        MomentumEvent::Query {
            at: one,
            frame_interval: LANE,
        },
        key(six),
    ] {
        crafted.push(ev).expect("room");
    }
    let bytes = crafted.encode(&schema).expect("encodes");
    let (decoded, tail) =
        EventLog::decode(&schema, &bytes, &identity, &crafted.header().ancestor, 16)
            .expect("decodes");
    assert_eq!(tail, Tail::Clean);
    assert_eq!(decoded.inputs(), crafted.inputs());
    let run = replay(
        &schema,
        &PrebuiltMomentum::new(MomentumCode::Shipped, BUILD),
        start,
        decoded.inputs(),
        decoded.header().first_cursor,
        &mut NoEffects,
    );
    let observed: Vec<MomentumObservation> = run.observations.into_iter().map(|(_, o)| o).collect();
    assert_eq!(
        observed,
        [
            MomentumObservation::Keyed,
            MomentumObservation::Refused(AdmitRefusal::TimeWentBack {
                last: five,
                at: two
            }),
            MomentumObservation::Refused(AdmitRefusal::TimeWentBack {
                last: five,
                at: one
            }),
            MomentumObservation::Keyed,
        ]
    );

    host.on_key(t0 + ms(5), TAU);
    host.on_key(t0 + ms(2), TAU);
    let _ = host.next_change_deadline(t0 + ms(1), LANE);
    host.on_key(t0 + ms(6), TAU);
    let live: Vec<i128> = hosted(&host)
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .iter()
        .filter_map(time_of)
        .collect();
    assert_eq!(live, [five, five, five, six]);
    let run = replay_log(&host, MomentumCode::Shipped);
    assert!(
        run.observations
            .iter()
            .all(|(_, o)| !matches!(o, MomentumObservation::Refused(_)))
    );
    assert_eq!(host.aos_status().clamped, 2);
}

/// Unattached, nothing is clamped: the direct engine takes every time as given.
#[test]
fn an_unattached_host_reports_no_clamps() {
    let t0 = Instant::now();
    let mut host = MomentumHost::default();
    host.on_key(t0 + ms(5), TAU);
    host.on_key(t0, TAU);
    assert_eq!(host.aos_status().clamped, 0);
}
