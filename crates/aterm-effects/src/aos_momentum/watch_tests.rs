// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The safe code, off-main repair, the online watcher and the trace check.

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::NoEffects;
use aterm_aos::log::IdentityMismatch;
use aterm_aos::publish::{PublishRefusal, Retire};
use aterm_aos::registry::Judged;
use aterm_aos::replay::{Run, replay};
use aterm_time::Instant;

use super::corpus::{Corpus, GOLDEN_TRACE};
use super::lane::run;
use super::test_fixtures::{
    BUILD, LANE, attached, attached_for_lane, burst, cfg, draw, geom, hosted, hosted_mut,
    judged_log, ms, replay_by, run_lane, strand,
};
use super::{
    HOT_HAS_WAKE, MomentumCode, MomentumEvent, MomentumHost, MomentumObservation, MomentumSchema,
    MomentumTrace, NoStrandedLight, PrebuiltMomentum, Sched, Segment, TraceFault,
};

/// The run's Frame observations, in order.
fn frames(r: &Run<MomentumSchema>) -> Vec<&MomentumObservation> {
    r.observations
        .iter()
        .map(|(_, o)| o)
        .filter(|o| matches!(o, MomentumObservation::Frame { .. }))
        .collect()
}

/// `code` replayed over `inputs` from `start`, with the contract's verdict.
fn judged(
    schema: &MomentumSchema,
    code: MomentumCode,
    start: super::MomentumState,
    inputs: &[MomentumEvent],
    first: u64,
) -> (Run<MomentumSchema>, Verdict) {
    let r = replay(
        schema,
        &PrebuiltMomentum::new(code, BUILD),
        start,
        inputs,
        first,
        &mut NoEffects,
    );
    assert_eq!(r.refused, None, "{code:?} replays");
    let v = NoStrandedLight::new().judge(schema, inputs, &r);
    (r, v)
}

#[test]
fn the_safe_code_keeps_the_contract_on_the_golden_trace_with_the_shipped_frames() {
    let t0 = Instant::now();
    let trace = MomentumTrace::decode(GOLDEN_TRACE).expect("the golden trace decodes");
    let read = trace.read(t0).expect("the golden trace reads");
    let start = || {
        read.checkpoint
            .restore(&read.schema, &trace.identity)
            .expect("its checkpoint restores")
    };
    let (inputs, first) = (read.log.inputs(), read.log.header().first_cursor);
    let (safe, v) = judged(&read.schema, MomentumCode::Safe, start(), inputs, first);
    assert_eq!(v, Verdict::Pass);
    let (shipped, _) = judged(&read.schema, MomentumCode::Shipped, start(), inputs, first);
    assert!(!frames(&shipped).is_empty(), "fixture: the trace draws");
    assert_eq!(frames(&safe), frames(&shipped));
    assert_eq!(safe.state_digest, shipped.state_digest);
}

#[test]
fn the_safe_code_keeps_the_contract_on_the_m4_corpus_live_and_over_the_shipped_logs() {
    let t0 = Instant::now();
    let c = Corpus::generate().expect("generator v1's scenarios are admitted");
    let mut lanes = 0usize;
    for item in c.items() {
        let Some(s) = item.scenario() else { continue };
        lanes += 1;
        let mut live = attached_for_lane(t0, Sched::Prebuilt(MomentumCode::Safe));
        let _ = run(&mut live, s, t0);
        assert_eq!(
            judged_log(&live),
            (Verdict::Pass, true),
            "{}: the safe code run live",
            item.id()
        );
        let mut shipped = attached_for_lane(t0, Sched::Prebuilt(MomentumCode::Shipped));
        let _ = run(&mut shipped, s, t0);
        let ran = replay_by(
            &shipped,
            &PrebuiltMomentum::new(MomentumCode::Shipped, BUILD),
        );
        let safe = replay_by(&shipped, &PrebuiltMomentum::new(MomentumCode::Safe, BUILD));
        let b = &hosted(&shipped).boundary;
        let inputs = b.log().expect("attached").inputs();
        assert_eq!(
            NoStrandedLight::new().judge(b.schema(), inputs, &safe),
            Verdict::Pass,
            "{}: the safe code over the shipped log",
            item.id()
        );
        assert_eq!(
            frames(&safe),
            frames(&ran),
            "{}: the same frames",
            item.id()
        );
    }
    assert!(lanes > 0, "fixture: the corpus has lane items");
}

