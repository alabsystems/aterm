// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for 12-byte schedule plans, prebuilt-code equivalence, and presentation fallback.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::NoEffects;
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::replay::{Run, replay};
use aterm_aos::task::state_digest;
use aterm_aos::wire::Enc;
use aterm_aos::{ArtifactId, ArtifactKind, Digest};
use aterm_time::Instant;

use super::plan::{
    DEMO_PLAN, MAX_GAP_US, MAX_PERIOD_US, MIN_PERIOD_US, PLAN_ARTIFACT_NAME, PLAN_DOMAIN,
    PLAN_FORMAT, PLAN_INTERPRETER, PLAN_LEN, PLAN_NAME, PlanField, PlanRefusal, SHIPPED_PLAN,
    SchedulePlan, Split, Wake,
};
use super::test_fixtures::{
    BUILD, Call, LANE, Seen, attached, burst, cfg, draw, geom, hosted, hosted_mut, ms, play,
    random_stream, run_lane, strand,
};
use super::{
    HOT_HAS_WAKE, MOMENTUM_HOST_MAX_EVENTS, MomentumCode, MomentumHost, MomentumSchema,
    MomentumTrace, NoStrandedLight, PrebuiltMomentum, Sched, TraceFault, momentum_identity,
    target_name,
};
use crate::cursor_glow::Geom;
use crate::cursor_momentum::MomentumGlowConfig;

fn planned_host(t0: Instant, plan: SchedulePlan) -> MomentumHost {
    let mut h = MomentumHost::default();
    h.attach_planned(t0, plan, BUILD, 1, MOMENTUM_HOST_MAX_EVENTS);
    h
}

fn replay_with(h: &MomentumHost, code: &PrebuiltMomentum) -> Run<MomentumSchema> {
    let b = &hosted(h).boundary;
    let checkpoint = b.checkpoint().expect("fixture: attached");
    let log = b.log().expect("fixture: attached");
    let start = checkpoint
        .restore(b.schema(), b.identity())
        .expect("the ancestor restores");
    replay(
        b.schema(),
        code,
        start,
        log.inputs(),
        log.header().first_cursor,
        &mut NoEffects,
    )
}

fn plan(split: Split, wake: Wake) -> SchedulePlan {
    SchedulePlan::new(split, wake).expect("fixture: an in-range plan")
}

fn splits() -> Vec<Split> {
    let mut v = vec![Split::Shipped];
    v.extend(
        [0, 1, 8_333, 16_667, 250_000, 999_999, MAX_GAP_US]
            .into_iter()
            .map(|gap_us| Split::SettleAtStep { gap_us }),
    );
    v
}

fn wakes() -> Vec<Wake> {
    let mut v = vec![Wake::NextStep, Wake::ColdOnly, Wake::Cadence, Wake::Never];
    v.extend(
        [MIN_PERIOD_US, 1_001, 16_667, 999_999, MAX_PERIOD_US]
            .into_iter()
            .map(|period_us| Wake::Poll { period_us }),
    );
    v
}

fn grid() -> Vec<SchedulePlan> {
    let splits = [
        Split::Shipped,
        Split::SettleAtStep { gap_us: 0 },
        Split::SettleAtStep { gap_us: 1 },
        Split::SettleAtStep { gap_us: 16_667 },
        Split::SettleAtStep { gap_us: 100_000 },
        Split::SettleAtStep { gap_us: MAX_GAP_US },
    ];
    let wakes = [
        Wake::NextStep,
        Wake::ColdOnly,
        Wake::Poll {
            period_us: MIN_PERIOD_US,
        },
        Wake::Poll { period_us: 16_667 },
        Wake::Poll {
            period_us: MAX_PERIOD_US,
        },
        Wake::Cadence,
        Wake::Never,
    ];
    splits
        .iter()
        .flat_map(|&s| wakes.iter().map(move |&w| plan(s, w)))
        .collect()
}

