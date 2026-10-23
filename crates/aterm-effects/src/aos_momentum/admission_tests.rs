// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Attached-mode admission and refusal tests for momentum glow.

use std::time::Duration;

use aterm_aos::task::state_digest;
use aterm_time::Instant;

use super::test_fixtures::{
    attached, burst, cfg, draw, geom, hosted, hosted_mut, ms, replay_log, sentinels,
};
use super::{MomentumCode, MomentumEvent, MomentumHost, MomentumObservation};
use crate::cursor_momentum::{AdmitRefusal, GeometryFault, MOMENTUM_GLOW_TAU_S, MomentumGlowFrame};

fn digest(host: &MomentumHost) -> aterm_aos::Digest {
    let h = hosted(host);
    state_digest(h.boundary.schema(), h.state())
}

/// How a case sends its input to the host, at the instant the burst left off.
type Sender = Box<dyn Fn(&mut MomentumHost, Instant)>;

/// One refused input.
struct Case {
    name: &'static str,
    send: Sender,
    refusal: AdmitRefusal,
}

#[test]
fn every_refusal_in_attached_mode_is_a_total_step_that_replays_identically() {
    let t0 = Instant::now();
    let mut host = attached(t0, MomentumCode::Shipped);
    let c = cfg(true);
    let mut seen = Vec::new();
    let (last, cell) = burst(&mut host, t0, 10, &c, &mut seen);
    assert!(host.is_active(), "fixture: a hot glow");
    let horizon = Duration::from_secs(30 * 24 * 60 * 60);
    let past = horizon + Duration::from_nanos(1);
    let past_ns = past.as_nanos() as i128;
    let tick = move |host: &mut MomentumHost,
                     now: Instant,
                     g: crate::cursor_glow::Geom,
                     k: crate::cursor_momentum::MomentumGlowConfig| {
        let pre = sentinels(2, 0x0033_0000);
        let (mut out, mut under) = (pre.clone(), pre.clone());
        let frame = host.tick(Some(cell), now, g, &k, &mut out, &mut under);
        assert_eq!(
            (&out, &under),
            (&pre, &pre),
            "a refused tick appended or touched a stream"
        );
        assert_eq!(
            frame,
            MomentumGlowFrame::default(),
            "a refused tick shows the inert frame"
        );
    };
    let mut zero_cell = geom();
    zero_cell.cw = 0;
    let mut too_tall = geom();
    too_tall.origin_y = 65_100;
    let mut nan_radius = c;
    nan_radius.radius_cells = f32::NAN;
    let mut inf_intensity = c;
    inf_intensity.intensity = f32::INFINITY;
    let mut inverted = c;
    inverted.glyph_span = Some((5, 2));
    let mut no_tau = c;
    no_tau.tau_s = 0.0;
    let cases: Vec<Case> = vec![
        Case {
            name: "a NaN key tau",
            send: Box::new(|h, now| {
                h.on_key(now, f32::NAN);
            }),
            refusal: AdmitRefusal::Tau(f32::NAN.to_bits()),
        },
        Case {
            name: "a key tau past the profile",
            send: Box::new(|h, now| {
                h.on_key(now, 61.0);
            }),
            refusal: AdmitRefusal::Tau(61.0f32.to_bits()),
        },
        Case {
            name: "a key past the horizon, delivered from the queue",
            send: Box::new(move |h, _| {
                let key = MomentumEvent::Key {
                    at: past_ns,
                    tau_bits: MOMENTUM_GLOW_TAU_S.to_bits(),
                };
                let _ = hosted_mut(h).deliver(&key, false, &mut Vec::new(), &mut Vec::new());
            }),
            refusal: AdmitRefusal::Horizon(past_ns),
        },
        Case {
            name: "a tick with a zero-width cell",
            send: Box::new(move |h, now| tick(h, now, zero_cell, c)),
            refusal: AdmitRefusal::Geometry(GeometryFault::CellWidthZero),
        },
        Case {
            name: "a tick whose grid passes the pixel extent",
            send: Box::new(move |h, now| tick(h, now, too_tall, c)),
            refusal: AdmitRefusal::Geometry(GeometryFault::GridHeight {
                origin_y: 65_100,
                rows: 24,
                ch: 20,
                limit: 65_535,
            }),
        },
        Case {
            name: "a tick with a NaN radius",
            send: Box::new(move |h, now| tick(h, now, geom(), nan_radius)),
            refusal: AdmitRefusal::Radius(f32::NAN.to_bits()),
        },
        Case {
            name: "a tick with an infinite intensity",
            send: Box::new(move |h, now| tick(h, now, geom(), inf_intensity)),
            refusal: AdmitRefusal::Intensity(f32::INFINITY.to_bits()),
        },
        Case {
            name: "a tick with an inverted glyph span",
            send: Box::new(move |h, now| tick(h, now, geom(), inverted)),
            refusal: AdmitRefusal::GlyphSpan(5, 2),
        },
        Case {
            name: "a tick with a zero tau",
            send: Box::new(move |h, now| tick(h, now, geom(), no_tau)),
            refusal: AdmitRefusal::Tau(0.0f32.to_bits()),
        },
        Case {
            name: "a query with a frame interval past the profile",
            send: Box::new(|h, now| {
                let lane = Duration::from_secs(2);
                assert_eq!(
                    h.next_change_deadline(now, lane),
                    Some(now + lane),
                    "a refused query keeps a hot glow on the frame cadence"
                );
            }),
            refusal: AdmitRefusal::FrameInterval(Duration::from_secs(2)),
        },
    ];
    let mut expected = Vec::new();
    for case in &cases {
        let before = digest(&host);
        let cursor = hosted(&host).cursor();
        (case.send)(&mut host, last);
        assert_eq!(digest(&host), before, "{}: the state moved", case.name);
        assert_eq!(
            hosted(&host).cursor(),
            cursor + 1,
            "{}: not recorded once",
            case.name
        );
        expected.push((cursor, case.name, case.refusal));
    }
    assert!(matches!(
        draw(&mut host, Some(cell), last + ms(1), geom(), &c, 0),
        super::test_fixtures::Seen::Frame { frame, .. } if frame.hot
    ));
    let run = replay_log(&host, MomentumCode::Shipped);
    for (cursor, name, refusal) in expected {
        let found = run
            .observations
            .iter()
            .find(|(c, _)| *c == cursor)
            .map(|(_, o)| o.clone());
        assert_eq!(found, Some(MomentumObservation::Refused(refusal)), "{name}");
    }
    let h = hosted(&host);
    assert_eq!(
        run.state_digest,
        state_digest(h.boundary.schema(), h.state())
    );
    assert_eq!(run.observation_chain, h.chain.chain);
}

#[test]
fn an_attached_tick_reserves_its_budget_on_both_vectors_before_it_runs() {
    let t0 = Instant::now();
    for dark in [true, false] {
        let mut host = attached(t0, MomentumCode::Shipped);
        let c = cfg(dark);
        host.on_key(t0, MOMENTUM_GLOW_TAU_S);
        for _ in 0..9 {
            host.on_key(t0, MOMENTUM_GLOW_TAU_S);
        }
        let budget = crate::cursor_momentum::admit_tick(
            &crate::cursor_momentum::MOMENTUM_PROFILE,
            Some((4, 10)),
            &geom(),
            &c,
        )
        .expect("admitted");
        let (mut out, mut under) = (sentinels(3, 7), sentinels(3, 9));
        let frame = host.tick(Some((4, 10)), t0, geom(), &c, &mut out, &mut under);
        assert!(frame.hot);
        assert!(out.capacity() >= 3 + budget.out && under.capacity() >= 3 + budget.under);
        assert!(out.len() - 3 <= budget.out && under.len() - 3 <= budget.under);
        assert!(out.len() + under.len() > 6, "dark={dark}: the tick drew");
    }
}
