// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE SUBSCRIBE AXIS — the fleet-observation path nothing priced.
//
// `frame_latency` prices the render path; `workspace_scaling` prices the
// whole-workspace passes around it. Neither touches the CONTROL SOCKET's push
// loop, which is the surface a fleet orchestrator actually leans on: one
// `subscribe … events` connection watching N sessions, waking on notify or on a
// 250 ms liveness timeout, and emitting a digest line per new block / turn /
// meta-change / title / bell.
//
// WHAT THIS FILE IS FOR. The digest's per-wake cost was found to scale with
// RETAINED HISTORY DEPTH rather than with the live window: three monotone-id
// ledgers, each drained by a LINEAR filter against a watermark, re-walked on
// every wake including the bare liveness ticks where nothing has happened. So
// the sweep here is over the depth of those ledgers — completed shell blocks
// (`OUTPUT_BLOCKS_MAX` = 1000), turn records (`LEDGER_CAP` = 512) and timeline
// events (`TIMELINE_CAP` = 512) — crossed with the number of watched targets.
// A design whose idle wake is O(new) is FLAT in depth; one whose idle wake is
// O(total) is not. That difference is the whole measurement, and it is why the
// depth axis, not the target axis, is the primary one.
//
// WHAT IS TIMED, EXACTLY — the `frame_latency` contract, verbatim:
//
//     arm(..);                     // UNTIMED: fixture build, ledger fill, seed
//     let t0 = Instant::now();     //  ── timed span opens
//     black_box(<pass>);           //  the SHIPPING entry point, called directly
//     total += t0.elapsed();       //  ── timed span closes
//
// The timed pass is `subscribe::frames_for_watch` — the push loop's real
// per-target per-wake body — driven in the same loop shape `pump` drives it in,
// through the `bench-support` seam (an external target cannot see a private
// module, and the fix for that is a seam, not a copy of the code).
//
// TWO-SIDED GUARDS, on every workload:
//
//   * REACH (lower): the ledgers really are filled to the depth being priced
//     (`retained()` is asserted against the shipping caps, not against what the
//     fixture was ASKED for), AND the digest really does reach them — after one
//     genuine ledger append, a wake must produce frames. A fixture that failed
//     to fill, or a digest that short-circuited above the scan, would otherwise
//     be priced as a fast idle wake and called a win.
//
//   * BOUND (upper): an idle wake produces EXACTLY ZERO bytes. This is the
//     load-bearing one. `events` is a live stream, so a correctly seeded watch
//     emits nothing until something new lands; a watch seeded with empty
//     watermarks would dump the entire retained backlog on its first wake and
//     then go quiet, which would price a serialisation path under an idle name
//     and hide the scan entirely.
//
// WHAT IS OUT OF REACH, cut honestly rather than modelled: the socket write
// (`Egress` needs a peer; the digest fills a `String` and the caller writes it,
// so the split is the shipping one) and the notify/park round trip (a real
// `Subscription` would put a 250 ms sleep inside the timed span).
//
// The INSTANCE `sessions` roster arm is PARTLY modelled and says so: its diff is
// the shipping function, its set rebuild is an equivalent-allocation stand-in for
// `SessionStore::live_sids` (a registered `SessionHandle` needs a whole
// `SessionCtx`, reachable only from a `#[cfg(test)]` fixture). See
// `subscribe::bench_seam::RosterRebuild` for the exact gap.

use std::time::{Duration, Instant};

use aterm_gui::bench_support::{DigestBench, RosterBench};
use criterion::measurement::WallTime;
use criterion::{
    BenchmarkGroup, BenchmarkId, Criterion, black_box, criterion_group, criterion_main,
};

// ------------------------------------------------------------------ dials --

/// The RETAINED-DEPTH sweep — the primary axis. `0` is the empty-ledger floor
/// every "flat in depth" claim is measured against; `64` is an ordinary working
/// session; `SATURATED` asks for more than any shipping ring retains, so the
/// fixture ends up pinned at the caps (1000 blocks, 512 turns, 512 timeline
/// events) — the depth a long-lived agent session actually sits at.
const DEPTHS: [usize; 3] = [0, 64, SATURATED];

/// See [`DEPTHS`]. Deliberately past every cap so the saturated arm is defined
/// by the SHIPPING caps rather than by a number in this file.
const SATURATED: usize = 4096;