fn by_the_table(b: &[u8]) -> Result<(Split, Wake), PlanRefusal> {
    if b.len() != 12 {
        return Err(PlanRefusal::Length {
            expected: 12,
            found: b.len(),
        });
    }
    if b[0] != 1 {
        return Err(PlanRefusal::Format { found: b[0] });
    }
    if b[1] > 1 {
        return Err(PlanRefusal::SplitTag { tag: b[1] });
    }
    if b[2] > 4 {
        return Err(PlanRefusal::WakeTag { tag: b[2] });
    }
    if b[3] != 0 {
        return Err(PlanRefusal::Reserved { found: b[3] });
    }
    let gap =
        u32::from(b[4]) | u32::from(b[5]) << 8 | u32::from(b[6]) << 16 | u32::from(b[7]) << 24;
    let period =
        u32::from(b[8]) | u32::from(b[9]) << 8 | u32::from(b[10]) << 16 | u32::from(b[11]) << 24;
    let split = match (b[1], gap) {
        (0, 0) => Split::Shipped,
        (0, value) => {
            return Err(PlanRefusal::Unused {
                field: PlanField::Gap,
                value,
            });
        }
        (_, gap_us) if gap_us <= 1_000_000 => Split::SettleAtStep { gap_us },
        (_, gap_us) => return Err(PlanRefusal::Gap { gap_us }),
    };
    let wake = match (b[2], period) {
        (2, period_us) if (1_000..=1_000_000).contains(&period_us) => Wake::Poll { period_us },
        (2, period_us) => return Err(PlanRefusal::Period { period_us }),
        (_, value) if value != 0 => {
            return Err(PlanRefusal::Unused {
                field: PlanField::Period,
                value,
            });
        }
        (0, _) => Wake::NextStep,
        (1, _) => Wake::ColdOnly,
        (3, _) => Wake::Cadence,
        _ => Wake::Never,
    };
    Ok((split, wake))
}

fn agrees(b: &[u8]) -> Result<SchedulePlan, PlanRefusal> {
    let decoded = SchedulePlan::decode(b);
    assert_eq!(
        decoded.map(|p| (p.split(), p.wake())),
        by_the_table(b),
        "{b:02x?}"
    );
    if let Ok(p) = decoded {
        assert_eq!(
            &p.encode()[..],
            b,
            "an accepted plan re-encodes to its bytes"
        );
    }
    if let Err(r) = decoded {
        assert!(!r.to_string().is_empty());
    }
    decoded
}

fn seen_but_cadence(s: &Seen) -> Seen {
    match s {
        Seen::Schedule {
            is_active,
            settling,
            cold_at,
            deadline,
        } => Seen::Schedule {
            is_active: *is_active || *settling,
            settling: false,
            cold_at: *cold_at,
            deadline: *deadline,
        },
        other => other.clone(),
    }
}

fn assert_plan_is_code(planned: SchedulePlan, code: MomentumCode) {
    let t0 = Instant::now();
    let interpreter = PrebuiltMomentum::planned(planned, BUILD);
    let prebuilt = PrebuiltMomentum::new(code, BUILD);
    let mut steps = 0usize;
    for seed in 1..=48u64 {
        let calls = random_stream(seed, 300, true);
        let mut by_code = attached(t0, code);
        let mut by_plan = planned_host(t0, planned);
        let seen_code = play(&mut by_code, t0, &calls);
        let seen_plan = play(&mut by_plan, t0, &calls);
        assert_eq!(
            seen_code.iter().map(seen_but_cadence).collect::<Vec<_>>(),
            seen_plan.iter().map(seen_but_cadence).collect::<Vec<_>>(),
            "seed {seed}: the host is shown the same frames and wakes"
        );
        let (hc, hp) = (hosted(&by_code), hosted(&by_plan));
        let inputs = hc.boundary.log().expect("attached").inputs();
        assert_eq!(
            inputs,
            hp.boundary.log().expect("attached").inputs(),
            "seed {seed}: the same inputs are recorded"
        );
        let run_code = replay_with(&by_code, &prebuilt);
        let run_plan = replay_with(&by_code, &interpreter);
        assert_eq!(run_code.refused, None, "seed {seed}");
        assert_eq!(run_plan.refused, None, "seed {seed}");
        assert_eq!(run_code.observations.len(), inputs.len());
        for (i, (a, b)) in run_code
            .observations
            .iter()
            .zip(&run_plan.observations)
            .enumerate()
        {
            assert_eq!(a, b, "seed {seed}, step {i}: {:?}", inputs[i]);
        }
        assert_eq!(run_code.observations.len(), run_plan.observations.len());
        assert_eq!(run_code.state_digest, run_plan.state_digest, "seed {seed}");
        assert_eq!(
            run_code.observation_chain, run_plan.observation_chain,
            "seed {seed}"
        );
        let live_code = state_digest(hc.boundary.schema(), hc.state());
        let live_plan = state_digest(hp.boundary.schema(), hp.state());
        assert_eq!(live_code, run_code.state_digest, "seed {seed}");
        assert_eq!(live_plan, run_plan.state_digest, "seed {seed}");
        assert_eq!(hc.chain.chain, run_code.observation_chain, "seed {seed}");
        assert_eq!(hp.chain.chain, run_plan.observation_chain, "seed {seed}");
        assert_eq!(
            replay_with(&by_plan, &interpreter).observation_chain,
            hp.chain.chain,
            "seed {seed}: the planned host's log replays to its live chain"
        );
        steps += inputs.len();
    }
    assert!(steps > 48 * 200, "fixture: long streams ({steps} steps)");
}

