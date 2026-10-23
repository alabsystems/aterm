// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Contract tests for `momentum-glow.no-stranded-light` and mutant negative controls.

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::replay::Run;
use aterm_time::Instant;

use super::test_fixtures::{
    LANE, attached, burst, cfg, hosted, play, random_stream, replay_log, run_lane,
};
use super::{
    COLD_TICK_CLEARS, HOT_HAS_WAKE, MomentumCode, MomentumEvent, MomentumObservation,
    MomentumSchema, NoStrandedLight, RUN_MATCHES_INPUTS, SETTLE_MARGIN, WAKE_NO_LATER_THAN_CLEAR,
};
use crate::cursor_momentum::AdmitRefusal;

fn judge(host: &crate::aos_momentum::MomentumHost, run: &Run<MomentumSchema>) -> Verdict {
    let b = &hosted(host).boundary;
    let inputs = b.log().expect("attached").inputs();
    NoStrandedLight::new().judge(b.schema(), inputs, run)
}

/// Attached host under the shipped code that typed a burst and ran its lane to the clearing frame.
fn full_cool_down(t0: Instant, dark: bool) -> crate::aos_momentum::MomentumHost {
    let mut host = attached(t0, MomentumCode::Shipped);
    let c = cfg(dark);
    let mut seen = Vec::new();
    let (last, cell) = burst(&mut host, t0, 24, &c, &mut seen);
    let parked = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
    assert!(!parked.stranded, "fixture: the shipped lane parks cold");
    host
}

#[test]
fn on_one_trace_the_demonstration_fault_strands_light_and_the_shipped_code_does_not() {
    let t0 = Instant::now();
    for dark in [true, false] {
        let host = full_cool_down(t0, dark);
        let shipped = replay_log(&host, MomentumCode::Shipped);
        let demo = replay_log(&host, MomentumCode::DemoStrandedSettle);
        assert_eq!(judge(&host, &shipped), Verdict::Pass, "dark={dark}");
        let first_settling = shipped
            .observations
            .iter()
            .find(|(_, o)| matches!(o, MomentumObservation::Schedule { settling: true, .. }))
            .map(|(c, _)| *c)
            .expect("fixture: the trace queries a settling glow");
        match judge(&host, &demo) {
            Verdict::Fail { cursor, law, .. } => {
                assert_eq!((cursor, law), (first_settling, HOT_HAS_WAKE), "dark={dark}");
            }
            Verdict::Pass => panic!("dark={dark}: the demonstration fault passed"),
        }
        let schedules = shipped
            .observations
            .iter()
            .filter(|(_, o)| matches!(o, MomentumObservation::Schedule { settling: true, .. }))
            .count();
        assert!(schedules >= 3, "dark={dark}: {schedules} settle wakes");
    }
}

#[test]
fn the_shipped_code_passes_the_contract_on_every_random_stream() {
    let t0 = Instant::now();
    for seed in 1..=128u64 {
        let mut host = attached(t0, MomentumCode::Shipped);
        let _ = play(&mut host, t0, &random_stream(seed, 400, true));
        let run = replay_log(&host, MomentumCode::Shipped);
        assert_eq!(judge(&host, &run), Verdict::Pass, "seed {seed}");
    }
}

