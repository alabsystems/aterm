// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trace file round-trip, tamper refusal, and replay verification tests.

use aterm_aos::Digest;
use aterm_aos::log::{EventLog, LogFault, Tail};
use aterm_aos::replay::empty_chain;
use aterm_aos::wire::WireFault;
use aterm_time::Instant;

use super::test_fixtures::{
    BUILD, Glow, LANE, attached, attached_for_lane, burst, cfg, draw, geom, hosted, ms, run_lane,
};
use super::{
    EventCounts, MOMENTUM_HOST_MAX_EVENTS, MOMENTUM_TRACE_MAX_BYTES, MomentumCode, MomentumEvent,
    MomentumHost, MomentumSchema, MomentumTrace, Sched, TRACE_FILE_MAGIC, TraceFault,
};

fn typed(t0: Instant) -> MomentumHost {
    let mut host = attached(t0, MomentumCode::Shipped);
    let c = cfg(true);
    let mut seen = Vec::new();
    let (last, cell) = burst(&mut host, t0 + ms(5), 8, &c, &mut seen);
    let parked = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
    assert!(!parked.stranded, "fixture: the shipped glow clears");
    host
}

fn with_log(
    trace: &MomentumTrace,
    schema: &MomentumSchema,
    log: &EventLog<MomentumSchema>,
) -> MomentumTrace {
    MomentumTrace {
        log: log.encode(schema).expect("fixture: encodes"),
        ..trace.clone()
    }
}

#[test]
fn a_trace_file_round_trips_and_replays_on_another_epoch_to_the_recorded_head() {
    let t0 = Instant::now();
    let host = typed(t0);
    let trace = host.aos_trace().expect("attached");
    let b = &hosted(&host).boundary;
    let log = b.log().expect("attached");
    assert_eq!(
        trace.head_state,
        aterm_aos::task::state_digest(b.schema(), &b.pair().state)
    );
    let file = trace.encode();
    assert_eq!(file.get(..8), Some(&TRACE_FILE_MAGIC[..]));
    assert_eq!(MomentumTrace::decode(&file), Ok(trace.clone()));
    let counts = EventCounts::of(log.inputs());
    assert_eq!(counts.keys, 8);
    assert_eq!(counts.resets, 0);
    assert_eq!(counts.total(), log.len());
    for epoch in [t0, t0 + ms(3_600_000), Instant::now()] {
        let r = trace.replay(epoch, BUILD).expect("the trace replays");
        assert_eq!(r.state_digest, trace.head_state);
        assert_eq!(r.observation_chain, trace.head_chain);
        assert_eq!(r.code, MomentumCode::Shipped);
        assert_eq!(r.checkpoint, b.checkpoint().expect("attached").digest());
        assert_eq!((r.log_epoch, r.first_cursor), (1, 0));
        assert_eq!(r.counts, counts);
    }
}

#[test]
fn a_trace_file_cut_or_altered_anywhere_is_refused() {
    let t0 = Instant::now();
    let trace = typed(t0).aos_trace().expect("attached");
    let file = trace.encode();
    for cut in 0..file.len() {
        let refused = MomentumTrace::decode(&file[..cut]);
        assert!(refused.is_err(), "a cut at {cut} of {} decoded", file.len());
    }
    for at in 0..file.len() {
        let mut bent = file.clone();
        bent[at] ^= 0x20;
        let refused = MomentumTrace::decode(&bent).expect_err("an altered byte decoded");
        match at {
            0..7 => assert_eq!(refused, TraceFault::Magic, "byte {at}"),
            7 => assert_eq!(refused, TraceFault::Format { found: 0x21 }),
            _ => assert!(
                matches!(refused, TraceFault::Seal { .. }),
                "byte {at}: {refused:?}"
            ),
        }
    }
    assert_eq!(MomentumTrace::decode(&file[..7]), Err(TraceFault::Magic));
    assert_eq!(
        MomentumTrace::decode(&file[..39]),
        Err(TraceFault::Wire(WireFault::Truncated {
            field: "trace.seal",
            need: 32,
            have: 31,
        }))
    );
    let mut longer = file.clone();
    longer.push(0);
    assert!(matches!(
        MomentumTrace::decode(&longer),
        Err(TraceFault::Seal { .. })
    ));
}