/// The WATCHED-TARGET sweep. 1 isolates the per-target cost; 30 is the
/// campaign's power-user fleet on ONE `events` subscription — the shape the
/// whole push face exists for.
const TARGETS: [usize; 3] = [1, 8, 30];

/// The shipping ring caps, restated here ONLY as assertions about what the
/// fixture reached. If the engine's cap moves, this file should fail loudly
/// rather than quietly price a different depth.
const BLOCKS_CAP: usize = 1000;
/// See [`BLOCKS_CAP`]. `LEDGER_CAP` / `TIMELINE_CAP`.
const LEDGER_CAP: usize = 512;

/// The LIVE-SESSION sweep for the instance roster tick. 30 is the campaign's
/// power-user fleet; 100 is a heavy orchestrator; 1 is the baseline every
/// "flat in N" claim is measured against.
const SESSIONS: [usize; 3] = [1, 30, 100];

// ------------------------------------------------------------------ report --

/// The one human-readable line each verify pass prints: what state the workload
/// reached, in the numbers its guards assert on.
fn report(name: &str, detail: &str) {
    println!("REACH {name:<34} | {detail}");
}

// ------------------------------------------------------------------ verify --

/// PROVE one digest workload before it is timed, then hand the warmed fixture
/// back so the timed run continues from the verified state.
///
/// Both sides, in order:
///   1. the ledgers hold what this depth claims (saturating at the caps);
///   2. an idle wake emits NOTHING — the watch is seeded live, so what follows
///      is the scan and only the scan;
///   3. after one real turn append, a wake DOES emit — the digest reaches the
///      ledger it is being priced against;
///   4. and it is quiet again immediately after, so the timed loop below is
///      measuring idle wakes, not a stuck emit path.
fn verify(targets: usize, depth: usize) -> DigestBench {
    let mut b = DigestBench::build(targets, depth, depth, depth);
    let (blocks, turns, timeline) = b.retained();

    let want_blocks = depth.min(BLOCKS_CAP);
    let want_ledger = depth.min(LEDGER_CAP);
    assert!(
        blocks >= want_blocks && blocks <= BLOCKS_CAP + 1,
        "digest/{targets}x{depth}: engine retained {blocks} blocks, wanted \
         {want_blocks} (cap {BLOCKS_CAP}) — the fixture is not priced at the \
         depth it claims"
    );
    assert_eq!(
        (turns, timeline),
        (want_ledger, want_ledger),
        "digest/{targets}x{depth}: ledger depths are not what this arm prices"
    );

    assert_eq!(
        b.wake(true),
        0,
        "digest/{targets}x{depth}: a freshly seeded events watch must emit \
         NOTHING — a non-zero idle wake means the watermarks were seeded empty \
         and this workload would price a backlog dump, not the idle scan"
    );

    b.land_turn();
    let emitted = b.wake(true);
    assert!(
        emitted > 0,
        "digest/{targets}x{depth}: a wake after a real turn append emitted \
         nothing — the digest is not reaching the ledger this arm prices"
    );
    assert_eq!(
        b.wake(true),
        0,
        "digest/{targets}x{depth}: the digest must be quiet again once the new \
         record is reported (no double-report, and the timed loop below really \
         is idle)"
    );

    report(
        &format!("digest/{targets}x{depth}"),
        &format!(
            "blocks={blocks} turns={turns} timeline={timeline} \
             emit_after_append={emitted}B idle_emit=0B"
        ),
    );
    b
}

// ------------------------------------------------------------------ timing --

/// The IDLE WAKE: the 250 ms liveness tick that is the overwhelming majority of
/// what a `subscribe … events` connection does. Nothing has changed, nothing is
/// emitted, and the only question is what the digest paid to find that out.
///
/// This is the number the finding is about. Flat across [`DEPTHS`] means the
/// digest costs O(new); rising with depth means it costs O(total retained).
fn idle_wake(g: &mut BenchmarkGroup<'_, WallTime>, targets: usize, depth: usize) {
    let mut b = verify(targets, depth);
    g.bench_function(
        BenchmarkId::from_parameter(format!("t{targets}_d{depth}")),
        |bch| {
            bch.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let t0 = Instant::now();
                    // `woke = true` is the CONSERVATIVE arm: a real liveness
                    // timeout passes `false`, which lets an every-frame content
                    // subscriber skip a re-emit — but this subscription carries
                    // no content streams, so the two are the same code path and
                    // `true` cannot flatter the result.
                    black_box(b.wake(true));
                    total += t0.elapsed();
                }
                total
            });
        },
    );
}