fn beside_shipped(
    streams: u64,
    mut check: impl FnMut(SchedulePlan, &MomentumHost, &MomentumHost, &str),
) {
    let t0 = Instant::now();
    for seed in 1..=streams {
        let calls = random_stream(100 + seed, 300, true);
        for p in grid() {
            let mut shipped = attached(t0, MomentumCode::Shipped);
            let mut planned = planned_host(t0, p);
            for (i, call) in calls.iter().enumerate() {
                let a = play(&mut shipped, t0, std::slice::from_ref(call));
                let b = play(&mut planned, t0, std::slice::from_ref(call));
                let at = format!("{p:?}, seed {seed}, call {i}");
                match (a.as_slice(), b.as_slice()) {
                    (
                        [
                            Seen::Schedule {
                                is_active,
                                settling,
                                cold_at,
                                ..
                            },
                        ],
                        [
                            Seen::Schedule {
                                is_active: pa,
                                settling: ps,
                                cold_at: pc,
                                ..
                            },
                        ],
                    ) => {
                        assert_eq!(*is_active || *settling, *pa || *ps, "{at}: hot");
                        assert_eq!(cold_at, pc, "{at}: cold_at");
                    }
                    _ => assert_eq!(a, b, "{at}: the key, reset or frame and its quads"),
                }
                check(p, &planned, &shipped, &at);
            }
        }
    }
}

