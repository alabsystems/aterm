// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! MEASUREMENTS, reported and never asserted (design §9: "every wait in a test
//! is a hang detector, never a latency budget"). Each prints what it measured
//! and passes; run one by name:
//!
//! `targo --unverified test -p aterm-slog --release --test slog_suite --
//! measure:: --ignored --nocapture`
//!
//! The clock reads are the measurements' own; the engine still reads none.

use aterm_slog::{
    Close, Conf, Fields, Key, Kind, Log, Mark, Open, Out, RunOut, Src, Tier, TurnOut, decode,
    encode,
};

use crate::support::{agent_session, at};

/// A realistic session's records: an agent run with turns of verdict marks,
/// tool-like steps and closes, and shell commands with their exit codes.
#[must_use]
pub fn realistic_log(turns: u64) -> Log {
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut ms = 10;
    for t in 0..turns {
        ms += 100;
        let turn = log
            .open(
                Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                at(ms),
            )
            .expect("a turn opens");
        for w in ["busy", "prompt", "busy", "idle"] {
            ms += 37;
            let f = Fields::new()
                .word(Key::Word, w)
                .and_then(|f| f.u(Key::Rev, ms))
                .and_then(|f| f.id(Key::Gen, &format!("0.{ms}")))
                .and_then(|f| f.id(Key::Fp, &format!("{:016x}", ms * 2654435761)))
                .expect("verdict fields");
            log.mark(
                Mark::new(Kind::Agent, Src::Screen, Conf::Strong)
                    .at(run)
                    .fields(f),
                at(ms),
            )
            .expect("a verdict mark");
        }
        let step = log
            .open(
                Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                    .under(turn)
                    .fields(Fields::new().word(Key::Box, "bash").expect("a box")),
                at(ms),
            )
            .expect("a step");
        ms += 900;
        log.close(
            step,
            Close::new(
                Out::Step(aterm_slog::StepOut::Approved),
                Src::Screen,
                Conf::Strong,
            ),
            at(ms),
        )
        .expect("the step closes");
        ms += 1200;
        log.close(
            turn,
            Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
            at(ms),
        )
        .expect("the turn closes");
        if t % 3 == 0 {
            let f = Fields::new()
                .u(Key::Blk, t)
                .and_then(|f| f.tok(Key::Detail, "cargo"))
                .expect("cmd fields");
            let cmd = log
                .open(
                    Open::new(Kind::Cmd, Src::Shell, Conf::Strong).fields(f),
                    at(ms),
                )
                .expect("a command");
            ms += 300;
            log.close(
                cmd,
                Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact)
                    .fields(Fields::new().i(Key::Code, 0).expect("a code")),
                at(ms),
            )
            .expect("the command closes");
        }
    }
    log
}

/// The ns per record [`decode`] takes over `lines`, best of `rounds`.
fn decode_ns(lines: &[String], rounds: usize) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..rounds {
        #[expect(
            clippy::disallowed_methods,
            reason = "a measurement reads the clock it reports"
        )]
        let t0 = std::time::Instant::now();
        let mut n = 0usize;
        for l in lines {
            n += usize::from(decode(l).is_ok());
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "a measurement reads the clock it reports"
        )]
        let took = t0.elapsed();
        assert_eq!(n, lines.len(), "every line decodes");
        best = best.min(took.as_nanos() as f64 / lines.len() as f64);
    }
    best
}

/// §10.3: one record's line decode, ns, over a realistic window (the carry
/// decodes up to 2048 a session inside the update's freeze).
#[test]
#[ignore = "measurement: run by name with --ignored --nocapture"]
fn measure_line_decode_ns_per_record() {
    let log = realistic_log(400);
    let lines: Vec<String> = log
        .window()
        .map(|r| encode(r, Tier::Host).expect("fits"))
        .collect();
    let bytes: usize = lines.iter().map(String::len).sum();
    let ns = decode_ns(&lines, 300);
    println!(
        "MEASURE decode: {} records, {} bytes ({:.0} B/record): {ns:.0} ns/record",
        lines.len(),
        bytes,
        bytes as f64 / lines.len() as f64
    );
}