#[test]
fn the_safe_code_keeps_the_contract_on_the_demonstration_faults_logs() {
    let t0 = Instant::now();
    for dark in [true, false] {
        let (host, _, _) = strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, dark);
        let b = &hosted(&host).boundary;
        let inputs = b.log().expect("attached").inputs();
        let judge = |code| {
            let r = replay_by(&host, &PrebuiltMomentum::new(code, BUILD));
            let v = NoStrandedLight::new().judge(b.schema(), inputs, &r);
            (r, v)
        };
        let (demo, dv) = judge(MomentumCode::DemoStrandedSettle);
        assert!(
            matches!(
                dv,
                Verdict::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            ),
            "dark={dark}: negative control: the demonstration fault fails: {dv:?}"
        );
        let (safe, sv) = judge(MomentumCode::Safe);
        assert_eq!(sv, Verdict::Pass, "dark={dark}");
        let (shipped, _) = judge(MomentumCode::Shipped);
        assert!(!frames(&demo).is_empty(), "fixture: the fault draws");
        assert_eq!(frames(&safe), frames(&shipped), "dark={dark}");
        assert_eq!(frames(&safe), frames(&demo), "dark={dark}");

        let c = cfg(dark);
        let mut glow = attached(t0, MomentumCode::Safe);
        let mut seen = Vec::new();
        let (last, cell) = burst(&mut glow, t0, 24, &c, &mut seen);
        let parked = run_lane(&mut glow, last, LANE, cell, &c, &mut seen);
        assert!(!parked.stranded, "dark={dark}: the safe code clears live");
        assert_eq!(judged_log(&glow), (Verdict::Pass, true), "dark={dark}");
    }
}

#[test]
fn the_safe_code_names_a_frame_wake_while_hot_or_settling_and_none_at_rest() {
    let t0 = Instant::now();
    let c = cfg(true);
    let mut host = attached(t0, MomentumCode::Shipped);
    let mut seen = Vec::new();
    let (last, cell) = burst(&mut host, t0, 24, &c, &mut seen);
    let parked = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
    let cold = parked.at + ms(5_000);
    let _ = draw(&mut host, Some(cell), cold, geom(), &c, 1);
    assert_eq!(
        host.next_change_deadline(cold + ms(1), LANE),
        None,
        "fixture: a query at rest"
    );
    let log = hosted(&host).boundary.log().expect("attached");
    let first = log.header().first_cursor;
    let safe = replay_by(&host, &PrebuiltMomentum::new(MomentumCode::Safe, BUILD));
    let shipped = replay_by(&host, &PrebuiltMomentum::new(MomentumCode::Shipped, BUILD));
    let (mut lit, mut rest, mut differs) = (0, 0, 0);
    for ((cursor, s), (_, p)) in safe.observations.iter().zip(&shipped.observations) {
        let MomentumObservation::Schedule {
            is_active,
            settling,
            deadline,
            ..
        } = s
        else {
            continue;
        };
        let k = usize::try_from(cursor - first).expect("a cursor in the log");
        let MomentumEvent::Query { at, frame_interval } = log.inputs()[k] else {
            panic!("cursor {cursor}: a schedule answers a query");
        };
        if *is_active || *settling {
            let frame = i128::try_from(frame_interval.as_nanos()).expect("a frame interval");
            assert_eq!(
                *deadline,
                Some(at + frame),
                "cursor {cursor}: a frame's wake"
            );
            lit += 1;
        } else {
            assert_eq!(*deadline, None, "cursor {cursor}: at rest");
            rest += 1;
        }
        differs += usize::from(s != p);
    }
    assert!(lit > 0 && rest > 0, "fixture: queries lit and at rest");
    assert!(
        differs > 0,
        "negative control: the shipped code answers otherwise somewhere, so safe is not shipped"
    );
}

#[test]
fn a_repair_prepared_on_another_thread_commits_on_the_host_with_no_replay_on_it() {
    let t0 = Instant::now();
    let (mut host, now, _) = strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, true);
    let head = host.aos_pause(now, LANE).expect("pauses");
    host.on_key(now + ms(1), super::test_fixtures::TAU);
    let job = host
        .aos_repair_job(MomentumCode::Shipped)
        .expect("a paused glow makes a job");
    let worker = std::thread::spawn(move || job.prepare());
    let prepared = worker.join().expect("the worker ran");
    assert_eq!(prepared.replays(), 2, "both shadow replays ran on the copy");
    assert_eq!(prepared.refusal(), None);
    assert_eq!(host.aos_status().stats.replays, 0, "none ran on the host");
    let commit = host.aos_commit(prepared).expect("the host commits");
    assert_eq!(commit.witness.head(), &head);
    assert_eq!(commit.witness.candidate_verdict(), &Verdict::Pass);
    assert_eq!(
        commit.delivered.len(),
        1,
        "the queued key is delivered once"
    );
    assert_eq!(host.aos_code(), Some(MomentumCode::Shipped));
    let st = host.aos_status().stats;
    assert_eq!((st.replays, st.publications), (0, 1));
    assert_eq!(judged_log(&host), (Verdict::Pass, true));

    let (mut inplace, now, _) = strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, true);
    inplace.aos_pause(now, LANE).expect("pauses");
    inplace.aos_repair(MomentumCode::Shipped).expect("repairs");
    assert_eq!(
        inplace.aos_status().stats.replays,
        2,
        "negative control: the in-place repair replays on the host"
    );
}

