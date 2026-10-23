// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live repair through the wrapper with injected instants.

use aterm_aos::contract::Verdict;
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::publish::PublishRefusal;
use aterm_aos::task::state_digest;
use aterm_time::Instant;

use super::test_fixtures::{
    BUILD, LANE, Seen, attached, burst, cfg, draw, geom, hosted, hosted_mut, ms, replay_log,
    run_lane, strand,
};
use super::{
    HOT_HAS_WAKE, MOMENTUM_QUEUE_CAP, MomentumCode, MomentumEvent, MomentumHost,
    MomentumObservation, NoStrandedLight, PrebuiltMomentum,
};
use crate::cursor_momentum::{MOMENTUM_GLOW_TAU_S, MomentumGlow};

const TAU: f32 = MOMENTUM_GLOW_TAU_S;

fn stranded(t0: Instant, dark: bool) -> (MomentumHost, Instant, (u16, u16)) {
    strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, dark)
}

#[test]
fn the_stranded_glow_is_repaired_live_and_fresh_keys_heat_and_clear_with_no_reset_or_replay() {
    let t0 = Instant::now();
    for dark in [true, false] {
        let c = cfg(dark);
        let (mut host, now, cell) = stranded(t0, dark);
        let head = host
            .aos_pause(now, LANE)
            .expect("a running attached glow pauses");
        let log = hosted(&host)
            .boundary
            .log()
            .expect("attached")
            .inputs()
            .to_vec();
        assert_eq!(
            log.last(),
            Some(&MomentumEvent::Query {
                at: (now - t0).as_nanos() as i128,
                frame_interval: LANE,
            }),
            "the pause records the schedule the live code would arm"
        );
        let commit = host
            .aos_repair(MomentumCode::Shipped)
            .expect("the shipped code repairs the stranded glow");
        let w = &commit.witness;
        assert!(
            matches!(
                w.live_verdict(),
                Verdict::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            ),
            "dark={dark}: the live code fails the corrected contract: {:?}",
            w.live_verdict()
        );
        assert_eq!(w.candidate_verdict(), &Verdict::Pass);
        assert!(w.same_code_reproduced());
        assert_eq!(w.grade(), EvidenceGrade::Tested);
        assert_eq!(w.head(), &head);
        assert_eq!(w.live_code().name, "momentum-glow.demo-stranded-settle");
        assert_eq!(w.candidate_code().name, "momentum-glow.shipped");
        assert_eq!(commit.revision.0, 1);
        assert!(commit.delivered.is_empty());
        assert_eq!(host.aos_code(), Some(MomentumCode::Shipped));
        assert!(!host.aos_paused());

        assert!(host.settling(), "the accepted state is the settling glow");
        let wake = host
            .next_change_deadline(now, LANE)
            .expect("after the commit the settling glow names its wake");
        assert!(wake > now);
        let mut seen = Vec::new();
        let cleared = run_lane(&mut host, now, LANE, cell, &c, &mut seen);
        assert!(
            !cleared.stranded,
            "dark={dark}: the repaired lane parks cold"
        );
        match cleared.last {
            Some(Seen::Frame { frame, out, under }) => {
                assert!(
                    !frame.hot
                        && frame.fill.is_none()
                        && frame.fp == 0
                        && out.is_empty()
                        && under.is_empty(),
                    "dark={dark}: the lane parks on the inert clearing frame"
                );
            }
            _ => panic!("dark={dark}: the repaired lane drew"),
        }
        let after_commit = host.aos_status().stats;
        assert_eq!(after_commit.replays, 2);
        assert_eq!(after_commit.publications, 1);

        let restart = cleared.at + ms(500);
        let mut seen = Vec::new();
        let (last, cell) = burst(&mut host, restart, 12, &c, &mut seen);
        assert!(
            seen.iter()
                .any(|s| matches!(s, Seen::Frame { frame, .. } if frame.hot)),
            "dark={dark}: fresh keys heat the glow"
        );
        let mut tail = Vec::new();
        let again = run_lane(&mut host, last, LANE, cell, &c, &mut tail);
        assert!(
            !again.stranded,
            "dark={dark}: fresh keys clear with no stranded light"
        );
        let status = host.aos_status().stats;
        assert_eq!(
            (status.replays, status.publications, status.rejections),
            (
                after_commit.replays,
                after_commit.publications,
                after_commit.rejections
            ),
            "dark={dark}: no further replay, publication or rejection"
        );
        let h = hosted(&host);
        let inputs = h.boundary.log().expect("attached").inputs();
        assert!(
            inputs.iter().all(|i| !matches!(i, MomentumEvent::Reset)),
            "dark={dark}: no reset was ever needed"
        );
        assert!(
            inputs
                .iter()
                .any(|i| matches!(i, MomentumEvent::Query { .. })),
            "dark={dark}: the fresh settle was paced by named wakes"
        );
        let run = replay_log(&host, MomentumCode::Shipped);
        assert_eq!(
            run.state_digest,
            state_digest(h.boundary.schema(), h.state())
        );
        assert_eq!(run.observation_chain, h.chain.chain);
        assert_eq!(
            super::Contract::judge(&NoStrandedLight::new(), h.boundary.schema(), inputs, &run),
            Verdict::Pass
        );
    }
}