/// One append's wall time, ns, as the measurement's own clock reads it.
fn timed(f: impl FnOnce()) -> u128 {
    #[expect(
        clippy::disallowed_methods,
        reason = "a measurement reads the clock it reports"
    )]
    let t0 = std::time::Instant::now();
    f();
    #[expect(
        clippy::disallowed_methods,
        reason = "a measurement reads the clock it reports"
    )]
    let took = t0.elapsed();
    took.as_nanos()
}

/// §10.4: an append's cost with level-aware eviction active, over 10^6
/// appends of a realistic mix (verdict marks on an agent run, turns with an
/// approval step, shell commands, person marks) at the session bounds:
/// amortized ns per append, the worst single append (a batch), and the
/// worst with 60 nodes open (a deep open path, every batch classifying past
/// it).
#[test]
#[ignore = "measurement: run by name with --ignored --nocapture"]
fn measure_append_cost_with_eviction() {
    use aterm_slog::{Actor, Node, StepOut};
    let mut log = Log::new(1);
    let run = agent_session(&mut log, 0);
    let mut rng = aterm_spec::rng::XorShift::new(3);
    let (mut total, mut worst, mut n) = (0u128, 0u128, 0u64);
    let mut ms = 0;
    let mut turn = None;
    while log.last_seq() < 1_000_000 {
        ms += 1;
        let pick = rng.below(10);
        let ns = timed(|| match pick {
            0 if turn.is_none() => {
                turn = log
                    .open(
                        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
                        at(ms),
                    )
                    .ok();
            }
            1 if turn.is_some() => {
                let t = turn.take().expect("open");
                let s = log
                    .open(
                        Open::new(Kind::Approval, Src::Screen, Conf::Strong)
                            .under(t)
                            .fields(Fields::new().word(Key::Box, "bash").expect("box")),
                        at(ms),
                    )
                    .expect("step");
                let _ = log.close(
                    s,
                    Close::new(Out::Step(StepOut::Approved), Src::Screen, Conf::Strong),
                    at(ms),
                );
                let _ = log.close(
                    t,
                    Close::new(Out::Turn(TurnOut::Done), Src::Screen, Conf::Strong),
                    at(ms),
                );
            }
            2 => {
                let c = log
                    .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
                    .expect("cmd");
                let _ = log.close(
                    c,
                    Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                    at(ms),
                );
            }
            3 => {
                let _ = log.mark(
                    Mark::new(Kind::Human, Src::Input, Conf::Strong)
                        .at(Node::SESSION)
                        .by(Actor::Human),
                    at(ms),
                );
            }
            _ => {
                let f = Fields::new().word(Key::Word, "busy").expect("word");
                let _ = log.mark(
                    Mark::new(Kind::Agent, Src::Screen, Conf::Strong)
                        .at(run)
                        .fields(f),
                    at(ms),
                );
            }
        });
        total += ns;
        worst = worst.max(ns);
        n += 1;
    }
    println!(
        "MEASURE append: {} records in {n} calls at 2048/256 KiB: {:.0} ns amortized a call, worst call {:.1} us, lost {}",
        log.last_seq(),
        total as f64 / n as f64,
        worst as f64 / 1000.0,
        log.lost()
    );
    // 60 nodes open (runs under runs), then marks: every batch classifies
    // its records past a deep open path.
    let mut deep = Log::new(1);
    let mut at_node = agent_session(&mut deep, 0);
    for i in 0..60 {
        at_node = deep
            .open(
                Open::new(Kind::Prog, Src::Status, Conf::Strong).under(at_node),
                at(1 + i),
            )
            .expect("nested run");
    }
    let (mut total, mut worst, mut n) = (0u128, 0u128, 0u64);
    for i in 0..200_000u64 {
        let ns = timed(|| {
            let _ = deep.mark(
                Mark::new(Kind::Bell, Src::Engine, Conf::Exact).at(at_node),
                at(100 + i),
            );
        });
        total += ns;
        worst = worst.max(ns);
        n += 1;
    }
    println!(
        "MEASURE append, 60 open: {:.0} ns amortized, worst {:.1} us",
        total as f64 / n as f64,
        worst as f64 / 1000.0
    );
}