#[test]
fn a_replay_that_misses_the_recorded_head_is_refused_naming_both() {
    let t0 = Instant::now();
    let trace = typed(t0).aos_trace().expect("attached");
    let other = Digest::of(b"trace-test\0", &[b"another head"]);
    let bent = MomentumTrace {
        head_state: other,
        ..trace.clone()
    };
    assert_eq!(
        bent.replay(t0, BUILD),
        Err(TraceFault::State {
            recorded: other,
            replayed: trace.head_state,
        })
    );
    let bent = MomentumTrace {
        head_chain: other,
        ..trace.clone()
    };
    assert_eq!(
        bent.replay(t0, BUILD),
        Err(TraceFault::Chain {
            recorded: other,
            replayed: trace.head_chain,
        })
    );
    assert_eq!(
        trace.replay(t0, "another-build"),
        Err(TraceFault::UnknownCode {
            name: "momentum-glow.shipped".into(),
            build: "another-build".into(),
        })
    );
}

#[test]
fn a_trace_whose_log_is_unsealed_broken_or_of_another_checkpoint_is_refused() {
    let t0 = Instant::now();
    let host = typed(t0);
    let trace = host.aos_trace().expect("attached");
    let b = &hosted(&host).boundary;
    let schema = b.schema();
    let log = b.log().expect("attached");

    let cut = trace.log.len() - seal_record_len(&trace);
    let unsealed = MomentumTrace {
        log: trace.log[..cut].to_vec(),
        ..trace.clone()
    };
    assert_eq!(
        unsealed.read(t0).err(),
        Some(TraceFault::Tail(Tail::Unsealed { events: log.len() }))
    );

    let mut broken = EventLog::<MomentumSchema>::new(log.header().clone(), 1 << 16);
    for ev in log.inputs() {
        broken.push(*ev).expect("room");
    }
    broken.mark_broken("trace-test: an input went missing");
    let broken = with_log(&trace, schema, &broken);
    assert!(broken.read(t0).is_ok());
    assert_eq!(
        broken.replay(t0, BUILD),
        Err(TraceFault::Broken {
            why: "the log was written with a recording break"
        })
    );

    let other = typed_other(t0);
    let paired = MomentumTrace {
        checkpoint: other.checkpoint.clone(),
        ..trace.clone()
    };
    let (ca, cb) = (
        aterm_aos::checkpoint::Checkpoint::decode(&trace.checkpoint)
            .expect("decodes")
            .digest(),
        aterm_aos::checkpoint::Checkpoint::decode(&other.checkpoint)
            .expect("decodes")
            .digest(),
    );
    assert_ne!(ca, cb);
    assert_eq!(
        paired.read(t0).err(),
        Some(TraceFault::Log(LogFault::Ancestor {
            expected: cb,
            found: ca,
        }))
    );
}

fn typed_other(t0: Instant) -> MomentumTrace {
    let mut glow = MomentumHost::default();
    glow.on_key(t0 + ms(1), super::test_fixtures::TAU);
    glow.aos_attach(t0, MomentumCode::Shipped, BUILD, 1);
    glow.on_key(t0 + ms(70), super::test_fixtures::TAU);
    glow.aos_trace().expect("attached")
}