#[test]
fn a_candidate_that_fails_the_contract_is_refused_and_the_pause_holds() {
    let t0 = Instant::now();
    let (mut host, now, _) = stranded(t0, true);
    host.aos_pause(now, LANE).expect("pauses");
    host.on_key(now + ms(1), TAU);
    match host.aos_repair(MomentumCode::DemoStrandedSettle) {
        Err(PublishRefusal::CandidateFailed(Verdict::Fail { law, .. })) => {
            assert_eq!(law, HOT_HAS_WAKE);
        }
        other => panic!("the faulty candidate was not refused: {other:?}"),
    }
    assert!(host.aos_paused());
    assert_eq!(hosted(&host).boundary.queued(), 1);
}

#[test]
fn repair_needs_a_prior_pause_and_neither_refusal_records_anything() {
    let t0 = Instant::now();
    let (mut host, now, _) = stranded(t0, true);
    let len = hosted(&host).boundary.log().map(|l| l.len());
    assert!(matches!(
        host.aos_repair(MomentumCode::Shipped),
        Err(PublishRefusal::NotPaused)
    ));
    assert_eq!(hosted(&host).boundary.log().map(|l| l.len()), len);
    host.aos_pause(now, LANE).expect("pauses");
    let len = hosted(&host).boundary.log().map(|l| l.len());
    assert_eq!(
        host.aos_pause(now, LANE),
        Err(PublishRefusal::AlreadyPaused)
    );
    assert_eq!(hosted(&host).boundary.log().map(|l| l.len()), len);
    let mut plain = MomentumHost::default();
    assert_eq!(plain.aos_pause(now, LANE), Err(PublishRefusal::NotAttached));
    assert!(matches!(
        plain.aos_repair(MomentumCode::Shipped),
        Err(PublishRefusal::NotAttached)
    ));
}

#[test]
fn keys_while_paused_queue_and_are_delivered_once_after_the_commit() {
    let t0 = Instant::now();
    let (mut host, now, _) = stranded(t0, true);
    host.aos_pause(now, LANE).expect("pauses");
    let before = hosted(&host)
        .boundary
        .pair()
        .state
        .glow
        .value(now + ms(30), TAU);
    let keys = [now + ms(10), now + ms(20), now + ms(30)];
    for k in keys {
        host.on_key(k, TAU);
    }
    assert_eq!(hosted(&host).boundary.queued(), 3);
    assert_eq!(
        hosted(&host)
            .boundary
            .pair()
            .state
            .glow
            .value(now + ms(30), TAU),
        before,
        "a queued key does not reach the glow"
    );
    let commit = host.aos_repair(MomentumCode::Shipped).expect("commits");
    assert_eq!(commit.delivered, vec![Ok(MomentumObservation::Keyed); 3]);
    let log = hosted(&host)
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .to_vec();
    let at = |t: Instant| (t - t0).as_nanos() as i128;
    assert_eq!(
        log,
        keys.iter()
            .map(|k| MomentumEvent::Key {
                at: at(*k),
                tau_bits: TAU.to_bits()
            })
            .collect::<Vec<_>>()
    );
    let control: MomentumGlow = hosted(&host).boundary.pair().state.glow;
    let mut direct = stranded(t0, true).0;
    direct.aos_detach();
    let super::HostMode::Direct(g) = &mut direct.mode else {
        panic!("detached")
    };
    for k in keys {
        g.on_key(k, TAU);
    }
    assert_eq!(control.value(now + ms(40), TAU), g.value(now + ms(40), TAU));
}