/// The settling queries of a passing run, by observation index.
fn settling_queries(run: &Run<MomentumSchema>) -> Vec<usize> {
    run.observations
        .iter()
        .enumerate()
        .filter(|(_, (_, o))| matches!(o, MomentumObservation::Schedule { settling: true, .. }))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn a_settle_wake_past_the_clear_or_inside_a_frame_breaks_wake_no_later_than_clear() {
    let t0 = Instant::now();
    let host = full_cool_down(t0, true);
    let run = replay_log(&host, MomentumCode::Shipped);
    assert_eq!(judge(&host, &run), Verdict::Pass);
    let inputs = hosted(&host)
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .to_vec();
    let i = settling_queries(&run)[0];
    let MomentumEvent::Query { at, frame_interval } = inputs[i] else {
        panic!("fixture: a query");
    };
    let floor = at + frame_interval.as_nanos() as i128;
    for late in [true, false] {
        let mut bent = run.clone();
        let (cursor, obs) = &mut bent.observations[i];
        let MomentumObservation::Schedule {
            cold_at, deadline, ..
        } = obs
        else {
            panic!("fixture: a schedule");
        };
        let limit = cold_at.map_or(floor, |c| (c + SETTLE_MARGIN.as_nanos() as i128).max(floor));
        *deadline = Some(if late { limit + 1 } else { floor - 1 });
        let cursor = *cursor;
        match judge(&host, &bent) {
            Verdict::Fail { cursor: c, law, .. } => {
                assert_eq!((c, law), (cursor, WAKE_NO_LATER_THAN_CLEAR), "late={late}");
            }
            Verdict::Pass => panic!("late={late}: a bent wake passed"),
        }
    }
}

#[test]
fn a_tick_past_the_cold_forecast_that_still_draws_breaks_cold_tick_clears() {
    let t0 = Instant::now();
    let host = full_cool_down(t0, true);
    let run = replay_log(&host, MomentumCode::Shipped);
    let inputs = hosted(&host)
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .to_vec();
    let last = run.observations.len() - 1;
    let (cursor, clearing) = run.observations[last].clone();
    assert!(
        matches!(&clearing, MomentumObservation::Frame { hot: false, fp: 0, out, under, .. } if out.is_empty() && under.is_empty()),
        "fixture: the lane ends on the clearing frame"
    );
    let lit = |run: &Run<MomentumSchema>| {
        let mut bent = run.clone();
        if let MomentumObservation::Frame { hot, fp, .. } = &mut bent.observations[last].1 {
            *hot = true;
            *fp = 0x51;
        }
        bent
    };
    match judge(&host, &lit(&run)) {
        Verdict::Fail { cursor: c, law, .. } => assert_eq!((c, law), (cursor, COLD_TICK_CLEARS)),
        Verdict::Pass => panic!("a lit tick past the cold forecast passed"),
    }
    let contract = NoStrandedLight::new();
    let schema = hosted(&host).boundary.schema();
    let mut keyed_inputs = inputs.clone();
    let mut keyed = lit(&run);
    keyed_inputs[last - 1] = MomentumEvent::Key {
        at: 0,
        tau_bits: crate::cursor_momentum::MOMENTUM_GLOW_TAU_S.to_bits(),
    };
    keyed.observations[last - 1].1 = MomentumObservation::Keyed;
    assert_eq!(contract.judge(schema, &keyed_inputs, &keyed), Verdict::Pass);
    let mut dimmer = inputs.clone();
    if let MomentumEvent::Tick { cfg, .. } = &mut dimmer[last] {
        cfg.intensity = 0.5f32.to_bits();
    }
    assert_eq!(contract.judge(schema, &dimmer, &lit(&run)), Verdict::Pass);
    assert!(
        matches!(inputs[last - 1], MomentumEvent::Query { .. }),
        "fixture: query, tick"
    );
    let mut stale_inputs = inputs.clone();
    stale_inputs.insert(
        last - 1,
        MomentumEvent::Key {
            at: 0,
            tau_bits: crate::cursor_momentum::MOMENTUM_GLOW_TAU_S.to_bits(),
        },
    );
    let mut stale = lit(&run);
    stale
        .observations
        .insert(last - 1, (cursor - 1, MomentumObservation::Keyed));
    for (c, _) in &mut stale.observations[last..] {
        *c += 1;
    }
    assert_eq!(contract.judge(schema, &stale_inputs, &stale), Verdict::Pass);
}

#[test]
fn an_observation_that_is_not_its_inputs_breaks_run_matches_inputs() {
    let t0 = Instant::now();
    let host = full_cool_down(t0, true);
    let run = replay_log(&host, MomentumCode::Shipped);
    let mut swapped = run.clone();
    swapped.observations[0].1 = MomentumObservation::WasReset;
    match judge(&host, &swapped) {
        Verdict::Fail { cursor, law, .. } => {
            assert_eq!((cursor, law), (run.observations[0].0, RUN_MATCHES_INPUTS));
        }
        Verdict::Pass => panic!("a swapped observation passed"),
    }
    let mut short = run.clone();
    let _ = short.observations.pop();
    assert!(matches!(
        judge(&host, &short),
        Verdict::Fail {
            law: RUN_MATCHES_INPUTS,
            ..
        }
    ));
    let mut long = run.clone();
    let extra = long.observations.last().cloned().expect("a run");
    long.observations.push((extra.0 + 1, extra.1));
    assert!(matches!(
        judge(&host, &long),
        Verdict::Fail {
            law: RUN_MATCHES_INPUTS,
            ..
        }
    ));
}

#[test]
fn refused_inputs_are_judged_by_no_law() {
    let t0 = Instant::now();
    let host = full_cool_down(t0, true);
    let run = replay_log(&host, MomentumCode::Shipped);
    let inputs = hosted(&host)
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .to_vec();
    let schema = hosted(&host).boundary.schema();
    let last = run.observations.len() - 1;
    let nan = f32::NAN.to_bits();
    let mut with_refusal = inputs.clone();
    with_refusal.insert(
        last,
        MomentumEvent::Key {
            at: 0,
            tau_bits: nan,
        },
    );
    let mut bent = run.clone();
    let (clearing_cursor, clearing) = bent.observations.remove(last);
    bent.observations.push((
        clearing_cursor,
        MomentumObservation::Refused(AdmitRefusal::Tau(nan)),
    ));
    bent.observations.push((clearing_cursor + 1, clearing));
    let contract = NoStrandedLight::new();
    assert_eq!(contract.judge(schema, &with_refusal, &bent), Verdict::Pass);
    if let MomentumObservation::Frame { hot, .. } = &mut bent.observations[last + 1].1 {
        *hot = true;
    }
    match contract.judge(schema, &with_refusal, &bent) {
        Verdict::Fail { cursor, law, .. } => {
            assert_eq!((cursor, law), (clearing_cursor + 1, COLD_TICK_CLEARS));
        }
        Verdict::Pass => panic!("a refusal lifted ColdTickClears"),
    }
}