fn seal_record_len(trace: &MomentumTrace) -> usize {
    let schema = MomentumSchema::new(Instant::now());
    let ancestor = aterm_aos::checkpoint::Checkpoint::decode(&trace.checkpoint)
        .expect("decodes")
        .digest();
    let (whole, _) = EventLog::decode(&schema, &trace.log, &trace.identity, &ancestor, 1 << 16)
        .expect("decodes");
    (1..trace.log.len())
        .find(|&k| {
            matches!(
                EventLog::decode(
                    &schema,
                    &trace.log[..trace.log.len() - k],
                    &trace.identity,
                    &ancestor,
                    1 << 16
                ),
                Ok((_, Tail::Unsealed { events })) if events == whole.len()
            )
        })
        .expect("a sealless prefix decodes")
}

#[test]
fn right_after_a_commit_the_trace_is_the_new_checkpoint_with_the_empty_chain() {
    let t0 = Instant::now();
    let mut host = typed(t0);
    let end = t0 + ms(5_000);
    host.aos_pause(end, LANE).expect("pauses");
    host.aos_repair(MomentumCode::Shipped).expect("commits");
    let trace = host.aos_trace().expect("attached");
    assert_eq!(trace.head_chain, empty_chain());
    assert_eq!(
        host.aos_status().observation_chain,
        Some(empty_chain().short()),
        "the status reads the chain of the fresh log, not the last one folded"
    );
    let r = trace.replay(t0, BUILD).expect("the fresh log replays");
    assert_eq!(r.counts, EventCounts::default());
    assert_eq!(r.state_digest, trace.head_state);
    let _ = host.next_change_deadline(end + ms(1), LANE);
    let trace = host.aos_trace().expect("attached");
    assert_ne!(trace.head_chain, empty_chain());
    let r = trace.replay(t0, BUILD).expect("replays");
    assert_eq!(r.observation_chain, trace.head_chain);
    assert_eq!(
        r.counts,
        EventCounts {
            queries: 1,
            ..EventCounts::default()
        }
    );
    assert!(matches!(
        hosted(&host).boundary.log().map(|l| l.inputs()),
        Some([MomentumEvent::Query { .. }])
    ));
}

/// A key on `host` at `t0`, then frames 8 ms apart (its largest input), until its live log holds `len` inputs.
fn frames_until(host: &mut MomentumHost, t0: Instant, len: usize) {
    let c = super::MomentumGlowConfig {
        glyph_span: Some((10, 11)),
        ..cfg(false)
    };
    let mut k = 0u64;
    while host.aos_status().log_len < len {
        let at = t0 + ms(8 * k);
        if k == 0 {
            host.key(at, super::test_fixtures::TAU);
        } else {
            let _ = draw(host, Some((4, 10)), at, geom(), &c, 0);
        }
        k += 1;
        assert_eq!(host.aos_retired_waiting(), 0, "fixture: one log epoch");
    }
}

#[test]
fn a_check_refuses_a_log_longer_than_one_host_epoch_before_it_replays() {
    let t0 = Instant::now();
    let mut host = attached_for_lane(t0, Sched::Prebuilt(MomentumCode::Shipped));
    frames_until(&mut host, t0 + ms(5), MOMENTUM_HOST_MAX_EVENTS + 1);
    let long = host.aos_trace().expect("exports");
    assert_eq!(
        long.check(t0, BUILD).err(),
        Some(TraceFault::Log(LogFault::TooManyEvents {
            limit: MOMENTUM_HOST_MAX_EVENTS
        }))
    );
    assert!(
        long.replay(t0, BUILD).is_ok(),
        "negative control: the plain replay keeps its wider limit"
    );
}

#[test]
fn the_largest_trace_a_window_exports_fits_twice_under_the_check_bound() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    frames_until(&mut host, t0 + ms(5), MOMENTUM_HOST_MAX_EVENTS);
    let full = host.aos_trace().expect("exports");
    let bytes = full.encode().len();
    assert!(
        bytes * 2 <= MOMENTUM_TRACE_MAX_BYTES,
        "a full host epoch is {bytes} bytes"
    );
    assert!(full.check(t0, BUILD).is_ok(), "a full host epoch checks");
}
