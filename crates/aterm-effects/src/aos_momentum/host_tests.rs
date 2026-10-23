// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live wrapper tests against the shipped engine and recorded replay.

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

fn story(g: &mut impl super::test_fixtures::Glow, t0: Instant, dark: bool) -> Vec<Seen> {
    let c = cfg(dark);
    let mut seen = Vec::new();
    let (last, cell) = burst(g, t0, 24, &c, &mut seen);
    let parked = run_lane(g, last, LANE, cell, &c, &mut seen);
    assert!(!parked.stranded, "fixture: the shipped lane parks cold");
    seen
}

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

    let mut paused = attached(t0, MomentumCode::Shipped);
    let (last, _) = burst(&mut paused, start, 4, &c, &mut seen);
    paused.aos_pause(last, LANE).expect("pauses");
    assert_eq!(paused.next_change_deadline(past, LANE), None);
    assert!(paused.aos_attached() && paused.aos_paused());
}

#[test]
fn the_reused_encoder_and_recycled_overlay_change_no_link_and_no_held_quad() {
    let t0 = Instant::now();
    let (mut grew, mut shrank, mut empty) = (0u32, 0u32, 0u32);
    let mut check = |host: &mut MomentumHost, calls: &[Call], what: &str| {
        let mut before = 0usize;
        for (i, call) in calls.iter().enumerate() {
            let seen = play(host, t0, std::slice::from_ref(call));
            let h = hosted(host);
            if let (Call::Tick { .. }, [Seen::Frame { frame, out, under }]) = (call, &seen[..]) {
                let held = h.held.as_ref().expect("a delivered tick is held");
                assert_eq!(
                    (&held.frame, &held.out, &held.under),
                    (frame, out, under),
                    "{what}, call {i}: the held overlay"
                );
                let n = out.len() + under.len();
                if n == 0 {
                    empty += 1;
                    assert_eq!(
                        (held.out.capacity(), held.under.capacity()),
                        (0, 0),
                        "{what}, call {i}: an overlay that holds nothing keeps no room"
                    );
                } else if n > before {
                    grew += 1;
                } else if n < before {
                    shrank += 1;
                }
                before = n;
            }
        }
        let run = replay_log(host, MomentumCode::Shipped);
        let h = hosted(host);
        assert_eq!(run.refused, None, "{what}");
        assert_eq!(run.observation_chain, h.chain.chain, "{what}: the chain");
        assert_eq!(run.observations.len() as u64, h.chain.len, "{what}");
    };
    for seed in 1..=8u64 {
        for in_profile in [true, false] {
            let calls = random_stream(seed, 300, in_profile);
            let mut host = attached(t0, MomentumCode::Shipped);
            check(
                &mut host,
                &calls,
                &format!("seed {seed} in profile {in_profile}"),
            );
        }
    }
    for dark in [true, false] {
        let c = cfg(dark);
        let mut recorder = Recorder {
            glow: MomentumGlow::default(),
            t0,
            calls: Vec::new(),
        };
        let mut seen = Vec::new();
        let (last, cell) = burst(&mut recorder, t0, 24, &c, &mut seen);
        let _ = run_lane(&mut recorder, last, LANE, cell, &c, &mut seen);
        let mut host = attached(t0, MomentumCode::Shipped);
        check(&mut host, &recorder.calls, &format!("story dark={dark}"));
    }
    assert!(
        grew > 0 && shrank > 0 && empty > 0,
        "fixture: frames grew ({grew}), shrank ({shrank}) and drew nothing ({empty})"
    );
}

struct Recorder {
    glow: MomentumGlow,
    t0: Instant,
    calls: Vec<Call>,
}

impl Recorder {
    fn at(&self, now: Instant) -> std::time::Duration {
        now.saturating_duration_since(self.t0)
    }
}

impl super::test_fixtures::Glow for Recorder {
    fn key(&mut self, now: Instant, tau: f32) {
        self.calls.push(Call::Key {
            at: self.at(now),
            tau,
        });
        self.glow.on_key(now, tau);
    }
    fn rest(&mut self) {
        self.calls.push(Call::Reset);
        self.glow.reset();
    }
    fn frame(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: crate::cursor_glow::Geom,
        cfg: &crate::cursor_momentum::MomentumGlowConfig,
        out: &mut Vec<aterm_render::GlowQuad>,
        under: &mut Vec<aterm_render::GlowQuad>,
    ) -> crate::cursor_momentum::MomentumGlowFrame {
        self.calls.push(Call::Tick {
            cur,
            at: self.at(now),
            geom,
            cfg: *cfg,
            prefix: out.len(),
        });
        self.glow.tick(cur, now, geom, cfg, out, under)
    }
    fn active(&self) -> bool {
        self.glow.is_active()
    }
    fn settles(&self) -> bool {
        self.glow.settling()
    }
    fn cold(&self) -> Option<Instant> {
        self.glow.cold_at()
    }
    fn wake(&mut self, now: Instant, lane: std::time::Duration) -> Option<Instant> {
        self.calls.push(Call::Query {
            at: self.at(now),
            lane,
        });
        self.glow.next_change_deadline(now, lane)
    }
}
