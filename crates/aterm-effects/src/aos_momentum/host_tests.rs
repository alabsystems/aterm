// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live wrapper against the shipped engine, and the live recording against its replay:
//! unattached the host is the engine; attached under the shipped code it shows the host the
//! same frames, quads and wakes; and the recorded stream replayed through `Code::step` from
//! the attach checkpoint lands on the live state and observation chain exactly, from every
//! boundary.

use aterm_aos::checkpoint::{Checkpoint, Provenance};
use aterm_aos::effects::NoEffects;
use aterm_aos::replay::{chain_link, empty_chain, replay};
use aterm_aos::task::{Schema, state_digest};
use aterm_aos::wire::Enc;
use aterm_aos::{BranchId, Revision};
use aterm_time::Instant;

use super::test_fixtures::{
    BUILD, Call, LANE, Seen, TAU, attached, burst, cfg, draw, geom, hosted, ms, play,
    random_stream, replay_log, run_lane,
};
use super::{MOMENTUM_QUEUE_CAP, MomentumCode, MomentumHost, PrebuiltMomentum};
use crate::cursor_momentum::MomentumGlow;

/// The burst-then-lane story on one glow: 24 keys at 60 ms, then the host's lane until it
/// parks.
fn story(g: &mut impl super::test_fixtures::Glow, t0: Instant, dark: bool) -> Vec<Seen> {
    let c = cfg(dark);
    let mut seen = Vec::new();
    let (last, cell) = burst(g, t0, 24, &c, &mut seen);
    let parked = run_lane(g, last, LANE, cell, &c, &mut seen);
    assert!(!parked.stranded, "fixture: the shipped lane parks cold");
    seen
}

/// Unattached (every production window), the host is the shipped engine: on random streams
/// that include inputs outside the profile (NaN and oversized τ, a zero cell, a non-finite
/// intensity, an inverted span, a 2 s lane) and on the typing story, every frame, every
/// quad appended after other effects' and every wake is the engine's own.
#[test]
fn an_unattached_host_shows_exactly_what_the_engine_shows() {
    let t0 = Instant::now();
    for seed in 1..=16u64 {
        for in_profile in [false, true] {
            let calls = random_stream(seed, 400, in_profile);
            let (mut engine, mut host) = (MomentumGlow::default(), MomentumHost::default());
            assert_eq!(
                play(&mut engine, t0, &calls),
                play(&mut host, t0, &calls),
                "seed {seed} (in profile: {in_profile})"
            );
            assert!(
                !host.aos_attached(),
                "nothing attaches a host but aos_attach"
            );
        }
    }
    for dark in [true, false] {
        let (mut engine, mut host) = (MomentumGlow::default(), MomentumHost::default());
        assert_eq!(story(&mut engine, t0, dark), story(&mut host, t0, dark));
    }
}

/// Attached under the shipped code, inside the profile, recording changes nothing the host
/// sees: the same frames, appended quads (other effects' prefixes untouched) and wakes as
/// the engine called directly, on random streams and on the typing story on both pages;
/// every input was admitted and recorded once, except each reset of a glow already at rest,
/// which changes nothing and is not an input.
#[test]
fn an_attached_shipped_host_shows_the_host_what_the_engine_shows() {
    let t0 = Instant::now();
    let mut at_rest_resets = 0;
    for seed in 1..=16u64 {
        let calls = random_stream(seed, 400, true);
        let mut engine = MomentumGlow::default();
        let mut host = attached(t0, MomentumCode::Shipped);
        assert_eq!(
            play(&mut engine, t0, &calls),
            play(&mut host, t0, &calls),
            "seed {seed}"
        );
        let mut probe = MomentumGlow::default();
        let mut inputs = 0;
        for call in &calls {
            let at_rest = probe.snapshot(t0) == MomentumGlow::default().snapshot(t0);
            if !(matches!(call, Call::Reset) && at_rest) {
                inputs += 1;
            }
            let _ = play(&mut probe, t0, std::slice::from_ref(call));
        }
        at_rest_resets += calls.len() - inputs;
        let h = hosted(&host);
        assert_eq!(
            h.boundary.log().map(|l| l.len()),
            Some(inputs),
            "seed {seed}"
        );
        let run = replay_log(&host, MomentumCode::Shipped);
        assert!(
            run.observations
                .iter()
                .all(|(_, o)| !matches!(o, super::MomentumObservation::Refused(_))),
            "seed {seed}: an in-profile input was refused"
        );
    }
    assert!(
        at_rest_resets > 0,
        "fixture: some reset finds the glow at rest"
    );
    for dark in [true, false] {
        let mut engine = MomentumGlow::default();
        let mut host = attached(t0, MomentumCode::Shipped);
        assert_eq!(story(&mut engine, t0, dark), story(&mut host, t0, dark));
    }
}