/// §10.1/§10.2, the engine's share: one full session log carried at the
/// 128 KiB cap. The snapshot (the part a host runs under the session's
/// lock), its preparation and the skeleton's cut (off it: what every handed
/// session pays), the full cut (what a session granted all its detail pays
/// on top), and the successor's read (`Log::from_carry`, inside the update's
/// freeze), with the bytes.
#[test]
#[ignore = "measurement: run by name with --ignored --nocapture"]
fn measure_carry_encode_and_decode() {
    use aterm_slog::{CarryAdopt, CarryBudget, Limits};
    let log = realistic_log(600);
    let mut best = (u128::MAX, u128::MAX, u128::MAX, u128::MAX);
    let mut out = None;
    let host = ["host v=1".to_string()];
    for _ in 0..20 {
        let mut snap = None;
        let t_snap = timed(|| snap = Some(log.carry_snapshot()));
        let snap = snap.expect("a snapshot");
        let mut prepared = None;
        let t_skel = timed(|| {
            let p = snap.prepare();
            let _ = p.skeleton(&host);
            prepared = Some(p);
        });
        let prepared = prepared.expect("prepared");
        let mut o = None;
        let t_enc = timed(|| o = prepared.encode(CarryBudget::SESSION, &host));
        let o = o.expect("carried");
        let a = CarryAdopt {
            ep: log.ep() + 1,
            next_seq: log.last_seq() + 65_537,
            ids: log.id_counters(),
            limits: Limits::SESSION,
            at: at(9_999_999),
        };
        let t_dec = timed(|| {
            let _ = Log::from_carry(&o.lines, a).expect("reads");
        });
        best = (
            best.0.min(t_snap),
            best.1.min(t_skel),
            best.2.min(t_enc),
            best.3.min(t_dec),
        );
        out = Some(o);
    }
    let o = out.expect("carried");
    println!(
        "MEASURE carry: ring {} records / {} B; carried {} records, shed {}, {} B ({} B skeleton) in {} lines; snapshot {:.0} us (under the lock), prepare+skeleton {:.0} us, full cut+encode {:.0} us, from_carry {:.0} us",
        log.len(),
        log.bytes(),
        o.carried,
        o.shed,
        o.bytes,
        o.skeleton_bytes,
        o.lines.len(),
        best.0 as f64 / 1000.0,
        best.1 as f64 / 1000.0,
        best.2 as f64 / 1000.0,
        best.3 as f64 / 1000.0
    );
}

/// §10.5: a full log's estimated heap against its encoded bytes (what the
/// ring's 256 KiB counts), and the parts of it.
#[test]
#[ignore = "measurement: run by name with --ignored --nocapture"]
fn measure_log_memory() {
    let log = realistic_log(700);
    println!(
        "MEASURE memory: {} records, {} B encoded, footprint {} B ({:.2}x), fold {} B, pinned {} B",
        log.len(),
        log.bytes(),
        log.footprint(),
        log.footprint() as f64 / log.bytes() as f64,
        log.fold().footprint(),
        log.pinned_bytes()
    );
}