#[test]
fn negative_control_a_job_whose_head_moved_is_refused_and_changes_nothing() {
    let t0 = Instant::now();
    let paused = || {
        let (mut host, now, _) = strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, true);
        host.aos_pause(now, LANE).expect("pauses");
        (host, now)
    };

    let (mut host, now) = paused();
    let prepared = host
        .aos_repair_job(MomentumCode::Shipped)
        .unwrap()
        .prepare();
    let _ = host.aos_resume();
    host.on_key(now + ms(2), super::test_fixtures::TAU);
    let moved = host.aos_pause(now + ms(3), LANE).expect("pauses again");
    assert!(
        matches!(
            host.aos_commit(prepared),
            Err(PublishRefusal::StaleHead { .. })
        ),
        "an input since the job"
    );
    assert_eq!(hosted(&host).boundary.paused_head(), Some(&moved));
    assert_eq!(host.aos_code(), Some(MomentumCode::DemoStrandedSettle));

    let (mut host, now) = paused();
    let prepared = host
        .aos_repair_job(MomentumCode::Shipped)
        .unwrap()
        .prepare();
    host.aos_attach(t0, MomentumCode::DemoStrandedSettle, BUILD, 9);
    host.aos_pause(now, LANE)
        .expect("pauses the fresh recording");
    let refused = host.aos_commit(prepared).err();
    assert!(
        matches!(
            refused,
            Some(PublishRefusal::StaleHead { .. } | PublishRefusal::WitnessMismatch(_))
        ),
        "a re-attach since the job: {refused:?}"
    );
    assert_eq!(host.aos_status().stats.publications, 0);

    let (mut host, _) = paused();
    let prepared = host
        .aos_repair_job(MomentumCode::Shipped)
        .unwrap()
        .prepare();
    host.aos_detach();
    assert_eq!(
        host.aos_commit(prepared).err(),
        Some(PublishRefusal::NotAttached)
    );

    let (host, _) = paused();
    let failing = host
        .aos_repair_job(MomentumCode::DemoStrandedSettle)
        .unwrap()
        .prepare();
    assert!(failing.refusal().is_some(), "the fault's own code fails");
    let mut host = host;
    assert!(host.aos_commit(failing).is_err());
    assert!(host.aos_paused(), "a refused job keeps the pause");

    let running = attached(t0, MomentumCode::Shipped);
    assert_eq!(
        running.aos_repair_job(MomentumCode::Shipped).err(),
        Some(PublishRefusal::NotPaused)
    );
    assert_eq!(
        MomentumHost::default()
            .aos_repair_job(MomentumCode::Shipped)
            .err(),
        Some(PublishRefusal::NotAttached)
    );
}

/// A host running `code` over typing bursts, each run until its lane parks, until its log has re-anchored twice.
fn long_run(t0: Instant, code: MomentumCode) -> MomentumHost {
    let mut host = attached(t0, code);
    let c = cfg(true);
    let mut at = t0;
    for _ in 0..1000 {
        if host.aos_retired_waiting() >= 2 {
            return host;
        }
        let mut seen = Vec::new();
        let (last, cell) = burst(&mut host, at, 24, &c, &mut seen);
        let parked = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
        at = parked.at + ms(500);
    }
    panic!("fixture: the log never re-anchored twice");
}