#[test]
fn keys_while_paused_are_delivered_once_to_the_old_pair_on_resume() {
    let t0 = Instant::now();
    let (mut host, now, _) = stranded(t0, true);
    host.aos_pause(now, LANE).expect("pauses");
    host.on_key(now + ms(5), TAU);
    host.reset();
    host.on_key(now + ms(9), TAU);
    let delivered = host.aos_resume();
    assert_eq!(
        delivered,
        vec![
            Ok(MomentumObservation::Keyed),
            Ok(MomentumObservation::WasReset),
            Ok(MomentumObservation::Keyed)
        ]
    );
    assert!(!host.aos_paused());
    assert_eq!(host.aos_code(), Some(MomentumCode::DemoStrandedSettle));
    assert!(
        host.aos_resume().is_empty(),
        "a second resume delivers nothing"
    );
    let inputs = hosted(&host)
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .to_vec();
    assert_eq!(&inputs[inputs.len() - 3..][1], &MomentumEvent::Reset);
    let run = replay_log(&host, MomentumCode::DemoStrandedSettle);
    let h = hosted(&host);
    assert_eq!(
        run.state_digest,
        state_digest(h.boundary.schema(), h.state())
    );
    assert_eq!(run.observation_chain, h.chain.chain);
}

#[test]
fn ticks_while_paused_show_the_held_overlay_and_do_not_advance_the_state() {
    let t0 = Instant::now();
    for dark in [true, false] {
        let c = cfg(dark);
        let mut host = attached(t0, MomentumCode::Shipped);
        let mut seen = Vec::new();
        let (last, cell) = burst(&mut host, t0, 16, &c, &mut seen);
        let held = seen.last().cloned().expect("a frame");
        host.aos_pause(last, LANE).expect("pauses");
        let digest = {
            let h = hosted(&host);
            state_digest(h.boundary.schema(), h.state())
        };
        let cursor = hosted(&host).cursor();
        for k in 1..=5u64 {
            let shown = draw(&mut host, Some(cell), last + ms(16 * k), geom(), &c, 2);
            assert_eq!(shown, held, "dark={dark}: the held overlay");
        }
        assert!(!host.is_active() && !host.settling());
        assert_eq!(host.next_change_deadline(last + ms(90), LANE), None);
        let h = hosted(&host);
        assert_eq!(state_digest(h.boundary.schema(), h.state()), digest);
        assert_eq!(h.cursor(), cursor, "a paused tick or query is not recorded");
        assert_eq!(h.boundary.stats().skipped, 6);
    }
}

#[test]
fn a_full_queue_rejects_the_update_and_loses_no_key() {
    let t0 = Instant::now();
    let mut host = MomentumHost::default();
    host.attach_with(t0, MomentumCode::DemoStrandedSettle, BUILD, 1, 2, 1 << 16);
    let mut engine = MomentumGlow::default();
    host.on_key(t0, TAU);
    engine.on_key(t0, TAU);
    host.aos_pause(t0 + ms(1), LANE).expect("pauses");
    let keys = [t0 + ms(10), t0 + ms(20), t0 + ms(30)];
    for k in keys {
        host.on_key(k, TAU);
        engine.on_key(k, TAU);
    }
    assert!(!host.aos_paused(), "the third key rejected the update");
    assert!(matches!(
        host.aos_repair(MomentumCode::Shipped),
        Err(PublishRefusal::NotPaused)
    ));
    let h = hosted(&host);
    assert_eq!(h.boundary.stats().rejections, 1);
    assert_eq!(h.boundary.stats().queued_max, 2);
    let keyed = h
        .boundary
        .log()
        .expect("attached")
        .inputs()
        .iter()
        .filter(|i| matches!(i, MomentumEvent::Key { .. }))
        .count();
    assert_eq!(keyed, 4, "every key is in the log once");
    let probe = t0 + ms(31);
    assert_eq!(h.state().glow.value(probe, TAU), engine.value(probe, TAU));
    let run = replay_log(&host, MomentumCode::DemoStrandedSettle);
    assert_eq!(
        run.state_digest,
        state_digest(h.boundary.schema(), h.state())
    );
    assert_eq!(run.observation_chain, h.chain.chain);
}