/// §6: what a close costs the event loop to keep a full session for the
/// closed-session index (`Log::kept_within`, the skeleton then its summary),
/// best of a few rounds, and what each copy weighs.
#[test]
#[ignore = "measurement: run by name with --ignored --nocapture"]
fn measure_closed_copy() {
    let mut log = realistic_log(700);
    log.close_session(
        Close::new(
            Out::Session(aterm_slog::SessionOut::UiClose),
            Src::Aterm,
            Conf::Exact,
        ),
        crate::support::at(10_000_000),
    )
    .expect("the session closes");
    let mut best = std::time::Duration::MAX;
    let mut sizes = (0, 0, 0);
    for _ in 0..5 {
        #[expect(
            clippy::disallowed_methods,
            reason = "a measurement reads the clock it reports"
        )]
        let t0 = std::time::Instant::now();
        let skeleton = log.kept_within(64 * 1024);
        let summary = skeleton.kept_within(0);
        #[expect(
            clippy::disallowed_methods,
            reason = "a measurement reads the clock it reports"
        )]
        let took = t0.elapsed();
        best = best.min(took);
        sizes = (skeleton.bytes(), skeleton.footprint(), summary.footprint());
    }
    println!(
        "MEASURE closed copy: {} records {} B -> skeleton {} B (footprint {} B), summary \
         footprint {} B, in {best:?}",
        log.len(),
        log.bytes(),
        sizes.0,
        sizes.1,
        sizes.2
    );
}

/// Phase 4 M3, the engine's share of `log-take`: 300 sessions each holding
/// 64 unlanded bus-class records (32 shell commands). Per session, the copy
/// (`Log::bus_copy`, all the timeline lock is held for) and its encode (off
/// the lock), and the drain of all 300 × 64 slots in takes of 64, as the GUI
/// serves `log-take n=64` (its store walk and the wire are not in this).
#[test]
#[ignore = "measurement: run by name with --ignored --nocapture"]
fn measure_bus_take() {
    use aterm_slog::Stamp;
    const SESSIONS: usize = 300;
    let mut logs: Vec<Log> = (0..SESSIONS)
        .map(|_| {
            let mut log = Log::new(1);
            let _ = agent_session(&mut log, 0);
            let mut ms = 10;
            while log.ms_head() < 66 {
                ms += 1;
                let c = log
                    .open(Open::new(Kind::Cmd, Src::Shell, Conf::Strong), at(ms))
                    .expect("cmd");
                let _ = log.close(
                    c,
                    Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
                    at(ms),
                );
            }
            // What the session's first take would carry is landed: 64 owed.
            let first = log
                .bus_copy(2, Stamp::new(9, 9))
                .encode(Tier::Wire, &|_| None);
            if let Some(s) = first.slots.last() {
                log.land(s.at).expect("lands");
            }
            log
        })
        .collect();
    let (mut copies, mut encodes, mut slots) = (Vec::new(), Vec::new(), 0usize);
    let mut takes = 0usize;
    #[expect(
        clippy::disallowed_methods,
        reason = "a measurement reads the clock it reports"
    )]
    let t0 = std::time::Instant::now();
    for log in &mut logs {
        let mut copy = None;
        copies.push(timed(|| copy = Some(log.bus_copy(64, Stamp::new(10, 10)))));
        let copy = copy.expect("copied");
        let mut take = None;
        encodes.push(timed(|| take = Some(copy.encode(Tier::Wire, &|_| None))));
        let take = take.expect("encoded");
        slots += take.slots.len();
        takes += 1;
        if let Some(s) = take.slots.last() {
            log.land(s.at).expect("lands");
        }
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a measurement reads the clock it reports"
    )]
    let all = t0.elapsed();
    copies.sort_unstable();
    encodes.sort_unstable();
    let pct = |v: &[u128], p: usize| v[(v.len() * p / 100).min(v.len() - 1)] as f64 / 1000.0;
    println!(
        "MEASURE bus take: {SESSIONS} sessions, {slots} slots in {takes} takes of <= 64: \
         copy under the lock p50 {:.1} us p99 {:.1} us max {:.1} us; encode p50 {:.1} us max {:.1} us; \
         the whole drain {:.1} ms ({:.1} us a take)",
        pct(&copies, 50),
        pct(&copies, 99),
        *copies.last().expect("one") as f64 / 1000.0,
        pct(&encodes, 50),
        *encodes.last().expect("one") as f64 / 1000.0,
        all.as_secs_f64() * 1000.0,
        all.as_secs_f64() * 1e6 / takes as f64
    );
}