/// The live-recorded stream, replayed through `Code::step` from the attach checkpoint,
/// lands on the live state's digest and reproduces the live observation chain exactly,
/// for both codes, on random streams and the typing story.
#[test]
fn the_recorded_stream_replays_from_the_attach_checkpoint_to_the_live_state_and_chain() {
    let t0 = Instant::now();
    for code in MomentumCode::ALL.iter().copied() {
        let mut streams: Vec<Vec<Call>> = (1..=8u64).map(|s| random_stream(s, 300, true)).collect();
        streams.push(Vec::new());
        for (i, calls) in streams.iter().enumerate() {
            let mut host = attached(t0, code);
            let _ = play(&mut host, t0, calls);
            if calls.is_empty() {
                for dark in [true, false] {
                    let c = cfg(dark);
                    let mut seen = Vec::new();
                    let start = t0 + ms(10_000 * u64::from(dark));
                    let (last, cell) = burst(&mut host, start, 24, &c, &mut seen);
                    let _ = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
                }
            }
            let run = replay_log(&host, code);
            let h = hosted(&host);
            let live = state_digest(h.boundary.schema(), h.state());
            assert_eq!(run.refused, None, "{code:?} stream {i}");
            assert_eq!(run.state_digest, live, "{code:?} stream {i}: the state");
            assert_eq!(
                run.observation_chain, h.chain.chain,
                "{code:?} stream {i}: the observation chain"
            );
            assert_eq!(run.observations.len() as u64, h.chain.len);
        }
    }
}

/// A checkpoint taken at any boundary of a recorded run, encoded, decoded and restored,
/// continues exactly as the straight run: the same final state digest and, joined to the
/// prefix's, the same observations and chain.
#[test]
fn restoring_at_every_boundary_continues_the_straight_run() {
    let t0 = Instant::now();
    for dark in [true, false] {
        let mut host = attached(t0, MomentumCode::Shipped);
        let c = cfg(dark);
        let mut seen = Vec::new();
        let (last, cell) = burst(&mut host, t0, 12, &c, &mut seen);
        let _ = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
        let h = hosted(&host);
        let b = &h.boundary;
        let schema = b.schema();
        let code = PrebuiltMomentum::new(MomentumCode::Shipped, BUILD);
        let log = b.log().expect("attached");
        let inputs = log.inputs();
        let first = log.header().first_cursor;
        let c0 = b.checkpoint().expect("attached");
        let start = c0.restore(schema, b.identity()).expect("C0 restores");
        let straight = replay(schema, &code, start, inputs, first, &mut NoEffects);
        assert!(inputs.len() > 40, "fixture: a long trace");
        for k in 0..=inputs.len() {
            let head = replay(schema, &code, start, &inputs[..k], first, &mut NoEffects);
            let at = first + k as u64;
            let mid = Checkpoint::capture(
                schema,
                &head.state,
                Provenance {
                    identity: b.identity().clone(),
                    log_epoch: 1,
                    input_cursor: at,
                    effect_cursor: 0,
                    branch: BranchId(0),
                    revision: Revision(0),
                    parent: Some(c0.digest()),
                },
            );
            let bytes = mid.encode().expect("a momentum checkpoint encodes");
            let back = Checkpoint::decode(&bytes).expect("and decodes");
            let resumed = back.restore(schema, b.identity()).expect("and restores");
            let tail = replay(schema, &code, resumed, &inputs[k..], at, &mut NoEffects);
            assert_eq!(
                tail.state_digest, straight.state_digest,
                "page dark={dark}, boundary {k}"
            );
            let joined: Vec<_> = head
                .observations
                .iter()
                .chain(&tail.observations)
                .cloned()
                .collect();
            assert_eq!(
                joined, straight.observations,
                "page dark={dark}, boundary {k}"
            );
            let mut chain = empty_chain();
            for (cursor, obs) in &joined {
                let mut e = Enc::new();
                schema.encode_observation(obs, &mut e);
                chain = chain_link(&chain, *cursor, e.as_bytes());
            }
            assert_eq!(chain, straight.observation_chain);
        }
    }
}