#[test]
fn a_second_candidate_prepared_against_the_old_head_is_refused_after_a_commit() {
    let t0 = Instant::now();
    let (mut host, now, _) = stranded(t0, true);
    let head = host.aos_pause(now, LANE).expect("pauses");
    let h = hosted_mut(&mut host);
    let first = h
        .boundary
        .prepare(PrebuiltMomentum::new(MomentumCode::Shipped, BUILD))
        .expect("prepared");
    let second = h
        .boundary
        .prepare(PrebuiltMomentum::new(MomentumCode::Shipped, BUILD))
        .expect("prepared against the same head");
    assert_eq!((first.expected(), second.expected()), (&head, &head));
    let commit = h.boundary.commit(first).expect("the first commits");
    assert!(!h.boundary.is_paused());
    let live = h.boundary.pause().expect("the new pair pauses");
    assert_eq!(live.revision, commit.revision);
    match h.boundary.commit(second) {
        Err(PublishRefusal::StaleHead {
            expected,
            live: now_live,
        }) => {
            assert_eq!(*expected, head);
            assert_eq!(*now_live, live);
        }
        other => panic!("a stale candidate was not refused: {other:?}"),
    }
    assert_eq!(
        h.boundary.pair().head,
        live,
        "the refusal left the live pair as it was"
    );
    assert!(h.boundary.is_paused(), "the refusal held the pause");
    assert_eq!(h.boundary.stats().publications, 1);
}

#[test]
fn re_attaching_re_anchors_and_another_code_starts_afresh() {
    let t0 = Instant::now();
    let c = cfg(true);
    let mut host = attached(t0, MomentumCode::Shipped);
    let mut seen = Vec::new();
    let _ = burst(&mut host, t0, 6, &c, &mut seen);
    let old = hosted(&host)
        .boundary
        .checkpoint()
        .expect("attached")
        .digest();
    host.aos_attach(t0, MomentumCode::Shipped, BUILD, 2);
    let b = &hosted(&host).boundary;
    let ck = b.checkpoint().expect("attached");
    assert_eq!(ck.provenance().parent, Some(old));
    assert_eq!(
        b.log().map(|l| (l.header().log_epoch, l.len())),
        Some((2, 0))
    );
    let state = *hosted(&host).state().glow();
    host.aos_attach(t0, MomentumCode::DemoStrandedSettle, BUILD, 3);
    let b = &hosted(&host).boundary;
    assert_eq!(b.checkpoint().map(|c| c.provenance().parent), Some(None));
    assert_eq!(host.aos_code(), Some(MomentumCode::DemoStrandedSettle));
    let probe = t0 + ms(400);
    assert_eq!(
        hosted(&host).state().glow().value(probe, TAU),
        state.value(probe, TAU)
    );
    assert_eq!(MOMENTUM_QUEUE_CAP, hosted(&host).boundary.queue_cap());
}

#[test]
fn the_exported_trace_decodes_to_the_live_checkpoint_and_log() {
    let t0 = Instant::now();
    let (host, _, _) = stranded(t0, false);
    let trace = host
        .aos_trace()
        .expect("an attached glow exports its trace");
    let b = &hosted(&host).boundary;
    let ck = aterm_aos::checkpoint::Checkpoint::decode(&trace.checkpoint).expect("decodes");
    assert_eq!(Some(&ck), b.checkpoint());
    let (log, tail) = aterm_aos::log::EventLog::decode(
        b.schema(),
        &trace.log,
        &trace.identity,
        &ck.digest(),
        1 << 16,
    )
    .expect("decodes");
    assert_eq!(tail, aterm_aos::log::Tail::Clean);
    assert_eq!(log.inputs(), b.log().expect("attached").inputs());
    assert_eq!(
        MomentumHost::default().aos_trace(),
        Err(super::TraceRefusal::NotAttached)
    );
}