#[test]
fn the_watcher_pins_the_demonstration_faults_segments_and_passes_the_shipped_ones() {
    let t0 = Instant::now();
    let mut demo = long_run(t0, MomentumCode::DemoStrandedSettle);
    assert!(
        demo.aos_retired_waiting() >= 1,
        "fixture: the log re-anchored"
    );
    let judged: Vec<_> = demo
        .aos_take_retired()
        .into_iter()
        .map(Segment::judge)
        .collect();
    assert_eq!(demo.aos_retired_waiting(), 0, "taken");
    assert!(judged.iter().all(|j| j.cause == Retire::Full));
    assert!(
        judged
            .iter()
            .all(|j| !matches!(j.judged, Judged::Unjudged(_))),
        "{judged:?}"
    );
    let failed: Vec<_> = judged.iter().filter(|j| j.judged.failed()).collect();
    assert!(
        !failed.is_empty(),
        "the demonstration fault fails a segment"
    );
    for j in &failed {
        assert!(
            matches!(
                j.judged,
                Judged::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            ),
            "{j:?}"
        );
        assert_eq!(j.code, "demo-stranded-settle");
        let fault = j
            .fault
            .as_ref()
            .expect("a failing segment is kept as a trace");
        let back = MomentumTrace::decode(&fault.encode()).expect("the kept fault decodes");
        let check = back.check(t0, BUILD).expect("the kept fault replays");
        assert!(
            matches!(
                check.verdict,
                Verdict::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            ),
            "{:?}",
            check.verdict
        );
        assert_eq!(check.replay.log_epoch, j.log_epoch);
        assert_eq!(check.replay.first_cursor, j.from);
    }
    assert!(
        judged.windows(2).all(|w| w[0].to == w[1].from),
        "contiguous"
    );

    for code in [MomentumCode::Shipped, MomentumCode::Safe] {
        let mut good = long_run(t0, code);
        let judged: Vec<_> = good
            .aos_take_retired()
            .into_iter()
            .map(Segment::judge)
            .collect();
        assert!(!judged.is_empty(), "{code:?}: fixture: the log re-anchored");
        for j in judged {
            assert_eq!(j.judged, Judged::Pass, "{code:?}: {j:?}");
            assert_eq!(j.fault, None);
        }
    }
}

#[test]
fn a_segment_whose_replay_misses_its_end_state_or_whose_log_broke_is_left_unjudged() {
    let t0 = Instant::now();
    let mut host = long_run(t0, MomentumCode::Shipped);
    let schema = hosted(&host).boundary.schema().clone();
    let mut retired = hosted_mut(&mut host).boundary.drain_retired();
    let mut r = retired.pop().expect("fixture: the log re-anchored");
    let honest = Segment::new(schema.clone(), r.clone()).judge();
    assert_eq!(
        honest.judged,
        Judged::Pass,
        "negative control: the honest segment"
    );
    r.end_state = r.checkpoint.state_digest();
    let moved = Segment::new(schema.clone(), r).judge();
    assert_eq!(
        moved.judged,
        Judged::Unjudged("the replay does not reach the state the segment ended in")
    );
    assert_eq!(moved.fault, None);

    let mut broke = attached(t0, MomentumCode::DemoStrandedSettle);
    let mut seen = Vec::new();
    let _ = burst(&mut broke, t0, 4, &cfg(true), &mut seen);
    hosted_mut(&mut broke)
        .boundary
        .mark_broken("the host lost an input");
    let _ = burst(&mut broke, t0 + ms(400), 1, &cfg(true), &mut seen);
    let judged: Vec<_> = broke
        .aos_take_retired()
        .into_iter()
        .map(Segment::judge)
        .collect();
    assert_eq!(judged.len(), 1);
    assert_eq!(judged[0].cause, Retire::Broken);
    assert_eq!(judged[0].judged, Judged::Unjudged("the log is broken"));
}

#[test]
fn a_commit_retires_the_live_codes_segment_and_the_watcher_judges_it() {
    let t0 = Instant::now();
    let (mut host, now, _) = strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, true);
    host.aos_pause(now, LANE).expect("pauses");
    host.aos_repair(MomentumCode::Shipped).expect("repairs");
    let judged: Vec<_> = host
        .aos_take_retired()
        .into_iter()
        .map(Segment::judge)
        .collect();
    assert_eq!(judged.len(), 1);
    assert_eq!(judged[0].cause, Retire::Committed);
    assert_eq!(judged[0].code, "demo-stranded-settle");
    assert!(
        matches!(
            judged[0].judged,
            Judged::Fail {
                law: HOT_HAS_WAKE,
                ..
            }
        ),
        "{:?}",
        judged[0].judged
    );
}

#[test]
fn a_trace_from_another_build_is_refused_identity_before_it_replays() {
    let t0 = Instant::now();
    let (host, _, _) = strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, true);
    let trace = host.aos_trace().expect("exports");
    let ours = trace
        .check(t0, BUILD)
        .expect("this build replays its own trace");
    assert!(matches!(
        ours.verdict,
        Verdict::Fail {
            law: HOT_HAS_WAKE,
            ..
        }
    ));
    assert_eq!(ours.replay.code, MomentumCode::DemoStrandedSettle);
    assert_eq!(
        trace.check(t0, "another-build").err(),
        Some(TraceFault::Identity(IdentityMismatch::Code))
    );
    let mut foreign_task = trace.clone();
    foreign_task.identity.task.name = "another-task".to_owned();
    assert_eq!(
        foreign_task.check(t0, BUILD).err(),
        Some(TraceFault::Identity(IdentityMismatch::Task))
    );
    assert!(
        matches!(
            trace.replay(t0, "another-build"),
            Err(TraceFault::UnknownCode { .. })
        ),
        "the plain replay names only the code"
    );
}