/// A log that fills re-anchors at the next boundary (a fresh checkpoint, an empty log of the
/// next epoch) and the live chain restarts with it: a replay from the newest checkpoint
/// still lands on the live state and chain.
#[test]
fn a_rolling_reanchor_keeps_the_replay_exact() {
    let t0 = Instant::now();
    let mut host = MomentumHost::default();
    host.attach_with(t0, MomentumCode::Shipped, BUILD, 1, MOMENTUM_QUEUE_CAP, 32);
    let calls = random_stream(77, 500, true);
    let _ = play(&mut host, t0, &calls);
    let h = hosted(&host);
    assert!(
        h.boundary.stats().reanchors >= 10,
        "fixture: the log rolled"
    );
    let log = h.boundary.log().expect("attached");
    assert!(log.len() <= 32 && log.header().log_epoch > 1);
    let run = replay_log(&host, MomentumCode::Shipped);
    assert_eq!(
        run.state_digest,
        state_digest(h.boundary.schema(), h.state())
    );
    assert_eq!(run.observation_chain, h.chain.chain);
}

/// Attaching mid-cool-down carries the keys typed before the epoch as negative offsets: the
/// host goes on showing what the engine shows, and the replay from the attach checkpoint
/// reproduces the live state.
#[test]
fn attaching_mid_cool_down_continues_exactly_and_replays() {
    let t0 = Instant::now();
    let c = cfg(true);
    let (mut engine, mut host) = (MomentumGlow::default(), MomentumHost::default());
    let (mut seen_e, mut seen_h) = (Vec::new(), Vec::new());
    let (last, cell) = burst(&mut engine, t0, 20, &c, &mut seen_e);
    let _ = burst(&mut host, t0, 20, &c, &mut seen_h);
    assert_eq!(seen_e, seen_h);
    host.aos_attach(last + ms(5), MomentumCode::Shipped, BUILD, 7);
    let snap = host.aos_status();
    assert!(snap.attached && snap.hot, "fixture: attached while hot");
    let pe = run_lane(&mut engine, last + ms(5), LANE, cell, &c, &mut seen_e);
    let ph = run_lane(&mut host, last + ms(5), LANE, cell, &c, &mut seen_h);
    assert_eq!(seen_e, seen_h);
    assert_eq!((pe.at, pe.stranded), (ph.at, ph.stranded));
    let run = replay_log(&host, MomentumCode::Shipped);
    let h = hosted(&host);
    assert_eq!(
        run.state_digest,
        state_digest(h.boundary.schema(), h.state())
    );
    assert_eq!(run.observation_chain, h.chain.chain);
}

/// Detaching moves the live state back whole: the direct engine then continues exactly as
/// an engine that was never attached.
#[test]
fn detaching_moves_the_live_state_back_whole() {
    let t0 = Instant::now();
    let c = cfg(false);
    let (mut engine, mut host) = (MomentumGlow::default(), attached(t0, MomentumCode::Shipped));
    let (mut seen_e, mut seen_h) = (Vec::new(), Vec::new());
    let (last, cell) = burst(&mut engine, t0, 16, &c, &mut seen_e);
    let _ = burst(&mut host, t0, 16, &c, &mut seen_h);
    host.aos_detach();
    assert!(!host.aos_attached());
    let pe = run_lane(&mut engine, last, LANE, cell, &c, &mut seen_e);
    let ph = run_lane(&mut host, last, LANE, cell, &c, &mut seen_h);
    assert_eq!(seen_e, seen_h);
    assert_eq!(pe.at, ph.at);
}