#[test]
fn a_paused_tick_shows_the_held_overlay_only_for_its_own_caret_while_its_gates_allow_the_glow() {
    let t0 = Instant::now();
    let c = cfg(true);
    let mut host = attached(t0, MomentumCode::Shipped);
    let mut seen = Vec::new();
    let (last, cell) = burst(&mut host, t0, 16, &c, &mut seen);
    let held = seen.last().cloned().expect("a frame");
    assert!(
        matches!(&held, Seen::Frame { frame, out, .. } if frame.hot && !out.is_empty()),
        "fixture: a hot frame is held"
    );
    host.aos_pause(last, LANE).expect("pauses");
    let cursor = hosted(&host).cursor();
    let inert = Seen::Frame {
        frame: crate::cursor_momentum::MomentumGlowFrame::default(),
        out: Vec::new(),
        under: Vec::new(),
    };
    let off = crate::cursor_momentum::MomentumGlowConfig {
        enabled: false,
        ..c
    };
    let dim = crate::cursor_momentum::MomentumGlowConfig {
        intensity: 0.0,
        ..c
    };
    let bar = crate::cursor_momentum::MomentumGlowConfig { block: false, ..c };
    let light = cfg(false);
    let moved = Some((cell.0, cell.1 + 1));
    let other = super::test_fixtures::headed_geom();
    let cases = [
        ("enabled=false", Some(cell), geom(), off),
        ("intensity 0", Some(cell), geom(), dim),
        ("cur None", None, geom(), c),
        ("other geometry", Some(cell), other, c),
        ("other cell", moved, geom(), c),
        ("other page", Some(cell), geom(), light),
        ("bar caret", Some(cell), geom(), bar),
    ];
    for (k, (why, cur, g, cf)) in cases.into_iter().enumerate() {
        let at = last + ms(16 * (k as u64 + 1));
        assert_eq!(draw(&mut host, cur, at, g, &cf, 2), inert, "{why}");
        assert_eq!(
            draw(&mut host, Some(cell), at, geom(), &c, 2),
            held,
            "after {why}: the overlay's own caret still shows it"
        );
    }
    assert!(host.aos_paused());
    assert_eq!(hosted(&host).cursor(), cursor, "no paused tick is recorded");
    assert_eq!(hosted(&host).boundary.stats().skipped, 14);
}

#[test]
fn a_long_attached_run_reanchors_and_a_repair_after_it_still_produces_the_witness() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::DemoStrandedSettle);
    let calls = super::test_fixtures::random_stream(0x05ee_da05, 5000, true);
    let _ = super::test_fixtures::play(&mut host, t0, &calls);
    let end = calls
        .iter()
        .map(|call| match *call {
            super::test_fixtures::Call::Key { at, .. }
            | super::test_fixtures::Call::Tick { at, .. }
            | super::test_fixtures::Call::Query { at, .. } => at,
            super::test_fixtures::Call::Reset => std::time::Duration::ZERO,
        })
        .max()
        .expect("calls");
    let status = host.aos_status();
    assert!(
        status.stats.reanchors >= 1,
        "fixture: the run re-anchored ({status})"
    );
    assert_eq!(status.broken, None);
    let c = cfg(true);
    let mut seen = Vec::new();
    let (last, cell) = burst(&mut host, t0 + end + ms(2_000), 24, &c, &mut seen);
    let parked = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
    assert!(parked.stranded, "fixture: the fault strands the glow");
    host.aos_pause(parked.at, LANE).expect("pauses");
    let log = hosted(&host).boundary.log().expect("attached");
    assert!(log.len() <= super::MOMENTUM_HOST_MAX_EVENTS);
    assert!(
        log.header().log_epoch > 1,
        "the pause lands in a later epoch"
    );
    assert!(matches!(
        log.inputs().last(),
        Some(MomentumEvent::Query { .. })
    ));
    let commit = host
        .aos_repair(MomentumCode::Shipped)
        .expect("the shipped code repairs the stranded glow");
    let w = &commit.witness;
    assert!(
        matches!(
            w.live_verdict(),
            Verdict::Fail {
                law: HOT_HAS_WAKE,
                ..
            }
        ),
        "the live code fails from the re-anchored checkpoint: {:?}",
        w.live_verdict()
    );
    assert_eq!(w.candidate_verdict(), &Verdict::Pass);
    assert!(w.same_code_reproduced());
    assert_eq!(w.grade(), EvidenceGrade::Tested);
}