/// The EMITTING WAKE: one genuinely new turn record per target, reported and
/// then quiet. Priced beside the idle wake so a change that made idle cheap by
/// making emission expensive cannot hide — the pair is the honest picture.
fn emit_wake(g: &mut BenchmarkGroup<'_, WallTime>, targets: usize, depth: usize) {
    let mut b = verify(targets, depth);
    g.bench_function(
        BenchmarkId::from_parameter(format!("t{targets}_d{depth}")),
        |bch| {
            bch.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                let mut emitted = 0usize;
                for _ in 0..iters {
                    // UNTIMED arm: land the record the timed wake will report.
                    b.land_turn();
                    let t0 = Instant::now();
                    let n = black_box(b.wake(true));
                    total += t0.elapsed();
                    emitted += n;
                }
                assert!(
                    emitted > 0 || iters == 0,
                    "emit_wake: every timed wake emitted nothing — the arm is idle"
                );
                total
            });
        },
    );
}

/// The INSTANCE ROSTER wake: what a `subscribe … sessions` connection pays per
/// 250 ms tick to answer "did the session set change?".
///
/// The shipping answer is to REBUILD the whole live-sid set and run two
/// `HashSet::difference` passes over it — every tick, per subscriber, whether or
/// not anything moved. So this arm rises with live sessions by construction, and
/// that rise IS the finding: a monotonic lifecycle journal on the store turns the
/// unchanged case into one `u64` compare, at which point this entire arm is work
/// the shipping tick no longer does. Read it as the size of a deleted cost
/// category, not as a before/after of the same call.
///
/// Two-sided, like the digest arms: an unchanged tick must emit ZERO bytes (or
/// the timed loop is measuring a serialisation path), and a tick after real churn
/// must emit SOMETHING (or the diff is not running at all).
fn roster_wake(g: &mut BenchmarkGroup<'_, WallTime>, sessions: usize) {
    let mut b = RosterBench::new(sessions);
    assert_eq!(
        b.sessions(),
        sessions,
        "roster/{sessions}: the fixture is not the size it claims"
    );
    assert_eq!(
        b.tick(),
        0,
        "roster/{sessions}: an unchanged roster must emit nothing — a non-zero \
         tick means the fixture starts un-caught-up and this would price a \
         first-sync dump"
    );
    b.churn();
    assert!(
        b.tick() > 0,
        "roster/{sessions}: a tick after a real spawn+exit emitted nothing — the \
         set diff is not running"
    );
    assert_eq!(
        b.tick(),
        0,
        "roster/{sessions}: and quiet again once reported"
    );
    report(
        &format!("roster/{sessions}"),
        &format!("sessions={sessions} idle_emit=0B churn_emit>0B"),
    );
    g.bench_function(BenchmarkId::from_parameter(format!("n{sessions}")), |bch| {
        bch.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let t0 = Instant::now();
                black_box(b.tick());
                total += t0.elapsed();
            }
            total
        });
    });
}

fn subscribe_digest(c: &mut Criterion) {
    {
        let mut g = c.benchmark_group("subscribe_digest_idle");
        for &targets in &TARGETS {
            for &depth in &DEPTHS {
                idle_wake(&mut g, targets, depth);
            }
        }
        g.finish();
    }
    {
        let mut g = c.benchmark_group("subscribe_digest_emit");
        // SATURATED DEPTH ONLY, and that is deliberate. The emit arm appends a
        // record per timed iteration, so on a SHALLOW arm the ledger would fill
        // up underneath the measurement and the "depth" in the id would be a
        // lie by the tenth sample. At saturation the ring is already pinned at
        // its cap by drop-oldest, so appending keeps the depth exactly where the
        // arm says it is — and it is also the worst case, which is the one an
        // emit path has to be defended at.
        for &targets in &TARGETS {
            emit_wake(&mut g, targets, SATURATED);
        }
        g.finish();
    }
    {
        let mut g = c.benchmark_group("subscribe_roster_rebuild");
        for &sessions in &SESSIONS {
            roster_wake(&mut g, sessions);
        }
        g.finish();
    }
}

criterion_group!(benches, subscribe_digest);
criterion_main!(benches);