#[test]
fn the_plan_bytes_are_the_table() {
    assert_eq!((PLAN_FORMAT, PLAN_LEN), (1, 12));
    assert_eq!(PLAN_DOMAIN, b"aterm-aos-m4-plan-v1\0");
    assert_eq!(SHIPPED_PLAN.encode(), [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(DEMO_PLAN.encode(), [1, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        (SHIPPED_PLAN.split(), SHIPPED_PLAN.wake()),
        (Split::Shipped, Wake::NextStep)
    );
    assert_eq!(
        (DEMO_PLAN.split(), DEMO_PLAN.wake()),
        (Split::Shipped, Wake::Never)
    );
    let p = plan(
        Split::SettleAtStep { gap_us: 999_999 },
        Wake::Poll { period_us: 16_667 },
    );
    assert_eq!(
        p.encode(),
        [1, 1, 2, 0, 0x3f, 0x42, 0x0f, 0, 0x1b, 0x41, 0, 0]
    );
    assert_eq!(plan(Split::Shipped, Wake::ColdOnly).encode()[2], 1);
    assert_eq!(plan(Split::Shipped, Wake::Cadence).encode()[2], 3);
    for q in [SHIPPED_PLAN, DEMO_PLAN, p] {
        assert_eq!(q.digest(), Digest::of(PLAN_DOMAIN, &[&q.encode()]));
        assert_eq!(SchedulePlan::decode(&q.encode()), Ok(q));
    }
    assert_ne!(SHIPPED_PLAN.digest(), DEMO_PLAN.digest());
    assert_eq!(
        SchedulePlan::new(
            Split::SettleAtStep {
                gap_us: MAX_GAP_US + 1
            },
            Wake::NextStep
        ),
        Err(PlanRefusal::Gap {
            gap_us: MAX_GAP_US + 1
        })
    );
    assert_eq!(
        SchedulePlan::new(Split::Shipped, Wake::Poll { period_us: 999 }),
        Err(PlanRefusal::Period { period_us: 999 })
    );
    assert_eq!(
        SchedulePlan::new(
            Split::Shipped,
            Wake::Poll {
                period_us: MAX_PERIOD_US + 1
            }
        ),
        Err(PlanRefusal::Period {
            period_us: MAX_PERIOD_US + 1
        })
    );
}

#[test]
fn every_malformed_byte_is_refused_naming_its_field() {
    let mut named: BTreeSet<(usize, &str)> = BTreeSet::new();
    let mut bases = 0;
    for &s in &splits() {
        for &w in &wakes() {
            let base = plan(s, w).encode();
            bases += 1;
            for at in 0..PLAN_LEN {
                for v in 0..=u8::MAX {
                    let mut b = base;
                    b[at] = v;
                    if let Err(r) = agrees(&b) {
                        named.insert((at, r.field().name()));
                    }
                }
            }
        }
    }
    assert_eq!(bases, splits().len() * wakes().len());
    for (at, field) in [
        (0, "format"),
        (1, "split"),
        (2, "wake"),
        (3, "reserved"),
        (4, "gap_us"),
        (7, "gap_us"),
        (8, "period_us"),
        (11, "period_us"),
    ] {
        assert!(
            named.contains(&(at, field)),
            "a change at byte {at} is refused naming {field}: {named:?}"
        );
    }
    assert_eq!(
        SchedulePlan::decode(&[1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]),
        Err(PlanRefusal::Unused {
            field: PlanField::Gap,
            value: 1
        })
    );
    assert_eq!(
        SchedulePlan::decode(&[1, 0, 3, 0, 0, 0, 0, 0, 0, 1, 0, 0]),
        Err(PlanRefusal::Unused {
            field: PlanField::Period,
            value: 256
        })
    );
    assert_eq!(
        SchedulePlan::decode(&[1, 2, 9, 7, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]),
        Err(PlanRefusal::SplitTag { tag: 2 })
    );
    assert_eq!(
        SchedulePlan::decode(&[1, 1, 9, 7, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]),
        Err(PlanRefusal::WakeTag { tag: 9 })
    );
    assert_eq!(
        SchedulePlan::decode(&[1, 1, 2, 7, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]),
        Err(PlanRefusal::Reserved { found: 7 })
    );
    assert_eq!(
        SchedulePlan::decode(&[1, 1, 2, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0]),
        Err(PlanRefusal::Gap { gap_us: u32::MAX })
    );
    assert_eq!(
        SchedulePlan::decode(&[1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        Err(PlanRefusal::Period { period_us: 0 })
    );
    assert_eq!(
        SchedulePlan::decode(&[0; 12]),
        Err(PlanRefusal::Format { found: 0 })
    );
    let long: Vec<u8> = SHIPPED_PLAN.encode().into_iter().chain([0]).collect();
    for len in 0..=40 {
        let b: Vec<u8> = long.iter().copied().cycle().take(len).collect();
        if len == PLAN_LEN {
            continue;
        }
        assert_eq!(
            agrees(&b),
            Err(PlanRefusal::Length {
                expected: PLAN_LEN,
                found: len
            })
        );
    }
    assert_eq!(
        SchedulePlan::decode(&long).map_err(|r| r.field()),
        Err(PlanField::Length)
    );
}

#[test]
fn every_decodable_plan_re_encodes_to_its_bytes() {
    let gaps = [0, 1, 999_999, MAX_GAP_US, MAX_GAP_US + 1, u32::MAX];
    let periods = [
        0,
        1,
        999,
        MIN_PERIOD_US,
        1_001,
        999_999,
        MAX_PERIOD_US,
        MAX_PERIOD_US + 1,
        u32::MAX,
    ];
    let mut accepted = Vec::new();
    let mut tried = 0;
    for format in [0u8, 1, 2] {
        for split in [0u8, 1, 2, 255] {
            for wake in [0u8, 1, 2, 3, 4, 5, 255] {
                for reserved in [0u8, 1] {
                    for gap in gaps {
                        for period in periods {
                            let mut b = [0u8; PLAN_LEN];
                            b[0] = format;
                            b[1] = split;
                            b[2] = wake;
                            b[3] = reserved;
                            b[4..8].copy_from_slice(&gap.to_le_bytes());
                            b[8..12].copy_from_slice(&period.to_le_bytes());
                            tried += 1;
                            if let Ok(p) = agrees(&b) {
                                assert_eq!(SchedulePlan::new(p.split(), p.wake()), Ok(p));
                                accepted.push(p);
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(tried, 3 * 4 * 7 * 2 * gaps.len() * periods.len());
    assert_eq!(accepted.len(), 5 * 8);
    let digests: BTreeSet<Digest> = accepted.iter().map(SchedulePlan::digest).collect();
    assert_eq!(digests.len(), accepted.len());
    let bytes: BTreeSet<[u8; PLAN_LEN]> = accepted.iter().map(SchedulePlan::encode).collect();
    assert_eq!(bytes.len(), accepted.len());
}

#[test]
fn the_shipped_plan_is_the_shipped_engine() {
    const RECORDED: &[&str] = &["aarch64-macos"];
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/aos-momentum")
        .join(format!("{}.trace", target_name()));
    match std::fs::read(&path) {
        Ok(golden) => {
            let trace = MomentumTrace::decode(&golden).expect("the golden trace decodes");
            let read = trace
                .read(Instant::now() + Duration::from_secs(3))
                .expect("and reads on another epoch");
            let start = read
                .checkpoint
                .restore(&read.schema, &trace.identity)
                .expect("its checkpoint restores");
            let first = read.log.header().first_cursor;
            let inputs = read.log.inputs();
            let run = |code: &PrebuiltMomentum| {
                replay(&read.schema, code, start, inputs, first, &mut NoEffects)
            };
            let shipped = run(&PrebuiltMomentum::new(MomentumCode::Shipped, BUILD));
            let planned = run(&PrebuiltMomentum::planned(SHIPPED_PLAN, BUILD));
            assert!(inputs.len() > 20, "fixture: the story is a trace");
            assert_eq!(shipped.refused, None);
            assert_eq!(planned.refused, None);
            assert_eq!(shipped.observations, planned.observations);
            assert_eq!(
                (shipped.state_digest, shipped.observation_chain),
                (trace.head_state, trace.head_chain),
                "the shipped code reaches the recorded head"
            );
            assert_eq!(
                (planned.state_digest, planned.observation_chain),
                (trace.head_state, trace.head_chain),
                "the shipped plan reaches the recorded head"
            );
        }
        Err(e) => assert!(
            !RECORDED.contains(&target_name().as_str()),
            "missing momentum trace {}: {e}",
            path.display()
        ),
    }
    assert_plan_is_code(SHIPPED_PLAN, MomentumCode::Shipped);
}

#[test]
fn the_demo_plan_is_the_demo_fault_and_fails_hot_has_wake() {
    assert_plan_is_code(DEMO_PLAN, MomentumCode::DemoStrandedSettle);
    let t0 = Instant::now();
    for dark in [true, false] {
        let (by_code, at_code, _) =
            strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, dark);
        let (mut by_plan, at_plan, _) = strand(planned_host(t0, DEMO_PLAN), t0, dark);
        assert_eq!(at_code, at_plan, "dark={dark}: both lanes park at once");
        let (hc, hp) = (hosted(&by_code), hosted(&by_plan));
        let inputs = hp.boundary.log().expect("attached").inputs();
        assert_eq!(hc.boundary.log().expect("attached").inputs(), inputs);
        let schema = hp.boundary.schema();
        let judge = |run: &Run<MomentumSchema>| NoStrandedLight::new().judge(schema, inputs, run);
        let fault = judge(&replay_with(
            &by_code,
            &PrebuiltMomentum::new(MomentumCode::DemoStrandedSettle, BUILD),
        ));
        let planned = judge(&replay_with(
            &by_plan,
            &PrebuiltMomentum::planned(DEMO_PLAN, BUILD),
        ));
        assert!(
            matches!(
                planned,
                Verdict::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            ),
            "dark={dark}: {planned:?}"
        );
        assert_eq!(
            planned, fault,
            "dark={dark}: the same failure, at the same cursor"
        );
        assert_eq!(
            judge(&replay_with(
                &by_plan,
                &PrebuiltMomentum::planned(SHIPPED_PLAN, BUILD)
            )),
            Verdict::Pass,
            "dark={dark}: the shipped plan passes the same trace"
        );

        let head = by_plan.aos_pause(at_plan, LANE).expect("pauses");
        let h = hosted_mut(&mut by_plan);
        let prepared = h
            .boundary
            .prepare(PrebuiltMomentum::new(MomentumCode::Shipped, BUILD))
            .expect("the shipped candidate passes against the stranded plan");
        let w = prepared.witness();
        assert!(
            matches!(
                w.live_verdict(),
                Verdict::Fail {
                    law: HOT_HAS_WAKE,
                    ..
                }
            ),
            "dark={dark}: {:?}",
            w.live_verdict()
        );
        assert_eq!(w.live_code().name, PLAN_ARTIFACT_NAME);
        assert_eq!(w.candidate_verdict(), &Verdict::Pass);
        assert!(
            w.same_code_reproduced(),
            "the planned live run is its replay"
        );
        assert_eq!(w.grade(), EvidenceGrade::Tested);
        assert_eq!(w.head(), &head);
    }
}

#[test]
fn no_plan_changes_glow_state() {
    let mut calls = 0usize;
    beside_shipped(3, |p, planned, shipped, at| {
        let (hp, hs) = (hosted(planned), hosted(shipped));
        assert_eq!(
            state_digest(hp.boundary.schema(), hp.state()),
            state_digest(hs.boundary.schema(), hs.state()),
            "{at}: the state"
        );
        assert_eq!(
            hp.boundary.log().map(|l| l.len()),
            hs.boundary.log().map(|l| l.len()),
            "{at}: the same inputs recorded"
        );
        assert_eq!(planned.cold_at(), shipped.cold_at(), "{at}: cold_at");
        assert_eq!(hp.sched(), Sched::Plan(p));
        calls += 1;
    });
    assert_eq!(calls, 3 * grid().len() * 300);
}

#[test]
fn every_plan_keeps_active_or_settling_equal_to_hot() {
    let (mut hot, mut settled_early) = (0usize, 0usize);
    beside_shipped(3, |p, planned, shipped, at| {
        let s = hosted(planned).state();
        let sched = Sched::Plan(p);
        let (a, b) = (sched.is_active(s), sched.settling(s));
        assert!(!(a && b), "{at}: the plan's predicates are disjoint");
        assert_eq!(a || b, s.glow.hot(), "{at}: the plan's union is hot");
        let (ha, hb) = (planned.is_active(), planned.settling());
        assert!(!(ha && hb), "{at}: the host's predicates are disjoint");
        assert_eq!(ha || hb, s.glow.hot(), "{at}: the host's union is hot");
        assert_eq!(
            ha || hb,
            shipped.is_active() || shipped.settling(),
            "{at}: hot is the shipped code's hot"
        );
        hot += usize::from(s.glow.hot());
        settled_early += usize::from(b && shipped.is_active());
    });
    assert!(hot > 1000, "fixture: the streams heat the glow ({hot})");
    assert!(
        settled_early > 0,
        "fixture: some plan settles a frame the shipped code keeps active"
    );
}

#[test]
fn a_presentation_change_holds_cadence_for_a_plan() {
    let t0 = Instant::now();
    let eager = plan(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep);
    for dark in [true, false] {
        let mut host = planned_host(t0, eager);
        let c = cfg(dark);
        let mut now = t0;
        for k in 0..6 {
            now = t0 + ms(60 * k);
            host.on_key(now, c.tau_s);
        }
        let firings = |h: &MomentumHost| hosted(h).guard.firings;
        let tick = |h: &mut MomentumHost,
                    now: Instant,
                    cell: (u16, u16),
                    g: Geom,
                    c: &MomentumGlowConfig| {
            let _ = draw(h, Some(cell), now, g, c, 1);
            let s = hosted(h).state();
            assert!(s.glow.hot(), "fixture: the glow is hot");
            assert!(
                !Sched::Plan(eager).is_active(s),
                "fixture: the plan alone settles every frame (the trace records its answer)"
            );
        };
        tick(&mut host, now, (4, 10), geom(), &c);
        assert!(host.is_active() && !host.settling(), "dark={dark}");
        assert_eq!(firings(&host), 1);
        now += ms(16);
        tick(&mut host, now, (4, 10), geom(), &c);
        assert!(!host.is_active() && host.settling(), "dark={dark}");
        assert_eq!(firings(&host), 1);
        let mut moved_geom = geom();
        moved_geom.win_w += 10;
        let mut moved_radius = c;
        moved_radius.radius_cells = 1.0;
        let mut moved_hue = c;
        moved_hue.hue = 0x00FF_4FA0;
        let moves: [((u16, u16), Geom, MomentumGlowConfig); 4] = [
            ((4, 11), geom(), c),
            ((4, 11), moved_geom, c),
            ((4, 11), moved_geom, moved_radius),
            ((4, 11), moved_geom, moved_hue),
        ];
        for (i, (cell, g, cf)) in moves.into_iter().enumerate() {
            now += ms(16);
            tick(&mut host, now, cell, g, &cf);
            assert!(
                host.is_active() && !host.settling(),
                "dark={dark}, move {i}: a moved presentation keeps the cadence"
            );
            assert_eq!(firings(&host), 2 + i as u64);
            now += ms(16);
            tick(&mut host, now, cell, g, &cf);
            assert!(
                !host.is_active() && host.settling(),
                "dark={dark}, move {i}: a still one is the plan's"
            );
            assert_eq!(firings(&host), 2 + i as u64);
        }
    }

    let mut shipped = attached(t0, MomentumCode::Shipped);
    let mut planned = planned_host(t0, eager);
    let mut c = cfg(false);
    for k in 0..8 {
        shipped.on_key(t0 + ms(50 * k), c.tau_s);
        planned.on_key(t0 + ms(50 * k), c.tau_s);
    }
    let hues = [0x0050_FA7B, 0x00FF_4FA0, 0x0040_80FF];
    for k in 0..24u64 {
        let now = t0 + ms(400 + 16 * k);
        c.hue = hues[usize::try_from(k).unwrap_or(0) % hues.len()];
        let a = draw(&mut shipped, Some((4, 10)), now, geom(), &c, 0);
        let b = draw(&mut planned, Some((4, 10)), now, geom(), &c, 0);
        assert_eq!(a, b, "frame {k}");
        assert!(
            shipped.is_active(),
            "frame {k}: the shipped hue moves the fingerprint"
        );
        assert!(
            planned.is_active() && !planned.settling(),
            "frame {k}: a moving hue keeps the plan's cadence"
        );
    }
}

#[test]
fn the_status_reads_the_cadence_the_host_runs() {
    let t0 = Instant::now();
    let eager = plan(Split::SettleAtStep { gap_us: 0 }, Wake::NextStep);
    let c = cfg(true);
    let mut planned = planned_host(t0, eager);
    let mut shipped = attached(t0, MomentumCode::Shipped);
    let mut now = t0;
    for k in 0..6 {
        now = t0 + ms(60 * k);
        planned.on_key(now, c.tau_s);
        shipped.on_key(now, c.tau_s);
    }
    let mut fired = 0u64;
    let same = |h: &MomentumHost, why: &str| {
        let s = h.aos_status();
        assert_eq!(
            (s.is_active, s.settling, s.hot),
            (h.is_active(), h.settling(), h.is_active() || h.settling()),
            "{why}: the status reads the host's cadence"
        );
    };
    for (i, cell) in [(4, 10), (4, 10), (4, 11), (4, 11), (5, 11)]
        .into_iter()
        .enumerate()
    {
        now += ms(16);
        let a = draw(&mut planned, Some(cell), now, geom(), &c, 1);
        let b = draw(&mut shipped, Some(cell), now, geom(), &c, 1);
        assert_eq!(a, b, "tick {i}: no plan changes the glow");
        assert!(hosted(&planned).state().glow.hot(), "fixture: hot");
        assert!(
            !Sched::Plan(eager).is_active(hosted(&planned).state()),
            "fixture: the plan alone settles every frame"
        );
        let moved = i != 1 && i != 3;
        assert_eq!(
            (planned.is_active(), planned.settling()),
            (moved, !moved),
            "tick {i}: the guard holds the cadence while the cell moves"
        );
        same(&planned, &format!("tick {i}, the plan"));
        same(&shipped, &format!("tick {i}, the shipped code"));
        fired += u64::from(moved);
        let s = planned.aos_status();
        assert_eq!(
            (s.guard_moved, s.guard_firings),
            (Some(moved), Some(fired)),
            "tick {i}: the plan's guard"
        );
        assert!(
            s.to_string()
                .contains(&format!(" guard=moved:{moved},firings:{fired} ")),
            "tick {i}: {s}"
        );
        let (h, s) = (hosted(&shipped), shipped.aos_status());
        assert_eq!((s.guard_moved, s.guard_firings), (None, None));
        assert!(!s.to_string().contains(" guard="), "{s}");
        assert_eq!(
            (s.is_active, s.settling),
            (
                MomentumCode::Shipped.is_active(h.state()),
                MomentumCode::Shipped.settling(h.state())
            ),
            "tick {i}: a prebuilt status is the code's own answer"
        );
    }
}

#[test]
fn the_fallback_never_fires_for_prebuilt_codes() {
    let t0 = Instant::now();
    for &code in MomentumCode::ALL {
        let mut moved = 0usize;
        for seed in 1..=8u64 {
            let calls = random_stream(seed, 300, true);
            let mut host = attached(t0, code);
            for call in &calls {
                let _ = play(&mut host, t0, std::slice::from_ref(call));
                let h = hosted(&host);
                let s = h.state();
                assert_eq!(host.is_active(), code.is_active(s), "{code:?} seed {seed}");
                assert_eq!(host.settling(), code.settling(s), "{code:?} seed {seed}");
                assert_eq!(h.guard.firings, 0, "{code:?} seed {seed}");
                moved += usize::from(h.guard.moved && s.glow.hot());
            }
        }
        assert!(
            moved > 0,
            "{code:?}: fixture: presentations move while the glow is hot"
        );
    }
    let (host, _, _) = strand(attached(t0, MomentumCode::DemoStrandedSettle), t0, true);
    assert_eq!(hosted(&host).guard.firings, 0);
    let mut host = attached(t0, MomentumCode::Shipped);
    let c = cfg(true);
    let mut seen = Vec::new();
    let (last, cell) = burst(&mut host, t0, 12, &c, &mut seen);
    let _ = run_lane(&mut host, last, LANE, cell, &c, &mut seen);
    assert_eq!(hosted(&host).guard.firings, 0);
}

#[test]
fn no_name_reaches_a_plan() {
    for name in [PLAN_ARTIFACT_NAME, PLAN_NAME, "plan", "momentum-glow.plan"] {
        assert_eq!(MomentumCode::from_name(name), None, "{name:?}");
    }
    assert!(
        MomentumCode::ALL
            .iter()
            .all(|c| c.name() != PLAN_NAME && c.artifact_name() != PLAN_ARTIFACT_NAME)
    );
    let shipped = PrebuiltMomentum::planned(SHIPPED_PLAN, BUILD);
    let id = shipped.id.clone();
    assert_eq!(
        (id.kind, id.name.as_str(), id.version),
        (ArtifactKind::Code, PLAN_ARTIFACT_NAME, 1)
    );
    let mut e = Enc::new();
    e.str(PLAN_ARTIFACT_NAME);
    e.u32(1);
    e.str(BUILD);
    e.str(super::plan::PLAN_NOTE);
    e.u8(PLAN_FORMAT);
    e.bytes(&SHIPPED_PLAN.encode());
    e.str(PLAN_INTERPRETER);
    assert_eq!(
        id,
        ArtifactId::new(ArtifactKind::Code, PLAN_ARTIFACT_NAME, 1, e.as_bytes())
    );
    assert!(super::plan::PLAN_NOTE.contains("interpreter"));
    assert_eq!(shipped.code(), None);
    assert_eq!(shipped.sched(), Sched::Plan(SHIPPED_PLAN));
    for &c in MomentumCode::ALL {
        let prebuilt = PrebuiltMomentum::new(c, BUILD);
        assert_eq!(prebuilt.code(), Some(c));
        assert_eq!(prebuilt.sched(), Sched::Prebuilt(c));
        assert_ne!(prebuilt.id.digest, id.digest, "{c:?}");
    }
    assert_ne!(
        PrebuiltMomentum::planned(DEMO_PLAN, BUILD).id.digest,
        id.digest
    );
    assert_ne!(
        PrebuiltMomentum::planned(SHIPPED_PLAN, "another build")
            .id
            .digest,
        id.digest
    );
    let shipped_identity = momentum_identity(&PrebuiltMomentum::new(MomentumCode::Shipped, BUILD));
    assert_eq!(
        shipped_identity.with_code(id.clone()),
        momentum_identity(&shipped)
    );

    let t0 = Instant::now();
    let mut host = planned_host(t0, SHIPPED_PLAN);
    let _ = play(
        &mut host,
        t0,
        &[Call::Key {
            at: ms(1),
            tau: cfg(true).tau_s,
        }],
    );
    assert_eq!(host.aos_code(), None);
    assert_eq!(host.aos_sched(), Sched::Plan(SHIPPED_PLAN));
    let status = host.aos_status();
    assert_eq!(status.code, PLAN_NAME);
    assert_eq!(status.code_digest, Some(id.digest.short()));
    let trace = host.aos_trace().expect("attached");
    assert_eq!(
        trace.replay(t0, BUILD).err(),
        Some(TraceFault::UnknownCode {
            name: PLAN_ARTIFACT_NAME.to_owned(),
            build: BUILD.to_owned(),
        })
    );
    host.attach_planned(t0, SHIPPED_PLAN, BUILD, 2, MOMENTUM_HOST_MAX_EVENTS);
    assert_eq!(host.aos_status().log_epoch, 2);
    assert_eq!(host.aos_status().revision, 0);
    host.attach_planned(t0, DEMO_PLAN, BUILD, 3, MOMENTUM_HOST_MAX_EVENTS);
    assert_eq!(host.aos_sched(), Sched::Plan(DEMO_PLAN));
    host.aos_attach(t0, MomentumCode::Shipped, BUILD, 4);
    assert_eq!(host.aos_code(), Some(MomentumCode::Shipped));
}