/// A reset that would change nothing is not an input: at rest, a hundred resets leave the
/// log as it was; paused at rest, two thousand resets (more than the queue holds) neither
/// queue nor reject the update; a reset of a glow that has seen a key is recorded once
/// running and queued once paused, and the resets after it, which would change nothing
/// more, are neither.
#[test]
fn a_reset_that_changes_nothing_is_neither_recorded_nor_queued() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    for _ in 0..100 {
        host.reset();
    }
    assert_eq!(hosted(&host).boundary.log().map(|l| l.len()), Some(0));

    host.aos_pause(t0 + ms(1), LANE).expect("pauses");
    let logged = hosted(&host).boundary.log().map(|l| l.len());
    for _ in 0..2000 {
        host.reset();
    }
    assert!(host.aos_paused(), "the pause is kept");
    assert_eq!(hosted(&host).boundary.queued(), 0);
    assert_eq!(hosted(&host).boundary.stats().rejections, 0);
    assert_eq!(hosted(&host).boundary.log().map(|l| l.len()), logged);
    assert!(host.aos_resume().is_empty());

    let c = cfg(true);
    let mut seen = Vec::new();
    let (last, _) = burst(&mut host, t0 + ms(10), 8, &c, &mut seen);
    let resets = |h: &MomentumHost| {
        hosted(h)
            .boundary
            .log()
            .expect("attached")
            .inputs()
            .iter()
            .filter(|i| matches!(i, super::MomentumEvent::Reset))
            .count()
    };
    for _ in 0..3 {
        host.reset();
    }
    assert_eq!(resets(&host), 1, "running: the reset of a keyed glow, once");
    let (last, _) = burst(&mut host, last + ms(60), 4, &c, &mut seen);
    host.aos_pause(last, LANE).expect("pauses");
    for _ in 0..3 {
        host.reset();
    }
    assert_eq!(hosted(&host).boundary.queued(), 1, "paused: queued once");
    host.on_key(last + ms(60), TAU);
    host.reset();
    host.reset();
    assert_eq!(
        hosted(&host).boundary.queued(),
        3,
        "a key after the queued reset makes the next reset change something again"
    );
    let delivered = host.aos_resume();
    assert_eq!(delivered.len(), 3);
    assert_eq!(resets(&host), 3);
    assert!(!host.is_active() && !host.settling());
}

/// Once a running attach's live time passes the hosted horizon, no timed input can be
/// admitted again, so the next live call ends the attach: a hot glow typed into right up
/// to the horizon is drawn hot by the tick that crosses it (the shipped engine took the state
/// back whole) and then cools and parks as the engine's would.
#[test]
fn a_hot_glow_crossing_the_horizon_ends_the_attach_and_still_cools() {
    let t0 = Instant::now();
    let horizon = crate::cursor_momentum::MOMENTUM_PROFILE.horizon;
    let mut host = attached(t0, MomentumCode::Shipped);
    let c = cfg(true);
    let mut seen = Vec::new();
    let start = t0 + horizon - ms(1_000);
    let (last, cell) = burst(&mut host, start, 17, &c, &mut seen);
    assert!(last < t0 + horizon && t0 + horizon - last < ms(100));
    assert!(host.aos_attached());
    let hot = match seen.last() {
        Some(Seen::Frame { frame, .. }) => frame.hot,
        _ => false,
    };
    assert!(hot, "fixture: hot at the last frame inside the horizon");
    let mut engine: MomentumGlow = *hosted(&host).state().glow();
    let past = t0 + horizon + ms(10);
    let crossing = draw(&mut host, Some(cell), past, geom(), &c, 1);
    assert!(!host.aos_attached(), "the crossing call ended the attach");
    assert_eq!(
        crossing,
        draw(&mut engine, Some(cell), past, geom(), &c, 1),
        "the crossing tick is the engine's"
    );
    assert!(matches!(&crossing, Seen::Frame { frame, out, .. } if frame.hot && !out.is_empty()));
    let parked = run_lane(&mut host, past, LANE, cell, &c, &mut seen);
    assert!(!parked.stranded, "the glow cools and the lane parks");
    assert_eq!(host.next_change_deadline(parked.at + ms(1), LANE), None);

    // Paused, the attach is kept past the horizon: the pause is the operator's.
    let mut paused = attached(t0, MomentumCode::Shipped);
    let (last, _) = burst(&mut paused, start, 4, &c, &mut seen);
    paused.aos_pause(last, LANE).expect("pauses");
    assert_eq!(paused.next_change_deadline(past, LANE), None);
    assert!(paused.aos_attached() && paused.aos_paused());
}
