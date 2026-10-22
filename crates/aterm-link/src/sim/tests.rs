// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SIMULATION'S SEEDS, its negative controls and its replay.
//!
//! * [`every_seed_holds_every_law`]: the committed world, seed after seed,
//!   breaks no law, and the seeds together exercise every fault the
//!   scheduler has (a run that exercised nothing cannot pass);
//! * [`each_rule_switched_off_is_caught`]: each rule the bridge, the endpoint
//!   and the broker arm keep is switched off once, and some seed then breaks
//!   the law that rule exists for — while, with the rule on, the same world
//!   and seed keep it;
//! * [`a_failing_seed_replays_exactly`]: a seed run twice is the same run,
//!   trace for trace, failure for failure;
//! * [`the_broker_arm_cuts_exactly_what_the_law_allows`] and
//!   [`the_device_keeps_a_durable_write_and_may_roll_back_the_rest`]: the two
//!   simulated arms, on their own;
//! * the FINDINGS the simulation made: F1, a torn state file after a power cut
//!   that wedged every later attach, is decided — the committed device tears,
//!   the bridge sets a torn file aside, and the old refusal is its negative
//!   control
//!   ([`a_torn_state_file_after_a_power_cut_is_set_aside_and_the_attach_goes_on`]);
//!   F2, a post the log lost that a later power cut brought back out of its
//!   sender's order, is decided — the first bridge of a boot after an unclean
//!   stop retires every whole in-flight entry, never publishing it again; the
//!   world that loses power both ways is a committed sweep
//!   ([`every_seed_of_the_world_that_loses_power_both_ways_holds_every_law`])
//!   and the old republish is its negative control
//!   ([`a_post_the_log_lost_stays_lost_after_a_later_power_cut`]); F4, a
//!   ghost row that outlived the bridges that could retire it, is decided —
//!   every session row carries its state dir's mark, a later bridge of the
//!   dir in the same boot retires its predecessors' ghosts by it, the same-boot
//!   worlds leave none, and the mark unread is its negative control
//!   ([`a_ghost_row_is_retired_by_a_later_bridge_of_its_state_dir`]), while a
//!   sibling under the node id is another state dir's, retired only by an
//!   attach that witnessed its will
//!   ([`a_siblings_row_is_retired_only_by_the_attach_that_witnessed_its_will`]),
//!   even one that took an incarnation this dir took
//!   ([`a_sibling_that_takes_one_of_this_dirs_incarnations_is_never_retired`],
//!   F4's review: the chain of incarnations the dir took, the first decision,
//!   retired such a sibling's live rows);
//!   F3, an endpoint that forgot every offset when a killed attach left it a
//!   generation ahead of the next bridge's witness, is fixed in
//!   aterm-gui-fabric (`LogClock::keep_below`) and is a negative control here;
//! * the WRITE ERRORS of sim item S7: the device fails the state dir's writes
//!   in every other eight committed seeds, and every law holds under it
//!   ([`every_seed_with_write_errors_holds_every_law`], where aterm quits too) —
//!   the eight defects it found fixed, each switched off a row of
//!   [`each_rule_switched_off_is_caught`] and each seed it broke kept in
//!   [`FOUND_ERRING`], [`FOUND_BOTH_WAYS`], [`FOUND_BOTH_WAYS_ERRING`] and
//!   [`FOUND_QUITTING`]; a durable write whose flush failed after its rename
//!   is written again rather than trusted
//!   ([`a_durable_write_whose_flush_failed_is_not_trusted_and_is_written_again`]);
//! * the seeds a wider search found breaking a law, each fixed in the bridge
//!   and kept here ([`the_seeds_that_broke_a_law_hold_every_law`]).
//!
//! One rule no END-STATE law can see broken: the inbox commit's wait for a
//! record's delivery. Committed before delivery, a record a kill left
//! undelivered is offered again by the next bridge's refill (§6.2), which
//! starts below every row the endpoint never took, so the world settles with
//! nothing lost; the model's tap sees the step itself — the group past a
//! record no row holds — and is that rule's control
//! (`Mutant::CommitBeforeDelivery`).
//!
//! A seed is a whole run, named in every failure with the tail of its trace;
//! `world::run(&Opts::seed(n))` runs it again, the same run (the replay test).

use super::bus::{self, ArmMutant, Escalation, Stop, Tier};
use super::device::Device;
use super::endpoint::EpRules;
use super::world::{run, Mutant, Opts, Outcome};
use super::Rng;

/// How many seeds the committed world is held to on every run (half of them
/// one device, half two, half of each with the bus mirror on, and half of
/// those with the broker holding acks: [`Opts::seed`]). A wider search, seeds
/// 1..=8064 at 400 steps and 1..=400 at 1500, breaks no law once the fixes
/// [`FOUND`] keeps are in; after aterm-0's loop and its held acks, as
/// reviewed (an idle loop waits for its posts' answers, the idle law on, a
/// rolled-back batch refusing commits too), 1..=4000 at 400 steps and
/// 1..=400 at 1500 broke none (134,105 and 56,819 acks held, 5,926 and 2,078
/// reads that went on without one, 626 and 80 records read before their
/// ack). With the device tearing (finding F1, decided) to nothing or
/// part-way, a sweep that sets aside every file not provably whole,
/// and a torn mirror pin left to stop the mirror, 1..=4000 at 400 steps and
/// 1..=400 at 1500 broke none (10,153 and 3,686 power cuts, 6,964 and 3,063
/// torn files set aside). With every whole in-flight entry of an earlier boot
/// retired after an unclean stop (finding F2, decided), 1..=8064 at 400 steps
/// and 1..=400 at 1500 broke none (20,387 and 3,670 power cuts, 18,518 and
/// 4,011 entries retired); and the world that loses power both ways
/// ([`both_ways`]), 1..=4000 at 400 and 1..=400 at 1500, broke none either
/// (20,327 and 7,412 power cuts, 16,148 and 5,707 generation moves, 18,264
/// and 8,034 entries retired) — where the old republish broke the order at
/// 67 of 1..=2000, and nothing else. With every session row carrying its state
/// dir's mark and a later bridge of the dir retiring its predecessors' ghosts
/// by it (finding F4, decided), 1..=2000 at 400 steps and 1..=400 at 1500
/// broke none and left no ghost row (5,111 and 3,671 power cuts, 11,814 and
/// 9,003 ghost rows retired), nor did the world that loses power both ways,
/// 1..=2000 at 400 (10,095 power cuts, 21,412 ghost rows retired), nor the
/// worlds with a sibling ([`Opts::with_a_sibling`]), 1..=2000 at 400 and
/// 1..=400 at 1500 (the sibling's row standing under a bridge that outranked
/// it in 1,356 and 375, retired by the attach that witnessed its will in 340
/// and none), nor the worlds whose sibling races an attach for one of this
/// dir's incarnations ([`Opts::with_a_racing_sibling`]), 1..=2000 at 400 and
/// 1..=400 at 1500 (the race run in 1,978 and 397, the sibling's row standing
/// in every one). Those F4 measurements used the superseded, unqualified
/// device/inode mark; they do not establish cross-boot retirement for the
/// current boot-bound mark. With the device failing writes (sim item S7) in
/// every other eight seeds and its fixes in, composed with main's per-open
/// run admission, 1..=4000 at 400 steps and 1..=400 at 1500 broke none
/// (11,961 and 4,396 writes failed before their effect, 2,113 and 738
/// durable replaces renamed with their flush failed, 1,422 and 511 appends
/// cut part-way, 63,281 and 23,230 writes a full disk refused), nor the
/// world that loses power both ways, 1..=4000 at 400. With the errors on in
/// every seed and aterm quitting now and then (`erring_quitting`,
/// `both_ways_erring_quitting`), 1..=4000 at 400 of each and 1..=400 at 1500
/// of the second broke none (25,002, 25,739 and 9,400 writes failed; 1,906,
/// 2,253 and 1,208 flushes failed; 14,544 quits of aterm in the first, 9,542
/// of them stopping the run cleanly), nor the worlds with a sibling or a
/// racing one, erring and quitting, 1..=500 each, nor the both-ways world
/// erring without quits, 1..=2000 — each process putting one copy of a file
/// in the quarantine however often its recovery was tried. This many run in
/// a few seconds.
const SEEDS: u64 = 64;

/// How many seeds the COMMITTED WORLD's own sweep runs ([`committed`]):
/// twice [`SEEDS`] since sim item S7, whose write errors are on in every
/// other eight seeds ([`Opts::write_errors`]), so that as many of its seeds
/// run on a clean disk as before them, and as many again on a failing one.
const COMMITTED_SEEDS: u64 = 2 * SEEDS;

/// The most seeds a negative control may take to show its rule's law broken.
const CONTROL_SEEDS: u64 = 64;

/// How many seeds the worlds with a SIBLING under the node id are held to
/// ([`a_siblings_row_is_retired_only_by_the_attach_that_witnessed_its_will`]).
const SIBLING_SEEDS: u64 = 32;

/// The most seeds a control of a RARE interleaving may take
/// ([`each_rule_switched_off_is_caught`]'s list). 2000 since aterm-0: the
/// loop that stopped parking on its own commits and acks moved F3's chain —
/// a kill on the `link log` line between two bus-only losses — from seed 308
/// to 1730, the first within 6000. 6000 since the mirror's durable copies
/// (`logkey`, `logfloor`): each is a durable replace in the state dir's root,
/// whose directory flush keeps `inc-lease`'s newest version through the next
/// power cut, so the rollback the incarnation lease's control needs (an older
/// `inc-lease` trusted across the boot) is rarer in a mirroring world — the
/// first within 8000 moved from 546 to 4023, and none within 4000 with the
/// mirror off. 9000 since sim item S6 (a clean stop's one full flush, the
/// emptied quarantine heard): the lease's control moved from 4023 to 8483, the
/// first within 12000. Sim item S7 adds one more such flush, the floor each
/// open writes again before its first batch (`StateDir::raise_logfloor`):
/// with it on, 8483 no longer breaks the lease's law, so the lease's control
/// switches that admission off as well, and at 8483 again (neither off alone
/// breaks it there, and the lease alone off breaks no seed of 1..=14000).
/// 20000 since sim item S7 composed with main's per-open run admission (one
/// more durable write at every bridge start, which moves where each erring
/// seed's write errors land): the replay hold's control
/// (`Mutant::DeliverBeforeTheDrain`) moved from 1844 to 18017, rare enough
/// that it has a unit test of its own as well, `bridge.rs`'s
/// `a_replayed_record_waits_for_its_posts_retirement_so_a_move_delivers_it_once`.
/// A search stops at its first hit, so the wider limit costs a run only
/// where a witness went stale.
const RARE_SEEDS: u64 = 20000;

fn report(out: &Outcome) -> String {
    let tail = out.trace.len().saturating_sub(40);
    format!(
        "seed {} ({:?}) {:?}:\n  {}\n  trace tail:\n    {}",
        out.seed,
        out.tier,
        out.stats,
        out.violations.join("\n  "),
        out.trace[tail..].join("\n    ")
    )
}

/// **THE COMMITTED WORLD BREAKS NO LAW**, over [`COMMITTED_SEEDS`] seeds — every tier,
/// kills at every kind of I/O step, relaunches, broker restarts of every kind
/// with generation moves and lost suffixes, power cuts of the whole machine,
/// tabs opening and closing, session logs mirrored onto the bus, acks answered
/// after the records they name (aterm-0's reordering) — and the seeds together
/// exercised every one of those.
#[test]
fn every_seed_holds_every_law() {
    let mut total = super::world::Stats::default();
    let mut tiers = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    for out in committed() {
        if !out.violations.is_empty() {
            failures.push(report(out));
        }
        tiers.insert(format!("{:?}", out.tier));
        let s = &out.stats;
        total.passes += s.passes;
        total.pumps += s.pumps;
        total.kills += s.kills;
        total.relaunches += s.relaunches;
        total.broker_restarts += s.broker_restarts;
        total.moves += s.moves;
        total.lost_records += s.lost_records;
        total.power_cuts += s.power_cuts;
        total.mail += s.mail;
        total.posts += s.posts;
        total.receipts += s.receipts;
        total.halts += s.halts;
        total.delivered += s.delivered;
        total.exits += s.exits;
        total.log_records += s.log_records;
        total.mirrored += s.mirrored;
        total.set_aside += s.set_aside;
        total.set_aside_told += s.set_aside_told;
        total.pins_torn += s.pins_torn;
        total.pins_set_aside += s.pins_set_aside;
        total.retired += s.retired;
        total.ghosts_retired += s.ghosts_retired;
        total.ghosts_left += s.ghosts_left;
        total.acks_held += s.acks_held;
        total.tries_not_yet += s.tries_not_yet;
        total.seen_before_ack += s.seen_before_ack;
        total.writes_failed += s.writes_failed;
        total.renamed_unflushed += s.renamed_unflushed;
        total.disk_full_refused += s.disk_full_refused;
    }
    eprintln!("the committed world over {COMMITTED_SEEDS} seeds: {total:?}");
    assert!(
        failures.is_empty(),
        "{} of {COMMITTED_SEEDS} seeds broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(tiers.len(), 3, "every tier ran: {tiers:?}");
    for (what, n) in [
        ("reader pumps", total.pumps),
        ("kills", total.kills),
        ("relaunches", total.relaunches),
        ("broker restarts", total.broker_restarts),
        ("generation moves", total.moves),
        ("acked records a power loss took", total.lost_records),
        ("power cuts", total.power_cuts),
        ("mail", total.mail),
        ("posts", total.posts),
        ("receipts", total.receipts),
        ("halts", total.halts),
        ("deliveries", total.delivered),
        ("sessions that left", total.exits),
        ("session-log records", total.log_records),
        ("`ms` the mirror put on the bus", total.mirrored),
        // FINDING F1's LAW: a power cut tore state files, and a bridge set
        // them aside and attached ([`a_torn_state_file_after_a_power_cut_is_set_aside_and_the_attach_goes_on`]).
        ("torn state files set aside", total.set_aside),
        ("torn state files told to aterm", total.set_aside_told),
        // THE TORN PIN'S LAW: a power cut tore the mirror's pin, and the
        // next boot set it aside and resumed the mirror
        // ([`a_torn_mirror_pin_after_a_power_cut_is_set_aside_and_the_mirror_resumes`]).
        ("power cuts that tore the mirror's pin", total.pins_torn),
        ("torn mirror pins set aside", total.pins_set_aside),
        // FINDING F2'S LAW: a power cut found in-flight entries, and the first
        // bridge of the next boot retired them unpublished
        // ([`a_post_the_log_lost_stays_lost_after_a_later_power_cut`]).
        (
            "in-flight entries of an earlier boot retired",
            total.retired,
        ),
        ("ghost rows retired", total.ghosts_retired),
        // THE REORDERING aterm-0's loop must survive, covered by the seeds:
        // acks answered late, the bridge's reads that went on without them,
        // and records handed over before their own publish's ack.
        ("acks the broker held", total.acks_held),
        ("reads that found an ack not yet there", total.tries_not_yet),
        ("records read before their ack", total.seen_before_ack),
        // SIM ITEM S7: the device failed writes, before their effect and
        // after it, and filled up ([`every_seed_with_write_errors_holds_every_law`]
        // counts every kind).
        ("state-dir writes failed", total.writes_failed),
        (
            "durable replaces renamed with their flush failed",
            total.renamed_unflushed,
        ),
        ("writes a full disk refused", total.disk_full_refused),
    ] {
        assert!(n > 0, "the seeds exercised no {what}: {total:?}");
    }
}

/// **THE LOOP GOES ON WITHOUT ITS ACKS, AND THE LAWS HOLD EITHER WAY**
/// (aterm-0). Over the committed world's seeds whose broker holds acks, the
/// bridge's reads that must not wait found answers not yet there and it went
/// on — and every law held ([`every_seed_holds_every_law`]). NEGATIVE
/// CONTROL: the same seeds with every read waiting (`WaitForEveryAck`, the
/// loop before aterm-0) never once go on without an answer, so the count is
/// the non-blocking loop's own — and hold every law too: what aterm-0 changes
/// is when an answer is read, never what the bridge does with it.
#[test]
fn the_loop_goes_on_without_its_acks_and_a_waiting_loop_never_does() {
    let held: Vec<&Outcome> = committed()
        .iter()
        .filter(|out| Opts::seed(out.seed).hold_acks)
        .collect();
    assert!(
        !held.is_empty(),
        "PRECONDITION: some committed seeds hold acks"
    );
    let tried: u64 = held.iter().map(|out| out.stats.tries_not_yet).sum();
    assert!(tried > 0, "the loop never went on without an answer");
    let seeds: Vec<u64> = held.iter().map(|out| out.seed).collect();
    let waiting = par_map(&seeds, |&seed| {
        run(&Opts::seed(seed).with(Mutant::WaitForEveryAck))
    });
    let mut failures = Vec::new();
    for out in &waiting {
        assert_eq!(
            out.stats.tries_not_yet, 0,
            "NEGATIVE CONTROL: a loop that waits for every ack never finds one not yet there \
             (seed {})",
            out.seed
        );
        if !out.violations.is_empty() {
            failures.push(report(out));
        }
    }
    assert!(
        failures.is_empty(),
        "the waiting loop broke a law:\n{}",
        failures.join("\n")
    );
    assert!(
        waiting.iter().map(|out| out.stats.acks_held).sum::<u64>() > 0,
        "PRECONDITION: the control's broker held acks too"
    );
}

/// Whether violation `v` is one of `law`'s: its words contain one of the
/// `|`-separated phrases. The model tap's violations (`model: …`) are only a
/// law's that names the tap: each carries the tap's state legend (`occ lost g
/// past pend …`), whose words would otherwise answer for the oracle's own
/// laws — a control of the no-loss check (`lost:`) once passed on the word
/// `lost` in that legend while the check itself caught nothing.
fn breaks(v: &str, law: &str) -> bool {
    law.split('|').any(|phrase| {
        if v.starts_with("model:") {
            phrase.starts_with("model:")
        } else {
            v.contains(phrase)
        }
    })
}

/// How many worlds run side by side: each is its own scratch dir, broker,
/// device, endpoint thread and bridge, and nothing of one reaches another.
/// Four, beside the test binary's own test per core: more lanes than that buy
/// a run no wall time and spend more CPU, most of it the kernel's (each world's
/// socket rendezvous).
const WORKERS: usize = 4;

/// `job` for every item, [`WORKERS`] at a time from ONE QUEUE — a worker takes
/// the next item as it frees, so a long run holds no batch behind it — and
/// the answers in item order, the same answers however the threads ran. A
/// job's panic is the caller's, with its own payload.
fn par_map<T: Sync, R: Send>(items: &[T], job: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let answers: Vec<std::sync::Mutex<Option<R>>> =
        items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS.min(items.len()))
            .map(|_| {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let answer = job(item);
                    *super::lock(&answers[i]) = Some(answer);
                })
            })
            .collect();
        for worker in workers {
            if let Err(payload) = worker.join() {
                std::panic::resume_unwind(payload);
            }
        }
    });
    answers
        .into_iter()
        .map(|a| {
            a.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("every item ran")
        })
        .collect()
}

/// Run `opts` for every seed in `seeds` ([`par_map`]), the outcomes in seed
/// order.
fn run_all(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    seeds: std::ops::RangeInclusive<u64>,
) -> Vec<Outcome> {
    let seeds: Vec<u64> = seeds.collect();
    par_map(&seeds, |&seed| run(&opts(seed)))
}

/// THE COMMITTED WORLD over seeds `1..=COMMITTED_SEEDS`, run once for every test that
/// reads it — its laws ([`every_seed_holds_every_law`]), the model's Tier-1
/// ([`the_round_refines_its_exactly_once_model`]) and the replay
/// of a passing seed: a seed is the same run however often it is asked for
/// ([`a_failing_seed_replays_exactly`]), so asking again only costs a run.
fn committed() -> &'static [Outcome] {
    static RUNS: std::sync::OnceLock<Vec<Outcome>> = std::sync::OnceLock::new();
    RUNS.get_or_init(|| run_all(&Opts::seed, 1..=COMMITTED_SEEDS))
}

/// Whether `out` breaks `law` ([`breaks`]).
fn breaks_law(out: &Outcome, law: &str) -> bool {
    out.violations.iter().any(|v| breaks(v, law))
}

/// The first seed in `1..=limit` whose run under `opts` is `hit`'s: the seeds
/// go out in order through [`par_map`]'s queue and none above a hit is
/// started, so every seed below the lowest hit has run when the queue ends —
/// the same seed however the threads ran.
fn first_within(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    limit: u64,
    hit: &(dyn Fn(&Outcome) -> bool + Sync),
) -> Option<Outcome> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let lowest = AtomicU64::new(u64::MAX);
    let seeds: Vec<u64> = (1..=limit).collect();
    par_map(&seeds, |&seed| {
        if seed > lowest.load(Ordering::Relaxed) {
            return None;
        }
        let out = run(&opts(seed));
        hit(&out).then(|| {
            lowest.fetch_min(seed, Ordering::Relaxed);
            out
        })
    })
    .into_iter()
    .flatten()
    .next()
}

/// A WITNESSED SEARCH: the run of `opts` at `witness` — the seed
/// [`first_within`] found, kept beside its search so a control is one world
/// rather than a search of hundreds — when it is `hit`'s, else that search of
/// `1..=limit`, naming the seed it found so the witness can move there. The
/// claim is the search's either way: some seed within `limit` (or the
/// witness) is `hit`'s.
fn witnessed(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    witness: u64,
    limit: u64,
    hit: &(dyn Fn(&Outcome) -> bool + Sync),
) -> Option<Outcome> {
    assert!(
        (1..=limit).contains(&witness),
        "witness seed {witness} is outside its search 1..={limit}: it would widen the claim"
    );
    let out = run(&opts(witness));
    if hit(&out) {
        return Some(out);
    }
    let found = first_within(opts, limit, hit);
    eprintln!(
        "STALE WITNESS: seed {witness} no longer shows it; the first seed within {limit} that \
         does is {:?}",
        found.as_ref().map(|o| o.seed)
    );
    found
}

/// [`witnessed`] with `law` broken ([`breaks`]) as the hit.
fn breaking_at(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    law: &str,
    witness: u64,
    limit: u64,
) -> Option<Outcome> {
    witnessed(opts, witness, limit, &|out| breaks_law(out, law))
}

/// `o` with every rule switched back on: the same world, seed and tier.
fn with_every_rule(mut o: Opts) -> Opts {
    o.mutants.clear();
    o.ep = EpRules::default();
    o.table = Escalation::aterm();
    o.arm = ArmMutant::None;
    o
}

/// **EACH RULE SWITCHED OFF IS CAUGHT** by the law it exists for, at some seed
/// — and with the rule on, the same world and seed keep that law: the failure
/// is the rule's.
#[test]
fn each_rule_switched_off_is_caught() {
    type Switch = Box<dyn Fn(u64) -> Opts + Sync>;
    let rules: Vec<(&str, Switch, &str, u64)> = vec![
        (
            "the endpoint takes one row per offset",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.ep = EpRules {
                    dedup: false,
                    ..EpRules::default()
                };
                o
            }),
            "dup delivery",
            1,
        ),
        (
            "the endpoint keeps what lane 3's continues= keeps (a lane-2 endpoint forgets all)",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Ordered);
                o.ep = EpRules {
                    lane: 2,
                    ..EpRules::default()
                };
                o
            }),
            "dup delivery",
            2,
        ),
        // ONE RULE, THREE LAWS, each a control of its own: the endpoint that
        // keeps the offsets a move took answers a later record at a reused
        // offset as the old one's row — the model's tap sees the round leave
        // FabricExactlyOnce; the no-loss check sees a record on the bus that
        // never reached its ring; the receipt check sees an answer naming the
        // record that offset held before.
        (
            "the endpoint forgets the offsets a move took (the model's tap)",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Relaxed);
                o.ep = EpRules {
                    forgets: false,
                    ..EpRules::default()
                };
                o
            }),
            "model:",
            98,
        ),
        (
            "the endpoint forgets the offsets a move took (no loss)",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Relaxed);
                o.ep = EpRules {
                    forgets: false,
                    ..EpRules::default()
                };
                o
            }),
            "lost:",
            256,
        ),
        (
            "the endpoint forgets the offsets a move took (receipts)",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Relaxed);
                o.ep = EpRules {
                    forgets: false,
                    ..EpRules::default()
                };
                o
            }),
            "not its ask",
            66,
        ),
        (
            "the inbox commit waits for the record's delivery",
            Box::new(|s| Opts::seed(s).with(Mutant::CommitBeforeDelivery)),
            "model:",
            4,
        ),
        (
            "a session ahead of the bridge's witness keeps below the chain's fork (F3)",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.ep = EpRules {
                    chain: false,
                    ..EpRules::default()
                };
                o
            }),
            "dup delivery",
            1730,
        ),
        (
            "the attach reads the lineage of a moved generation",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::IgnoreLineage);
                o.tier = Some(Tier::Ordered);
                o
            }),
            "dup delivery",
            2,
        ),
        (
            "a post's retry reuses its pinned sequence",
            Box::new(|s| Opts::seed(s).with(Mutant::FreshSeqOnRetry)),
            "dup post",
            1,
        ),
        (
            "a receipt's retry reuses its pinned sequence",
            Box::new(|s| Opts::seed(s).with(Mutant::FreshReceiptSeq)),
            "answered 2 times",
            16,
        ),
        (
            "one post in flight per sender",
            Box::new(|s| Opts::seed(s).with(Mutant::PipelineSender)),
            "order:",
            5,
        ),
        // THE EXACTLY-ONCE WAIT aterm-0 KEEPS where the loop no longer waits
        // for its acks: the outbox peek reads every post in flight first. Run
        // where the broker holds acks, so a drain meets a post still owed.
        (
            "the outbox is peeked only once every post in flight is answered",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::PeekPastOwedPost);
                o.hold_acks = true;
                o
            }),
            "dup post",
            1,
        ),
        // AND THE WAIT A SENDER NEEDS: an idle loop reads its posts' answers
        // waiting before it parks, so `outbox sent` never waits out the idle
        // backstop's grace. Run where the broker holds acks, so an idle
        // moment meets a post's answer still in its flush.
        (
            "an idle loop reads its posts' answers before it parks",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::IdlePostLeftToBackstop);
                o.hold_acks = true;
                o
            }),
            "idle:",
            1,
        ),
        (
            "the incarnation lease is trusted only in the boot that wrote it",
            Box::new(|s| {
                // The lease's boot is judged across a power cut, and the bus's
                // own fence (`max(local, bus) + 1`) holds wherever a record of
                // the incarnation survived: only a world whose log also loses
                // power alone shows the lease alone standing between a reused
                // incarnation and the bus ([`both_ways`], finding F2's world,
                // which with every rule on breaks no law).
                // Seed 149 until the next boot's first bridge retired the
                // entries a cut left (finding F2), which moved every schedule
                // after a power cut; 546 until the mirror's durable copies,
                // whose directory flushes pin `inc-lease` (`RARE_SEEDS`);
                // 4023 until sim item S6, 8483 since. Sim item S7's floor,
                // written again by each open before its first batch, is one
                // more such flush, and with it 8483 no longer rolls a lease
                // back with the lease alone off. So the floor's admission is
                // switched off too, as the incidental flush it is
                // (`Mutant::TrustTheFloorThatReads`): at 8483 neither switched
                // off alone breaks the law, both do, and with every rule on
                // the same seed keeps it.
                both_ways(s)
                    .with(Mutant::LeaseAcrossBoots)
                    .with(Mutant::TrustTheFloorThatReads)
            }),
            "cap-compromised",
            8483,
        ),
        (
            "a move of the log drops the mirror's pinned batch",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::KeepMirrorPinAcrossMove);
                o.mirror = true;
                o.tier = Some(Tier::Relaxed);
                o
            }),
            "mirror:",
            68,
        ),
        (
            "a replaced log forgets the mirror's cursors (aterm-gui's `forget_landed`)",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.mirror = true;
                o.tier = Some(Tier::Relaxed);
                o.ep = EpRules {
                    mirror_forgets: false,
                    ..EpRules::default()
                };
                o
            }),
            "mirror:",
            12,
        ),
        (
            "a relaunched bridge republishes the mirror's pinned batch at its sequences",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::FreshMirrorTake);
                o.mirror = true;
                o.tier = Some(Tier::Strict);
                o
            }),
            "never moved",
            1,
        ),
        (
            "`session-exited` publishes the session's `exited` row",
            Box::new(|s| Opts::seed(s).with(Mutant::SkipExitedRow)),
            "presence:",
            194,
        ),
        (
            "a ghost row is swept",
            Box::new(|s| Opts::seed(s).with(Mutant::NoGhostSweep)),
            "presence:",
            3,
        ),
        // FINDING F4'S LAW: a later bridge of a state dir retires the ghosts
        // its earlier bridges left, by the dir's mark on their rows — the rule
        // the committed worlds' ghosts are all retired by, which unread leaves
        // the ones whose writer's successors all died inside the five minutes.
        (
            "a later bridge reads its state dir's mark on a row (F4)",
            Box::new(|s| Opts::seed(s).with(Mutant::ReadNoMark)),
            "presence:",
            2,
        ),
        // THE WITNESS, where the mark cannot stand in for it: a SIBLING'S
        // death, a row under another state dir's mark, whose will the attach
        // read.
        (
            "the attach records the predecessor's will it read",
            Box::new(|s| Opts::with_a_sibling(s).with(Mutant::IgnoreWitnessedWill)),
            "presence:",
            4,
        ),
        // AND A SIBLING'S ROW IS NOT THIS DIR'S: a sweep that takes every
        // incarnation below its own for one of the dir's earlier bridges
        // retires it.
        (
            "a lower incarnation's row under another state dir's mark is not its own",
            Box::new(|s| Opts::with_a_sibling(s).with(Mutant::RetireEveryEarlierIncarnation)),
            "presence:",
            1,
        ),
        // SIM ITEM S6: what ends the owner's entry for the torn files is the
        // count the quarantine HOLDS, at every start — not what the start
        // moved, which a later boot that moves fewer understates.
        (
            "a bridge tells aterm what its quarantine holds",
            Box::new(|s| Opts::seed(s).with(Mutant::TellMovedNotHeld)),
            "quarantine:",
            1,
        ),
        // SIM ITEM S7, THE DEVICE'S WRITE ERRORS: each rule a failed write
        // needs, switched off where the committed world's disk fails
        // ([`Opts::write_errors`], every other eight seeds).
        (
            "an offset below the self-lane window is not added to it",
            Box::new(|s| Opts::seed(s).with(Mutant::RememberBelowTheWindow)),
            "forged-self",
            92,
        ),
        (
            "the recovery a write error stopped is done before the run record",
            Box::new(|s| Opts::seed(s).with(Mutant::BeginRunPastAFailedRecovery)),
            "liveness",
            47,
        ),
        (
            "the mirror judges its pin once the recovery reconciled it",
            Box::new(|s| Opts::seed(s).with(Mutant::JudgePinBeforeRecovery)),
            "mirror",
            31,
        ),
        (
            "an own record is delivered only once its in-flight entry is gone",
            Box::new(|s| Opts::seed(s).with(Mutant::DeliverPastAStandingEntry)),
            "dup delivery",
            28,
        ),
        (
            "a mirror floor covers a batch only once this open made it durable",
            Box::new(|s| Opts::seed(s).with(Mutant::TrustTheFloorThatReads)),
            "mirror total",
            1039,
        ),
        // The witness's write failing after a move, and then a later move:
        // the world that loses power both ways.
        (
            "an attach delivers on a new log only once its witness is written",
            Box::new(|s| both_ways(s).with(Mutant::AttachPastAnUnwrittenWitness)),
            "dup delivery",
            713,
        ),
        // A replayed post whose drain's try fails, and then a move: the world
        // that loses power both ways with the disk failing in every seed.
        (
            "a record the attach published again is delivered once its post is retired",
            Box::new(|s| both_ways_erring(s).with(Mutant::DeliverBeforeTheDrain)),
            "dup delivery",
            18017,
        ),
        // A clean stop made while the recovery is owed: a world where aterm
        // quits, and the disk fails, in every seed.
        (
            "a run whose recovery is owed makes no clean stop",
            Box::new(|s| erring_quitting(s).with(Mutant::CleanStopOverAnOwedRecovery)),
            "mirror",
            1535,
        ),
        // A recovery tried again after a try whose unlink failed: the world
        // that loses power both ways with the disk failing in every seed.
        (
            "a recovery tried again moves each file once",
            Box::new(|s| both_ways_erring(s).with(Mutant::CopyOnEveryTry)),
            "quarantine copies:",
            1,
        ),
        (
            "the broker escalates aterm's table (the oracle keeps it)",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Relaxed);
                o.table = Escalation::none();
                o
            }),
            "P3e",
            1,
        ),
        (
            "a power loss keeps the durable head",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Ordered);
                o.arm = ArmMutant::CutBelowDurable;
                o
            }),
            "P3",
            1,
        ),
        (
            "a power loss that loses acked records moves the generation",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Relaxed);
                o.arm = ArmMutant::LoseWithoutMove;
                o
            }),
            "P4",
            1,
        ),
        (
            "a move's lineage stops at what the log kept",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.tier = Some(Tier::Relaxed);
                o.arm = ArmMutant::LineageAtHead;
                o
            }),
            "P7",
            1,
        ),
    ];
    // EACH ROW'S LAST NUMBER IS ITS WITNESS: the first seed the search found
    // breaking its law (what the test prints, `OFF: … -> seed n`), so a run is
    // two worlds a rule rather than its search ([`witnessed`]). THE RULES
    // WHOSE LAW A SEED RARELY BREAKS get a longer search, named exactly (never
    // matched by a word of their name). The lease is backed up by the bus's
    // own fence in nearly every interleaving (its own conformance walk is
    // `state.rs`'s `the_incarnation_lease_refines_its_model`). F3's chain
    // needs a kill on the `link log` line between two bus-only losses. The
    // mirror's pin needs a move between the acks that put a batch on the bus
    // and the landing after them, taking what the batch put there (the
    // bridge's own model of the batch is `bridge_mirror_tests.rs`'s
    // `LogMirror` Tier-1). The no-loss check and the receipt check see a
    // forgetful endpoint only where a later record lands at an offset a move
    // took AND the endpoint is asked for it; the model's tap sees the same
    // rule sooner. The `exited` row's control needs a session to leave under
    // the running bridge's own incarnation and the world to settle with its
    // row still standing: seed 28 of the first 64 until aterm-0's review fixes
    // moved every schedule (a refused commit retried, an idle loop waiting for
    // its posts' answers), seed 66 until a later bridge of a state dir
    // retired its predecessors' ghosts (finding F4), seed 194 since. The
    // self-lane window's needs a move after an `acked` write failed and the
    // replay of the entry it kept: seed 28 of the first 64 on sim item S7's
    // own base, 92 since main's per-open run admission (one more durable
    // write at every bridge start) moved every schedule of an erring seed.
    const RARE: &[&str] = &[
        "an offset below the self-lane window is not added to it",
        "a run whose recovery is owed makes no clean stop",
        "a record the attach published again is delivered once its post is retired",
        "a mirror floor covers a batch only once this open made it durable",
        "an attach delivers on a new log only once its witness is written",
        "`session-exited` publishes the session's `exited` row",
        "the incarnation lease is trusted only in the boot that wrote it",
        "the endpoint forgets the offsets a move took (the model's tap)",
        "a session ahead of the bridge's witness keeps below the chain's fork (F3)",
        "a move of the log drops the mirror's pinned batch",
        "the endpoint forgets the offsets a move took (no loss)",
        "the endpoint forgets the offsets a move took (receipts)",
    ];
    for rare in RARE {
        assert!(
            rules.iter().any(|(rule, _, _, _)| rule == rare),
            "a rare rule names no rule: {rare}"
        );
    }
    // Every rule's two worlds through one queue: the rule off at its seed,
    // then every rule on at the same seed.
    let shown = par_map(&rules, |(rule, opts, law, witness)| {
        let limit = if RARE.contains(rule) {
            RARE_SEEDS
        } else {
            CONTROL_SEEDS
        };
        let off = breaking_at(opts.as_ref(), law, *witness, limit);
        let on = off
            .as_ref()
            .map(|out| run(&with_every_rule(opts(out.seed))));
        (off, on, limit)
    });
    let mut missed = Vec::new();
    for ((rule, _, law, _), (off, on, limit)) in rules.iter().zip(&shown) {
        match (off, on) {
            (Some(out), Some(committed)) => {
                eprintln!(
                    "OFF: {rule} -> seed {} breaks `{law}`: {}",
                    out.seed,
                    out.violations
                        .iter()
                        .find(|v| breaks(v, law))
                        .map_or("", String::as_str)
                );
                assert!(
                    !breaks_law(committed, law),
                    "with the rule on, the same seed still breaks `{law}`: {}",
                    report(committed)
                );
            }
            _ => missed.push(format!("{rule} (expected `{law}` within {limit} seeds)")),
        }
    }
    assert!(
        missed.is_empty(),
        "no seed caught these rules switched off:\n  {}",
        missed.join("\n  ")
    );
}

/// THE SEEDS A WIDER SEARCH FOUND BREAKING A LAW (the committed world over
/// seeds 65..=8064 at 400 steps, and 1..=400 at 1500), each a
/// defect fixed since, kept so the fix stays one. `(seed, steps, what broke)`:
/// the bridge, before its fix, broke these; every one now holds every law.
/// Each was found on a disk that failed no write, and is run on one
/// ([`on_a_clean_disk`]): sim item S7's write errors, on in every other eight
/// seeds of the committed world, would draw another schedule for half of
/// them, and their row would no longer replay the run that found it.
const FOUND: &[(u64, usize, &str)] = &[
    (102, 1500, "a refused `exited` row nothing wrote again"),
    (110, 1500, "a refused `exited` row nothing wrote again"),
    (909, 400, "a refused `exited` row nothing wrote again"),
    (1014, 400, "a refused `exited` row nothing wrote again"),
    (1099, 400, "a refused `exited` row nothing wrote again"),
    (1437, 400, "a refused post a power cut brought back"),
    (
        1782,
        400,
        "the tap read a commit owed on a replaced log as the offset's",
    ),
    (1803, 400, "a refused `exited` row nothing wrote again"),
    (1941, 400, "a refused post a power cut brought back"),
    (1971, 400, "a refused `exited` row nothing wrote again"),
    (2150, 400, "a refused `exited` row nothing wrote again"),
    (
        2324,
        400,
        "a receipt journaled with the link down wedged every attach",
    ),
    (2786, 400, "a refused `exited` row nothing wrote again"),
    (3044, 400, "a refused `exited` row nothing wrote again"),
    (
        3113,
        400,
        "a dropped receipt a power cut brought back, judged at the newest fork",
    ),
    (3786, 400, "a refused `exited` row nothing wrote again"),
    (
        4656,
        400,
        "the tap read a commit owed on a replaced log as the offset's",
    ),
    (4690, 400, "a refused `exited` row nothing wrote again"),
    (5172, 400, "a refused `exited` row nothing wrote again"),
    (
        5354,
        400,
        "the tap read a commit owed on a replaced log as the offset's",
    ),
    (5438, 400, "a refused `exited` row nothing wrote again"),
    (5844, 400, "a refused `exited` row nothing wrote again"),
    (6257, 400, "a refused post a power cut brought back"),
    (
        6430,
        400,
        "the tap read a commit owed on a replaced log as the offset's",
    ),
    (7042, 400, "a refused `exited` row nothing wrote again"),
    (7143, 400, "a refused `exited` row nothing wrote again"),
    (7432, 400, "a refused `exited` row nothing wrote again"),
    (
        7668,
        400,
        "the tap read a commit owed on a replaced log as the offset's",
    ),
    (
        7950,
        400,
        "the tap read a commit owed on a replaced log as the offset's",
    ),
    (7963, 400, "a refused `exited` row nothing wrote again"),
];

/// THE SEEDS THE DEVICE'S WRITE ERRORS FOUND BREAKING A LAW (sim item S7: the
/// committed world, whose every other eight seeds fail writes, over 1..=4000
/// at 400 steps; seeds 27, 28, 45, 47 and 63 broke a law too, and are among
/// the committed world's own), each a defect fixed since: as [`FOUND`], but
/// run in the committed world itself ([`Opts::seed`]), whose write errors
/// are on for every one of these seeds.
const FOUND_ERRING: &[(u64, usize, &str)] = &[
    (
        75,
        400,
        "a pin judged before the reconcile a write error stopped",
    ),
    (
        95,
        400,
        "a pin judged before the reconcile a write error stopped",
    ),
    (
        107,
        400,
        "a pin judged before the reconcile a write error stopped",
    ),
    (122, 400, "a deduped replay moved the self-lane window down"),
    (
        125,
        400,
        "a sweep a write error stopped, closed by the run record",
    ),
    (142, 400, "a deduped replay moved the self-lane window down"),
    (154, 400, "a deduped replay moved the self-lane window down"),
    (168, 400, "a deduped replay moved the self-lane window down"),
    (
        171,
        400,
        "a pin judged before the reconcile a write error stopped",
    ),
    (
        185,
        400,
        "a sweep a write error stopped, closed by the run record",
    ),
    (
        187,
        400,
        "a pin judged before the reconcile a write error stopped",
    ),
    (
        191,
        400,
        "a pin judged before the reconcile a write error stopped",
    ),
    (443, 400, "a floor whose flush failed trusted as it read"),
    (
        746,
        400,
        "a record delivered while its in-flight entry stood",
    ),
    (
        1240,
        400,
        "a record delivered while its in-flight entry stood",
    ),
    (
        1530,
        400,
        "a record delivered while its in-flight entry stood",
    ),
];

/// THE SEEDS A WIDER SEARCH OF THE WORLD THAT LOSES POWER BOTH WAYS FOUND
/// BREAKING A LAW ([`both_ways`], 1..=2000 at 400 steps, and 7470 of a search
/// of 2001..=14000 the lease's control ran), each a defect fixed since:
/// `(seed, steps, what broke)`, as [`FOUND`].
const FOUND_BOTH_WAYS: &[(u64, usize, &str)] = &[
    (
        1482,
        400,
        "an attach delivered on a new log whose witness it could not write",
    ),
    (
        7470,
        400,
        "an attach delivered on a log with no witness, one a power cut tore",
    ),
];

/// THE SEEDS THE WIDE SEARCH OF THE WORLD THAT LOSES POWER BOTH WAYS WITH THE
/// DISK FAILING IN EVERY SEED FOUND BREAKING A LAW ([`both_ways_erring`],
/// 1..=2000 at 400 steps), each a defect fixed since: as [`FOUND`].
const FOUND_BOTH_WAYS_ERRING: &[(u64, usize, &str)] = &[(
    1844,
    400,
    "a record the attach published again delivered while its post was queued",
)];

/// One found seed with the world it was found in: `(world, seed, steps, what
/// broke)`.
type FoundIn = (fn(u64) -> Opts, u64, usize, &'static str);

/// THE SEEDS THE WIDE SEARCH OF THE WORLDS WHERE ATERM QUITS FOUND BREAKING A
/// LAW ([`erring_quitting`] and [`both_ways_erring_quitting`], 1..=2000 at 400
/// steps each), each a defect fixed since: `(world, seed, steps, what broke)`.
const FOUND_QUITTING: &[FoundIn] = &[
    (
        erring_quitting,
        863,
        400,
        "a clean stop over a recovery owed vouched for an unreconciled pin",
    ),
    (
        both_ways_erring_quitting,
        406,
        400,
        "a clean stop over a recovery owed vouched for a torn pin",
    ),
    (
        both_ways_erring_quitting,
        1682,
        400,
        "a clean stop over a recovery owed vouched for a torn file",
    ),
];

/// **THE SEEDS THAT BROKE A LAW HOLD EVERY LAW** ([`FOUND`], on a clean disk,
/// [`FOUND_ERRING`], [`FOUND_BOTH_WAYS`], [`FOUND_BOTH_WAYS_ERRING`],
/// [`FOUND_QUITTING`]): a receipt journaled while the link was down that
/// wedged every later attach; a refused post or a dropped receipt a power cut
/// brought back and the next attach published; a refused `exited` row nothing
/// wrote again; the model's tap reading a commit owed on a replaced log as the
/// offset's own; and what the device's write errors found (sim item S7): the
/// self-lane window moved down by a deduped replay, a sweep or a pin's
/// reconcile a write error stopped and the run record closed, a pin judged
/// before it was reconciled, a record delivered with its in-flight entry
/// standing, a floor trusted as it read, and an attach that delivered on a
/// new log whose witness it could not write.
#[test]
fn the_seeds_that_broke_a_law_hold_every_law() {
    let found: Vec<FoundIn> =
        FOUND
            .iter()
            .map(|&(seed, steps, what)| (on_a_clean_disk as fn(u64) -> Opts, seed, steps, what))
            .chain(
                FOUND_ERRING
                    .iter()
                    .map(|&(seed, steps, what)| (Opts::seed as fn(u64) -> Opts, seed, steps, what)),
            )
            .chain(
                FOUND_BOTH_WAYS
                    .iter()
                    .map(|&(seed, steps, what)| (both_ways as fn(u64) -> Opts, seed, steps, what)),
            )
            .chain(FOUND_BOTH_WAYS_ERRING.iter().map(|&(seed, steps, what)| {
                (both_ways_erring as fn(u64) -> Opts, seed, steps, what)
            }))
            .chain(FOUND_QUITTING.iter().copied())
            .collect();
    let outs = par_map(&found, |&(world, seed, steps, what)| {
        let mut o = world(seed);
        o.steps = steps;
        (run(&o), what)
    });
    let failures: Vec<String> = outs
        .iter()
        .filter(|(out, _)| !out.violations.is_empty())
        .map(|(out, what)| format!("(once: {what}) {}", report(out)))
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} found seeds break a law again:\n{}",
        failures.len(),
        found.len(),
        failures.join("\n")
    );
}

/// **A FAILING SEED REPLAYS EXACTLY**: the same seed, run twice, is the same
/// run — every scheduled choice, every request the bridge made of the broker
/// and the endpoint, every kill and every image a power cut left, in order —
/// and fails the same way. A passing seed too.
#[test]
fn a_failing_seed_replays_exactly() {
    let failing = breaking_at(
        &|s| Opts::seed(s).with(Mutant::FreshSeqOnRetry),
        "dup post",
        1,
        CONTROL_SEEDS,
    )
    .expect("a seed the pin's negative control fails");
    let again = run(&Opts::seed(failing.seed).with(Mutant::FreshSeqOnRetry));
    assert_eq!(failing.violations, again.violations, "the same failure");
    assert_eq!(
        failing.trace.len(),
        again.trace.len(),
        "the same trace length"
    );
    if let Some(i) = (0..failing.trace.len()).find(|&i| failing.trace[i] != again.trace[i]) {
        panic!(
            "seed {} diverged at trace line {i}: {:?} vs {:?}",
            failing.seed, failing.trace[i], again.trace[i]
        );
    }
    let a = committed()
        .iter()
        .find(|o| o.violations.is_empty())
        .expect("a passing seed");
    let b = run(&Opts::seed(a.seed));
    assert_eq!(a.trace, b.trace, "a passing seed replays exactly too");
    assert!(
        a.trace.len() > 500,
        "a real run: {} trace lines",
        a.trace.len()
    );
}

/// **FINDING F1, DECIDED: A TORN STATE FILE AFTER A POWER CUT IS SET ASIDE, AND
/// THE ATTACH GOES ON** ([`super::device`]'s module doc). The committed device
/// is POSIX's worst case — a power cut may TEAR any file it did not find
/// flushed, to nothing or part-way ([`Opts::seed`]) — and the committed worlds'
/// bridges set what it tore aside (`crate::state::StateDir::set_aside_torn`:
/// after an unclean stop in an earlier boot, an in-flight entry, reservation,
/// key file or refill floor that is not provably whole is moved into the state
/// dir's quarantine; a torn mirror pin too, once its durable copies are on the
/// media, [`a_torn_mirror_pin_after_a_power_cut_is_set_aside_and_the_mirror_resumes`])
/// and break no law, the liveness law included
/// ([`every_seed_holds_every_law`] runs them; this names the ones that tore
/// something).
///
/// NEGATIVE CONTROLS: the old refusal (`Mutant::RefuseTornAfterPowerCut`, a
/// torn file refused where it is read, at every attach of every later boot)
/// wedges some seed's bridge, which never attaches again; the same seed's
/// committed world breaks no law; and the same seed under the old refusal with
/// a device that never tears breaks none either — the wedge is the tear's. And
/// the first fix's sweep (`Mutant::SweepByTheReaderAlone`, a file whole when
/// its reader takes it) wedges some seed too: a reply's in-flight entry cut
/// where it still reads, with no witness, is kept and refused at every attach.
#[test]
fn a_torn_state_file_after_a_power_cut_is_set_aside_and_the_attach_goes_on() {
    let torn: Vec<&Outcome> = committed()
        .iter()
        .filter(|out| out.stats.set_aside > 0)
        .collect();
    assert!(
        !torn.is_empty(),
        "PRECONDITION: some committed seed's power cut tore a file a bridge set aside"
    );
    for out in &torn {
        assert!(
            out.violations.is_empty(),
            "a world whose bridge set a torn file aside breaks a law: {}",
            report(out)
        );
    }
    eprintln!(
        "F1's law: {} of {COMMITTED_SEEDS} committed seeds set {} torn files aside and broke no law",
        torn.len(),
        torn.iter().map(|out| out.stats.set_aside).sum::<u64>()
    );
    let refusing = |s| Opts::seed(s).with(Mutant::RefuseTornAfterPowerCut);
    let wedged = breaking_at(&refusing, "liveness", 5, CONTROL_SEEDS)
        .expect("NEGATIVE CONTROL: with the old refusal, a torn file wedges some seed's attach");
    eprintln!(
        "the old refusal at seed {}: {}",
        wedged.seed,
        wedged.violations.join("; ")
    );
    assert!(
        run(&Opts::seed(wedged.seed)).violations.is_empty(),
        "the committed world of the same seed breaks no law"
    );
    let mut whole = refusing(wedged.seed);
    whole.torn_device = false;
    assert!(
        run(&whole).violations.is_empty(),
        "the same seed under the old refusal with a device that never tears breaks no law"
    );
    let by_the_reader = |s| Opts::seed(s).with(Mutant::SweepByTheReaderAlone);
    let kept = breaking_at(&by_the_reader, "liveness", 27, RARE_SEEDS).expect(
        "NEGATIVE CONTROL: a sweep that trusts the reader alone keeps a cut reply that wedges",
    );
    eprintln!(
        "the reader-alone sweep at seed {}: {}",
        kept.seed,
        kept.violations.join("; ")
    );
    assert!(
        run(&Opts::seed(kept.seed)).violations.is_empty(),
        "the committed world of the same seed breaks no law"
    );
}

/// **A TORN MIRROR PIN AFTER A POWER CUT IS SET ASIDE, AND THE MIRROR RESUMES**
/// (rulings Q6 and Q13 across a power cut). The pin (`logpin`) holds the
/// node's alias key and its lifetime total, which no reboot ends, so a torn
/// one used to be kept and stop the mirror for good. Each now has a durable
/// copy on the media before each atomic batch pin or send — the key in
/// `logkey`, before the first take names it; a floor at or above the total
/// and the batch in `logfloor`. The current version fence is first written durably, before
/// those copies, and a new open reestablishes it. So the next boot's first
/// bridge sets a torn pin aside like any torn file
/// (`crate::state::StateDir::reconcile_logpin`, which also raises a
/// rolled-back pin's total to the floor), and the mirror resumes. Over the
/// committed seeds, every world a power cut tore the pin in breaks no law:
/// not the mirror's completeness, which a torn pin used to excuse; not one
/// alias key over the node's life; not a total never below what any pin was
/// written with; not a stop for the pin at any step.
///
/// NEGATIVE CONTROLS: the old rule (`Mutant::KeepTornPin`, the torn pin kept)
/// stops some seed's mirror for its pin, while the same seed's committed world
/// does not, nor the old rule on a device that never tears — the stop is the
/// tear's. And WHY THE COPIES: resumed with the key off the pin alone
/// (`Mutant::KeyInThePinAlone`), some seed's takes name a second alias key;
/// with the total off the pin alone (`Mutant::TotalInThePinAlone`), some
/// seed's mirror counts fewer bytes than a pin was written with. The same
/// seeds' committed worlds break neither.
#[test]
fn a_torn_mirror_pin_after_a_power_cut_is_set_aside_and_the_mirror_resumes() {
    let torn: Vec<&Outcome> = committed()
        .iter()
        .filter(|out| out.stats.pins_torn > 0)
        .collect();
    assert!(
        !torn.is_empty(),
        "PRECONDITION: some committed seed's power cut tore the mirror's pin"
    );
    for out in &torn {
        assert!(
            out.violations.is_empty(),
            "a world whose power cut tore the mirror's pin breaks a law: {}",
            report(out)
        );
    }
    assert!(
        torn.iter().any(|out| out.stats.pins_set_aside > 0),
        "PRECONDITION: a bridge set a torn pin aside"
    );
    assert!(
        torn.iter().any(|out| out.stats.mirrored > 0),
        "and the mirror resumed: a later boot's sessions are on the bus"
    );
    eprintln!(
        "the torn pin's law: {} of {COMMITTED_SEEDS} committed seeds tore the pin {} times, set {} aside, \
         and broke no law",
        torn.len(),
        torn.iter().map(|out| out.stats.pins_torn).sum::<u64>(),
        torn.iter().map(|out| out.stats.pins_set_aside).sum::<u64>()
    );
    let keeping = |s| Opts::seed(s).with(Mutant::KeepTornPin);
    let stopped = breaking_at(&keeping, "stopped for its pin", 3, CONTROL_SEEDS)
        .expect("NEGATIVE CONTROL: with the torn pin kept, some seed's mirror stops for it");
    eprintln!(
        "the torn pin kept at seed {}: {}",
        stopped.seed,
        stopped.violations.join("; ")
    );
    assert!(
        run(&Opts::seed(stopped.seed)).violations.is_empty(),
        "the committed world of the same seed breaks no law"
    );
    // The stop is the tear's: with the torn pin kept on a device that never
    // tears, no committed seed's mirror stops for its pin, and the witness
    // breaks no law at all.
    let untorn = |s| {
        let mut o = keeping(s);
        o.torn_device = false;
        o
    };
    for out in run_all(&untorn, 1..=SEEDS) {
        assert!(
            !breaks_law(&out, "stopped for its pin"),
            "seed {}: with the torn pin kept, on a device that never tears, the mirror stopped \
             for its pin: {}",
            out.seed,
            report(&out)
        );
    }
    assert!(
        run(&untorn(stopped.seed)).violations.is_empty(),
        "the same seed with the torn pin kept, on a device that never tears, breaks no law"
    );
    for (mutant, law, witness) in [
        (Mutant::KeyInThePinAlone, "alias keys", 7),
        (Mutant::TotalInThePinAlone, "mirror total", 3),
    ] {
        let off = breaking_at(
            &move |s| Opts::seed(s).with(mutant),
            law,
            witness,
            CONTROL_SEEDS,
        )
        .unwrap_or_else(|| {
            panic!(
                "NEGATIVE CONTROL: with {mutant:?}, no seed within {CONTROL_SEEDS} breaks `{law}`"
            )
        });
        eprintln!(
            "{mutant:?} at seed {}: {}",
            off.seed,
            off.violations
                .iter()
                .find(|v| breaks(v, law))
                .map_or("", String::as_str)
        );
        assert!(
            !breaks_law(&run(&Opts::seed(off.seed)), law),
            "with the copy read, the same seed keeps `{law}`"
        );
    }
}

/// **FINDING F4, DECIDED: A GHOST ROW IS RETIRED BY A LATER BRIDGE OF ITS STATE
/// DIR** ([`super::world`]'s presence oracle). A session that left with nobody
/// to say so — its tab closed while no bridge ran, its instance gone in a power
/// cut, or its `exited` row taken by a lossy cut — keeps a `state=live` row.
/// The sweep used to retire one only from the incarnation that wrote it or the
/// one whose will it witnessed on the node face, after five minutes of its own
/// sight; when that bridge died inside the five minutes, every later one was a
/// stranger to the row, which stood `live` until `aterm fabric doctor
/// --retire-ghosts`. Over 400 committed seeds, whose kills are far denser than
/// a desk's, rows left outnumbered rows retired about twenty to one.
///
/// Now every session row a bridge writes carries its state dir's MARK
/// (`crate::state::StateDir::mark`, read off the directory, never stored), and
/// a later bridge of the dir — the one bridge the dir's lock admits, so every
/// earlier one is dead — retires the lower incarnations' rows that carry it,
/// under the rules its own rows are retired by. The oracle demands every ghost row of the dir retired
/// once settled when written in the same boot. A different kernel boot is
/// deliberately another mark: a cloned machine can repeat device and inode,
/// so cross-boot ghosts without a witnessed will remain the operator's.
/// (The first decision, a chain of the incarnations the
/// dir took, kept on disk, also retired a copy's live rows under a number the
/// dir took too: [`a_sibling_that_takes_one_of_this_dirs_incarnations_is_never_retired`].)
///
/// NEGATIVE CONTROL: the mark unread (`Mutant::ReadNoMark`), some seed leaves
/// a ghost the running bridge may retire, while the same seed's committed world
/// does not (`each_rule_switched_off_is_caught`'s row).
#[test]
fn a_ghost_row_is_retired_by_a_later_bridge_of_its_state_dir() {
    let same_boot = |seed| Opts {
        power_cuts: false,
        ..Opts::seed(seed)
    };
    let outs = run_all(&same_boot, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let (retired, left): (u64, u64) = outs.iter().fold((0, 0), |(r, l), out| {
        (r + out.stats.ghosts_retired, l + out.stats.ghosts_left)
    });
    eprintln!("F4 over {SEEDS} same-boot seeds: {retired} ghost rows retired, {left} left");
    assert!(
        retired > 0,
        "PRECONDITION: the committed worlds made ghosts and retired them"
    );
    let left: Vec<String> = outs
        .iter()
        .filter(|out| out.stats.ghosts_left > 0)
        .map(|out| format!("seed {}: {}", out.seed, out.stats.ghosts_left))
        .collect();
    assert!(
        left.is_empty(),
        "ghost rows left to the operator: {}",
        left.join(", ")
    );
}

/// **A SIBLING'S ROW IS RETIRED ONLY BY THE ATTACH THAT WITNESSED ITS WILL**
/// (finding F4's other half, [`Opts::with_a_sibling`]). A copy of the state
/// dir runs a bridge under the same node id: another state dir, so another
/// mark on its rows. It attaches outranking this node's bridges, carries a
/// session, and mostly dies; its will fires unless this node outranked it
/// first. Over the same seeds every law holds, the sibling's row among them:
/// never retired by a bridge here that did not read its will, and retired by
/// the running one that did. Both happen: some world ends with the row
/// standing `live` under a bridge here that outranks it, and some with it
/// retired.
///
/// NEGATIVE CONTROLS (`each_rule_switched_off_is_caught`'s rows): a sweep that
/// takes every lower incarnation for its own retires the sibling's row; an
/// attach that records no witnessed will leaves the dead sibling's ghost.
#[test]
fn a_siblings_row_is_retired_only_by_the_attach_that_witnessed_its_will() {
    let outs = run_all(&Opts::with_a_sibling, 1..=SIBLING_SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {SIBLING_SEEDS} seeds with a sibling broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let kept: u64 = outs.iter().map(|out| out.stats.sibling_kept).sum();
    let retired: u64 = outs.iter().map(|out| out.stats.sibling_retired).sum();
    eprintln!(
        "a sibling over {SIBLING_SEEDS} seeds: its row live under an outranking bridge in {kept}, \
         retired in {retired}"
    );
    assert!(
        kept > 0,
        "PRECONDITION: some world's bridge outranked the sibling"
    );
    assert!(
        retired > 0,
        "PRECONDITION: some world's attach witnessed the sibling's will"
    );
}

/// **A SIBLING THAT TAKES ONE OF THIS DIR'S INCARNATIONS IS STILL ANOTHER
/// DIR'S** (finding F4's review, [`Opts::with_a_racing_sibling`]). An attach
/// here takes an incarnation and the broker refuses its will, so nothing of
/// it reaches the bus; a copy of the state dir, under the same node id, then
/// reads the node face before the next attach here has presented anything,
/// takes that very number, and lives on with a session under it. Over the
/// committed seeds the race happens, and no bridge here ever retires the
/// sibling's row: it witnessed no will of it, and the number it shares with
/// one this dir took says nothing about whose rows are under it.
///
/// NEGATIVE CONTROL: a sweep that takes every incarnation below its own for
/// one of this dir's retires the racing sibling's row at some seed.
#[test]
fn a_sibling_that_takes_one_of_this_dirs_incarnations_is_never_retired() {
    let outs = run_all(&Opts::with_a_racing_sibling, 1..=SEEDS);
    let raced: u64 = outs.iter().map(|out| out.stats.sibling_raced).sum();
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    eprintln!(
        "a racing sibling over {SEEDS} seeds: it took one of this dir's incarnations in {raced}, \
         {} broke a law",
        failures.len()
    );
    assert!(
        raced > 0,
        "PRECONDITION: some world's sibling took an incarnation this dir took"
    );
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds with a racing sibling broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let generous = |s| Opts::with_a_racing_sibling(s).with(Mutant::RetireEveryEarlierIncarnation);
    let out = (1..=CONTROL_SEEDS)
        .map(|s| run(&generous(s)))
        .find(|out| out.violations.iter().any(|v| v.contains("the sibling's")))
        .expect("NEGATIVE CONTROL: a generous sweep retires the racing sibling's row");
    eprintln!(
        "the generous sweep at seed {}: {}",
        out.seed,
        out.violations.join("; ")
    );

    // CURRENT incarnation alias: both cloned dirs read the old face before
    // either presents. A distinct sibling connection registers the same will
    // and publishes live first. This bridge registers its own will, collides
    // on its first node publish, closes that connection and fires its own
    // aliased will over the STILL-LIVE sibling. Its next attach must not take
    // that numeric incarnation as proof the sibling died.
    let alias = Opts::with_an_aliasing_sibling(1);
    let out = run(&alias);
    assert_eq!(out.stats.sibling_alias_collided, 1, "{}", report(&out));
    assert_eq!(out.stats.alias_ghost_retired, 1, "{}", report(&out));
    assert_eq!(out.stats.sibling_kept, 1, "{}", report(&out));
    assert_eq!(out.stats.sibling_retired, 0, "{}", report(&out));
    assert!(out.violations.is_empty(), "{}", report(&out));
    let by_incarnation = run(&alias.with(Mutant::WitnessByIncarnationAlone));
    assert_eq!(
        by_incarnation.stats.alias_ghost_retired,
        1,
        "{}",
        report(&by_incarnation)
    );
    assert_eq!(
        by_incarnation.stats.sibling_retired,
        1,
        "{}",
        report(&by_incarnation)
    );
    assert_eq!(
        by_incarnation.stats.sibling_alias_collided,
        1,
        "{}",
        report(&by_incarnation)
    );
    assert!(
        by_incarnation
            .violations
            .iter()
            .any(|v| v.contains("the sibling's")),
        "NEGATIVE CONTROL: an aliased incarnation alone retires the live writer: {}",
        report(&by_incarnation)
    );
}

/// THE BROKER ARM, ON ITS OWN: over random histories of publishes and
/// escalated records at every tier, the cuts a restart may keep are exactly
/// the ones astream's law admits — every kept prefix from the durable head up
/// on a weak tier's power loss, the whole log otherwise — every cut drawn
/// passes the law check, and each of the arm's own defects is caught by the
/// property it breaks.
#[test]
fn the_broker_arm_cuts_exactly_what_the_law_allows() {
    let mut rng = Rng::new(0xa57e);
    let oracle = Escalation::aterm();
    let mut weak_losses = 0;
    for round in 0..300 {
        let tier = [Tier::Strict, Tier::Ordered, Tier::Relaxed][round % 3];
        let stop = [Stop::Clean, Stop::Crash, Stop::PowerLoss][(round / 3) % 3];
        let broker = bus::Broker::new(tier, Escalation::aterm());
        broker.name_producer(7, "n-sim");
        broker.name_producer(8, "h-alice");
        let n = 1 + rng.below(12);
        for i in 0..n {
            let escalated = rng.chance(1, 4);
            let (producer, subject) = if escalated {
                (8, "/f/sim/fleet/h-alice/halt")
            } else {
                (7, "/f/sim/in/n-sim/s-a/n-sim/note")
            };
            broker.publish_as(producer, i + 1, subject, b"v=1");
            if tier == Tier::Ordered && rng.chance(1, 5) {
                broker.horizon();
            }
        }
        let (allowed, durable, head) = {
            let s = broker.state.lock().expect("state");
            (s.allowed_cuts(stop), s.durable, s.log.len() as u64)
        };
        // The law's own answer, from first principles.
        let admitted: Vec<u64> = match (stop, tier) {
            (Stop::PowerLoss, Tier::Ordered | Tier::Relaxed) => (durable..=head).collect(),
            _ => vec![head],
        };
        assert_eq!(allowed, admitted, "round {round}: {tier:?} {stop:?}");
        let r = broker.stop(stop, &mut rng);
        assert!(
            bus::law_check(&r, &oracle).is_empty(),
            "round {round}: {:?}",
            bus::law_check(&r, &oracle)
        );
        if r.kept < head {
            weak_losses += 1;
        }
        broker.start();
    }
    assert!(
        weak_losses > 10,
        "the arm lost acked suffixes {weak_losses} times"
    );
    // Each defect of the arm is caught.
    for (mutant, tier, law) in [
        (ArmMutant::CutBelowDurable, Tier::Ordered, "P3"),
        (ArmMutant::LoseWithoutMove, Tier::Relaxed, "P4"),
        (ArmMutant::LineageAtHead, Tier::Relaxed, "P7"),
    ] {
        let mut caught = false;
        for _ in 0..200 {
            let broker = bus::Broker::new(tier, Escalation::aterm());
            broker.name_producer(7, "n-sim");
            broker.state.lock().expect("state").mutant = mutant;
            for i in 0..1 + rng.below(8) {
                broker.publish_as(7, i + 1, "/f/sim/in/n-sim/s-a/n-sim/note", b"v=1");
            }
            if tier == Tier::Ordered && rng.chance(1, 2) {
                broker.horizon();
                broker.publish_as(7, 99, "/f/sim/in/n-sim/s-a/n-sim/note", b"v=1");
            }
            let r = broker.stop(Stop::PowerLoss, &mut rng);
            if bus::law_check(&r, &oracle).iter().any(|v| v.contains(law)) {
                caught = true;
                break;
            }
        }
        assert!(caught, "{mutant:?} is never caught breaking {law}");
    }
    // An emptied table loses an escalated record the oracle protects.
    let mut caught = false;
    for _ in 0..200 {
        let broker = bus::Broker::new(Tier::Relaxed, Escalation::none());
        broker.name_producer(8, "h-alice");
        for i in 0..1 + rng.below(6) {
            broker.publish_as(8, i + 1, "/f/sim/fleet/h-alice/halt", b"v=1");
        }
        let r = broker.stop(Stop::PowerLoss, &mut rng);
        if bus::law_check(&r, &oracle)
            .iter()
            .any(|v| v.contains("P3e"))
        {
            caught = true;
            break;
        }
    }
    assert!(
        caught,
        "an emptied escalation table is never caught losing a halt"
    );
}

/// THE DEVICE, ON ITS OWN: a durable replace survives every power cut; an
/// atomic-only replace survives as its old version or a newer whole one; a
/// directory flush keeps an unlink; the lossless control keeps everything;
/// and a torn device can leave an unflushed replace cut short — to nothing,
/// or part-way — and only a torn device does.
#[test]
fn the_device_keeps_a_durable_write_and_may_roll_back_the_rest() {
    use crate::fx::Write;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-dev")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).expect("a subdirectory");
    // `gone` lives in a directory of its own: flushing that directory keeps
    // its unlink, and says nothing of `seq`'s unflushed replace.
    let (node, seq, gone) = (root.join("node"), root.join("seq"), sub.join("gone"));
    let mut seen_old = false;
    let mut seen_new = false;
    let (mut seen_empty, mut seen_part) = (false, false);
    let mut rng = Rng::new(7);
    for round in 0..64 {
        let mut device = Device::new(root);
        device.tears = round % 2 == 1;
        let replace = |device: &Device, path: &std::path::Path, bytes: &[u8], durable: bool| {
            std::fs::write(path, bytes).expect("write");
            device.journal(Write::Replace {
                path,
                bytes,
                flush: durable.then_some(aterm_durable::Flush::Durable),
                mode: 0o666,
            });
        };
        replace(&device, &node, b"n-1\n", true);
        replace(&device, &seq, b"1\n", true);
        replace(&device, &seq, b"2\n", false);
        replace(&device, &gone, b"x\n", true);
        std::fs::remove_file(&gone).expect("unlink");
        device.journal(Write::Remove { path: &gone });
        device.journal(Write::SyncDir {
            path: &sub,
            flush: aterm_durable::Flush::Durable,
        });
        device.power_cut(&mut rng);
        assert_eq!(std::fs::read(&node).expect("node"), b"n-1\n", "durable");
        assert!(!gone.exists(), "a flushed unlink stays done");
        let s = std::fs::read(&seq).expect("seq");
        match s.as_slice() {
            b"1\n" => seen_old = true,
            b"2\n" => seen_new = true,
            b"" | b"2" => {
                assert!(device.tears, "only a torn device cuts a file short");
                seen_empty |= s.is_empty();
                seen_part |= !s.is_empty();
            }
            other => panic!("seq holds {other:?}"),
        }
    }
    assert!(seen_old && seen_new, "both versions were chosen");
    assert!(
        seen_empty && seen_part,
        "a torn replace was cut to nothing and part-way"
    );
    let mut lossless = Device::new(root);
    lossless.lossless = true;
    std::fs::write(&seq, b"3\n").expect("write");
    lossless.journal(Write::Replace {
        path: &seq,
        bytes: b"3\n",
        flush: None,
        mode: 0o666,
    });
    lossless.power_cut(&mut rng);
    assert_eq!(std::fs::read(&seq).expect("seq"), b"3\n");
}

/// A DURABLE REPLACE FLUSHES ITS DIRECTORY FOR EVERY ENTRY IN IT, as the state
/// dir's `replace_bytes` does (`sync_dir(parent)`): `clear_halt_acked`'s unlink
/// followed by `set_halts`' durable write is kept by every power cut, and so is
/// a rename made before it — but not an append's data after its file's newest
/// entry change, which a directory flush does not write, nor anything in
/// another directory. Before the device modelled it, a cut offered the
/// unlinked file back: more loss than the contract allows.
#[test]
fn a_durable_replace_flushes_its_directory_for_its_siblings() {
    use crate::fx::Write;
    let dir = aterm_tempfile::Builder::new()
        .prefix("sim-dir")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let sub = root.join("inflight");
    std::fs::create_dir_all(&sub).expect("a subdirectory");
    let (acked_watermark, halts, seq, acked, entry) = (
        root.join("halt-acked"),
        root.join("halts"),
        root.join("seq"),
        root.join("acked"),
        sub.join("7"),
    );
    let mut rng = Rng::new(11);
    let mut appended_lost = false;
    let mut entry_lost = false;
    for _ in 0..64 {
        let device = Device::new(root);
        let replace = |path: &std::path::Path, bytes: &[u8], durable: bool| {
            std::fs::write(path, bytes).expect("write");
            device.journal(Write::Replace {
                path,
                bytes,
                flush: durable.then_some(aterm_durable::Flush::Durable),
                mode: 0o666,
            });
        };
        let append = |path: &std::path::Path, bytes: &[u8]| {
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| f.write_all(bytes))
                .expect("append");
            device.journal(Write::Append { path, bytes });
        };
        replace(&acked_watermark, b"9\n", false);
        replace(&seq, b"1\n", false);
        replace(&entry, b"seq=7\n", false);
        append(&acked, b"3,");
        std::fs::remove_file(&acked_watermark).expect("unlink");
        device.journal(Write::Remove {
            path: &acked_watermark,
        });
        replace(&halts, b"sim h-alice on kept\n", true);
        append(&acked, b"4,");
        device.power_cut(&mut rng);
        assert!(
            !acked_watermark.exists(),
            "an unlink a sibling's durable write flushed came back"
        );
        assert_eq!(
            std::fs::read(&seq).expect("seq"),
            b"1\n",
            "a flushed rename"
        );
        let a = std::fs::read(&acked).expect("the append that created it stays");
        assert!(a == b"3," || a == b"3,4,", "acked holds {a:?}");
        appended_lost |= a == b"3,";
        entry_lost |= !entry.exists();
        for p in [&halts, &seq, &acked, &entry] {
            let _ = std::fs::remove_file(p);
        }
    }
    assert!(appended_lost, "an append after the flush was never lost");
    assert!(entry_lost, "a write in another directory was never lost");
}

/// AN ORDERED WRITE REACHES THE MEDIA BEFORE EVERY LATER ONE — the device's
/// flush points, on their own (`aterm_durable::ordered`'s contract: "A site
/// may use this for 'A persists before B' on one device only after flushing
/// every descriptor that phase A depends on, including directory
/// publications"). A file `a` written with no flush, then the clean stop's
/// shape: `a` flushed where it is by the barrier, its directory's barrier,
/// the record `r` replaced, its bytes flushed by the barrier before its
/// rename — and the cut may land after any of them.
/// Wherever the record's new version is on the media, so is `a`'s, whole.
/// And the cut is real: both of `r`'s versions are drawn, and `a` torn or
/// old. NEGATIVE CONTROL: without the directory's barrier, `a`'s rename is
/// ordered by nothing, and a cut leaves the new record over an old or torn
/// `a` — the order the barrier exists for. A full flush at the end puts all
/// of it on the media.
#[test]
fn an_ordered_write_reaches_the_media_before_every_later_one() {
    use crate::fx::Write;
    use aterm_durable::Flush;
    let dir = aterm_tempfile::Builder::new()
        .prefix("sim-ord")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let sub = root.join("keys");
    std::fs::create_dir_all(&sub).expect("a subdirectory");
    let (a, r) = (sub.join("a"), root.join("r"));
    for barrier_on_dir in [true, false] {
        let mut rng = Rng::new(17);
        let (mut new_record, mut old_record, mut a_damaged) = (false, false, false);
        let mut broken = false;
        for round in 0..400 {
            let mut device = Device::new(root);
            device.tears = true;
            device.seed(&a, b"a0\n");
            device.seed(&r, b"open\n");
            let replace = |path: &std::path::Path, bytes: &[u8], flush| {
                std::fs::write(path, bytes).expect("write");
                device.journal(Write::Replace {
                    path,
                    bytes,
                    flush,
                    mode: 0o666,
                });
            };
            replace(&a, b"a1\n", None);
            // The stop, cut short after `steps` of its writes.
            let steps = round % 5;
            if steps >= 1 {
                device.journal(Write::Flush {
                    path: &a,
                    flush: Flush::Ordered,
                });
            }
            if steps >= 2 && barrier_on_dir {
                device.journal(Write::SyncDir {
                    path: &sub,
                    flush: Flush::Ordered,
                });
            }
            if steps >= 3 {
                replace(&r, b"clean\n", Some(Flush::Ordered));
            }
            if steps >= 4 {
                device.journal(Write::SyncDir {
                    path: root,
                    flush: Flush::Durable,
                });
            }
            device.power_cut(&mut rng);
            let record = std::fs::read(&r).expect("r");
            let held = std::fs::read(&a).expect("a");
            if steps >= 4 {
                assert_eq!(record, b"clean\n", "a full flush keeps the record");
                if barrier_on_dir {
                    assert_eq!(held, b"a1\n", "and what was flushed before it");
                }
            }
            match record.as_slice() {
                b"clean\n" => {
                    new_record = true;
                    if held != b"a1\n" {
                        assert!(
                            !barrier_on_dir,
                            "the new record over {held:?}: an ordered write was overtaken"
                        );
                        broken = true;
                    }
                }
                b"open\n" => old_record = true,
                other => panic!("an ordered record cut short: {other:?}"),
            }
            a_damaged |= held != b"a1\n";
        }
        assert!(
            new_record && old_record,
            "both of the record's versions drawn"
        );
        assert!(a_damaged, "the cut left `a` old or torn somewhere");
        assert_eq!(
            broken, !barrier_on_dir,
            "barrier on the directory: {barrier_on_dir}; the order broken"
        );
    }
}

/// **A CLEAN STOP KEEPS THE RUN'S FILES EXACTLY, WHEREVER A KILL AND A POWER
/// CUT LAND** (sim item S6: the clean stop ordered by the barrier, ended by
/// one full flush). A run of boot 1 writes, with no flush, a file of every
/// kind a power cut may tear — an in-flight entry, a reservation written
/// twice, a receipt's, a key's, a refill floor — clears a second in-flight
/// entry (an unlink nothing flushed), and writes the mirror's pin twice (the
/// first write durable, the second not, as a bridge of this build writes
/// it). Its clean stop ([`crate::state::StateDir::end_run_clean`]) is killed
/// at its `k`-th write, or not at all, and the machine then loses power on a
/// tearing device. In boot 2, wherever `run` says the run stopped cleanly,
/// every one of those files holds exactly what the run left: none torn, none
/// rolled back to an older version or to nothing, and the cleared entry not
/// back — so damage after a clean stop stays a bug's, refused where it is
/// read and never a power cut's, and a clean stop keeps every unlink (what
/// finding F2's republish after one rests on). The law is exercised: clean
/// records and open ones are both drawn, and an open one over files the cut
/// changed (the next boot's sweep is that one's). NEGATIVE CONTROLS
/// ([`crate::state::StopRules`]): the record written first, the directories
/// left unflushed, and the files left unflushed — each leaves a clean record
/// over files the cut changed, at some seed.
#[test]
fn a_clean_stop_keeps_the_runs_files_exactly_under_its_record() {
    use crate::state::{StateDir, StopRules};
    let off = |rule: &str| {
        let mut rules = StopRules::ALL;
        match rule {
            "record_last" => rules.record_last = false,
            "flush_dirs" => rules.flush_dirs = false,
            "flush_files" => rules.flush_files = false,
            _ => {}
        }
        rules
    };
    for rule in ["", "record_last", "flush_dirs", "flush_files"] {
        let (mut clean, mut open, mut open_changed, mut broken) = (0u64, 0u64, 0u64, 0u64);
        for seed in 1..=24u64 {
            for kill_at in 0..=30u64 {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("sim-stop")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let root = dir.path();
                let mut device = Device::new(root);
                device.tears = true;
                let core = super::Core::new(
                    seed,
                    bus::Broker::new(Tier::Strict, Escalation::aterm()),
                    device,
                );
                let process = super::Process::new(std::sync::Arc::clone(&core), 1);
                let st = StateDir::open_on(root, crate::fx::Disk::Sim(process))
                    .expect("the state dir on the device");
                let boot = |n| crate::state::IncSeam {
                    boot: Some(Some(crate::durability::test_boot(n))),
                    off_volume: false,
                    kill_at: None,
                    mutant: 0,
                    span: None,
                };
                st.set_inc_seam(boot(1));
                st.begin_run().expect("boot 1's run");
                write_every_torn_kind(&st);
                let left = torn_kinds_as_they_stand(root);
                if kill_at > 0 {
                    core.arm_kill(1, kill_at);
                }
                let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    st.end_run_clean_with(off(rule))
                }));
                if let Err(payload) = stopped {
                    assert!(payload.is::<super::SimKill>(), "a real panic in the stop");
                }
                core.disarm_kill();
                core.device.power_cut(&mut Rng::new(seed * 64 + kill_at));
                let kept = torn_kinds_as_they_stand(root) == left;
                let next = StateDir::open(root).expect("boot 2's look");
                next.set_inc_seam(boot(2));
                match next.last_run() {
                    Ok(Some(run)) if run.clean => {
                        clean += 1;
                        broken += u64::from(!kept);
                    }
                    _ => {
                        open += 1;
                        open_changed += u64::from(!kept);
                    }
                }
            }
        }
        eprintln!(
            "clean stop, {rule:?} off: {clean} clean, {broken} of them over files the cut \
             changed; {open} open, {open_changed} of them over files the cut changed"
        );
        if rule.is_empty() {
            assert_eq!(broken, 0, "a clean record over files the cut changed");
            assert!(
                clean > 0 && open > 0 && open_changed > 0,
                "the law was exercised"
            );
        } else {
            assert!(
                broken > 0,
                "CONTROL {rule} off: never a clean record over a change"
            );
        }
    }
}

/// **A DURABLE WRITE WHOSE FLUSH FAILED AFTER ITS RENAME IS NOT TRUSTED, AND
/// IS WRITTEN AGAIN** (sim item S7). Its new version is what every reader of
/// the dir sees, and the media may still hold the one before it; a write
/// skipped because the file already reads as wanted would leave it so. On the
/// simulated device, with the directory flush of exactly one replace failing:
///
/// * `run`: the start's record reads as this boot's run and is not taken for
///   one (main's per-open admission, `StateDir::begin_run`'s run fence), and
///   the next `begin_run` writes it again — where after a record that
///   finished it writes nothing (the control: the rewrite is the failure's);
/// * `logfloor`: a floor raised with its flush failed covers no batch
///   (`StateDir::logfloor_admitted`), and a raise to below it writes it again,
///   as it stands; after that, a raise to below it writes nothing. A fresh
///   open admits no floor it did not write — a killed predecessor's failed
///   flush looks the same — and its first raise writes it again. And the
///   media holds it: a power cut keeps the floor every batch was sent under.
///   NEGATIVE CONTROL: the floor trusted as it reads
///   (`StateDir::raise_logfloor_trusting_what_reads`) writes nothing after
///   the failed raise, and the power cut may take it back.
/// * `pins`: a first sight whose flush failed reads, and the route that reads
///   it writes the table again (`StateDir::settle_pins`), once; a power cut
///   then keeps the pin. NEGATIVE CONTROL: routed on as it reads (nothing
///   written), some cut takes the first sight back.
#[test]
fn a_durable_write_whose_flush_failed_is_not_trusted_and_is_written_again() {
    use crate::state::{StateDir, LOGFLOOR};
    let boot = |n| crate::state::IncSeam {
        boot: Some(Some(crate::durability::test_boot(n))),
        off_volume: false,
        kill_at: None,
        mutant: 0,
        span: None,
    };
    let open = |root: &std::path::Path, seed: u64| {
        let core = super::Core::new(
            seed,
            bus::Broker::new(Tier::Strict, Escalation::aterm()),
            Device::new(root),
        );
        let process = super::Process::new(std::sync::Arc::clone(&core), 1);
        let st = StateDir::open_on(root, crate::fx::Disk::Sim(process))
            .expect("the state dir on the device");
        st.set_inc_seam(boot(1));
        (core, st)
    };
    // THE RUN RECORD.
    let dir = aterm_tempfile::Builder::new()
        .prefix("sim-s7-run")
        .tempdir_in("/tmp")
        .expect("scratch");
    let (core, st) = open(dir.path(), 1);
    core.device.fail_after_its_rename(&dir.path().join("run"));
    assert!(st.begin_run().is_err(), "the record's flush failed");
    let this_boots = Some(crate::state::LastRun {
        boot: Some(crate::durability::test_boot(1)),
        clean: false,
    });
    assert_eq!(
        st.last_run().unwrap(),
        this_boots,
        "it reads as this boot's"
    );
    assert_eq!(st.begin_run().unwrap().full, 2, "written again, durably");
    assert_eq!(
        st.begin_run().unwrap().full,
        0,
        "CONTROL: a record that finished is not written again"
    );
    // A refused required directory barrier, or any other failure after the
    // rename, cannot admit a visible floor, even after an earlier success.
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Unsupported,
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("sim-s7-floor-errors")
            .tempdir_in("/tmp")
            .expect("scratch");
        let (_core, st) = open(dir.path(), 4);
        for top in [100, 200] {
            st.refuse_directory_flush(Some((dir.path(), aterm_durable::Flush::Durable, kind)));
            assert_eq!(st.raise_logfloor(top).unwrap_err().kind(), kind);
            assert_eq!(
                st.logfloor().unwrap(),
                Some(top),
                "the renamed bytes are visible"
            );
            assert!(!st.logfloor_admitted(), "a refusal covers no batch");
            st.refuse_directory_flush(None);
            let before = st.ops().fsyncs;
            assert_eq!(st.raise_logfloor(top - 1).unwrap(), top);
            assert_eq!(
                st.ops().fsyncs,
                before + 2,
                "retry finishes the required fence"
            );
            assert!(st.logfloor_admitted());
        }
        let mut killed = boot(1);
        killed.kill_at = Some(crate::state::IncStep::Renamed);
        st.set_inc_seam(killed);
        assert!(st.raise_logfloor(300).is_err(), "post-rename kill point");
        assert_eq!(st.logfloor().unwrap(), Some(300));
        assert!(
            !st.logfloor_admitted(),
            "the kill point also revokes admission"
        );
        st.set_inc_seam(boot(1));
        assert_eq!(st.raise_logfloor(250).unwrap(), 300);
        assert!(st.logfloor_admitted(), "retry admits the visible floor");
    }
    // THE MIRROR'S FLOOR: a raise whose flush fails, the raise after it, and
    // then a power cut — what the floor reads after it.
    let floor_after = |trusting: bool, cut: u64| {
        let dir = aterm_tempfile::Builder::new()
            .prefix("sim-s7-floor")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let (core, st) = open(root, 2);
        let raise = |top| {
            if trusting {
                st.raise_logfloor_trusting_what_reads(top)
            } else {
                st.raise_logfloor(top)
            }
        };
        core.device.fail_after_its_rename(&root.join(LOGFLOOR));
        assert!(raise(100).is_err(), "the floor's flush failed");
        assert_eq!(st.logfloor().unwrap(), Some(100), "it reads as raised");
        assert!(!st.logfloor_admitted(), "and covers no batch");
        let before = st.ops().fsyncs;
        assert_eq!(raise(50).unwrap(), 100, "never lowered");
        let wrote = st.ops().fsyncs - before;
        if trusting {
            assert_eq!(wrote, 0, "CONTROL: the floor trusted as it reads");
        } else {
            assert_eq!(wrote, 2, "written again, as it stands, durably");
            assert!(st.logfloor_admitted(), "and now it covers a batch");
            let before = st.ops().fsyncs;
            assert_eq!(st.raise_logfloor(60).unwrap(), 100);
            assert_eq!(st.ops().fsyncs, before, "a floor this open made durable");
            // A FRESH OPEN admits none it did not write.
            let (_fresh_core, fresh) = open(root, 3);
            assert!(!fresh.logfloor_admitted(), "a fresh open admits none");
            let before = fresh.ops().fsyncs;
            assert_eq!(fresh.raise_logfloor(60).unwrap(), 100);
            assert_eq!(fresh.ops().fsyncs, before + 2, "its first raise writes it");
        }
        let _ = core.device.power_cut(&mut Rng::new(cut));
        st.logfloor().unwrap()
    };
    for cut in 0..32 {
        assert_eq!(
            floor_after(false, cut),
            Some(100),
            "cut {cut}: the floor every batch was sent under stays"
        );
    }
    assert!(
        (0..32).any(|cut| floor_after(true, cut) != Some(100)),
        "NEGATIVE CONTROL: trusted as it reads, some cut takes the floor back"
    );
    // A TOFU PIN: a first sight whose flush fails, the route's settle of the
    // pin it reads (`Bridge::pinned_node_for`), and then a power cut.
    let pin_after = |settle: bool, cut: u64| {
        let dir = aterm_tempfile::Builder::new()
            .prefix("sim-s7-pins")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let (core, st) = open(root, 4);
        core.device.fail_after_its_rename(&root.join("pins"));
        assert!(
            st.pin("s-x", "n-far").is_err(),
            "the first sight's flush failed"
        );
        assert_eq!(
            st.pins().get("s-x").map(String::as_str),
            Some("n-far"),
            "it reads"
        );
        if settle {
            let before = st.ops().fsyncs;
            st.settle_pins().unwrap();
            assert_eq!(st.ops().fsyncs, before + 2, "written again, durably");
            let before = st.ops().fsyncs;
            st.settle_pins().unwrap();
            assert_eq!(st.ops().fsyncs, before, "and once on the media, not again");
        }
        let _ = core.device.power_cut(&mut Rng::new(cut));
        st.pins().get("s-x").cloned()
    };
    for cut in 0..32 {
        assert_eq!(
            pin_after(true, cut).as_deref(),
            Some("n-far"),
            "cut {cut}: the pin a post was routed on stays"
        );
    }
    assert!(
        (0..32).any(|cut| pin_after(false, cut).is_none()),
        "NEGATIVE CONTROL: routed on as it reads, some cut takes the first sight back"
    );
}

/// A file of every kind a power cut may tear, written as a run writes it —
/// with no flush: a reservation twice, a second in-flight entry cleared
/// again, and the mirror's pin twice, its first write durable.
fn write_every_torn_kind(st: &crate::state::StateDir) {
    let pin = |seq| crate::state::PublishPin {
        seq,
        via_key: false,
        identity: None,
        original: None,
        original_off: None,
        owner: None,
    };
    for seq in [7, 8] {
        st.set_in_flight(&crate::state::InFlight {
            pin: pin(seq),
            subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
            body: b"v=1 t=5 text=hello".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
        })
        .expect("an in-flight entry");
    }
    st.clear_in_flight(8).expect("the second entry cleared");
    st.set_post_pin("s-a", 1, &pin(7)).expect("a reservation");
    st.set_post_pin("s-a", 1, &pin(9))
        .expect("the reservation again");
    st.set_receipt_pin("s-a", 4, &pin(7)).expect("a receipt's");
    st.set_key_seq("s-a", "k1", 9).expect("a key's");
    st.set_topic_floors(
        "s-a",
        &std::collections::BTreeMap::from([("t".to_string(), 3)]),
    )
    .expect("a refill floor");
    for total in [100, 200] {
        let mirror = crate::mirror::Pin {
            inc: 1,
            total,
            key: Some([7; 16]),
            win: Vec::new(),
            slots: Vec::new(),
        };
        st.set_logpin(&mirror.render()).expect("the mirror's pin");
    }
}

/// Every file of a kind a power cut may tear, and the mirror's pin, as the
/// directory at `root` holds them now: path inside the dir → bytes.
fn torn_kinds_as_they_stand(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    for place in ["inflight", "sent", "acks", "keys", "unhandled"] {
        let Ok(entries) = std::fs::read_dir(root.join(place)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') {
                let bytes = std::fs::read(entry.path()).expect("read a state file");
                out.insert(format!("{place}/{name}"), bytes);
            }
        }
    }
    if let Ok(bytes) = std::fs::read(root.join(crate::state::LOGPIN)) {
        out.insert(crate::state::LOGPIN.to_string(), bytes);
    }
    out
}

/// THE WORLD THAT LOSES POWER BOTH WAYS — finding F2's: the committed world of
/// `seed`, whose broker's log may lose power ALONE ([`Opts::bus_losses`], a log
/// on another volume than the state dir, a lossy flush) and whose whole
/// machine may lose power too ([`Opts::power_cuts`]), whichever device layout
/// [`Opts::seed`] gives the seed.
fn both_ways(seed: u64) -> Opts {
    let mut o = Opts::seed(seed);
    o.power_cuts = true;
    o.bus_losses = true;
    o
}

/// The committed world of `seed` ON A CLEAN DISK: the device fails no write
/// ([`Opts::write_errors`] off), as in every seed before sim item S7 — the
/// world [`FOUND`]'s seeds were found in, which they replay.
fn on_a_clean_disk(seed: u64) -> Opts {
    Opts {
        write_errors: false,
        ..Opts::seed(seed)
    }
}

/// [`both_ways`] with the device's write errors on in every seed (sim item
/// S7, [`Opts::write_errors`]).
fn both_ways_erring(seed: u64) -> Opts {
    Opts {
        write_errors: true,
        ..both_ways(seed)
    }
}

/// The committed world of `seed` with the device's write errors and aterm's
/// quits on in every seed ([`Opts::write_errors`], [`Opts::quits`]):
/// [`every_seed_with_write_errors_holds_every_law`]'s.
fn erring_quitting(seed: u64) -> Opts {
    Opts {
        write_errors: true,
        quits: true,
        ..Opts::seed(seed)
    }
}

/// [`both_ways_erring`] with aterm's quits on too: the lossiest world the
/// wide search ran.
fn both_ways_erring_quitting(seed: u64) -> Opts {
    Opts {
        quits: true,
        ..both_ways_erring(seed)
    }
}

/// **EVERY SEED WITH WRITE ERRORS HOLDS EVERY LAW** (sim item S7): the
/// committed world of each of [`SEEDS`] seeds with the device's write errors
/// on in every one ([`Opts::write_errors`]) — `EIO` and `ENOSPC` on a replace's
/// write, flush or rename, on an append, an unlink and a flush, a durable
/// replace whose rename was made and whose directory flush failed, an append
/// cut part-way, and the disk full for spells — and aterm quitting and coming
/// back now and then ([`Opts::quits`]: the bridge's clean stop, its flushes
/// and its record, and the start after it, under the same errors) breaks no
/// law, the settle phase's clean disk healing whatever the faults held up;
/// and the seeds together failed every kind of write the device fails, and
/// stopped runs cleanly.
///
/// NO LAW IS WEAKER FOR IT. A post whose reservation or in-flight entry the
/// disk refuses is left queued, never published unjournaled, and goes once
/// the disk takes the write; a record of ours whose `acked` write or entry
/// unlink failed is not delivered until both are made; an attach whose
/// recovery, run record or new log's witness cannot be written does not go
/// on. Each of those waits for the disk, and LIVENESS is judged on the clean
/// disk the settle phase runs on: a bridge held up by a full disk is not a
/// violation, one a later clean disk does not heal is.
#[test]
fn every_seed_with_write_errors_holds_every_law() {
    let outs = run_all(&erring_quitting, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds with write errors broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let sum = |field: fn(&super::world::Stats) -> u64| -> u64 {
        outs.iter().map(|out| field(&out.stats)).sum()
    };
    let counts = [
        ("replaces and appends failed", sum(|s| s.writes_failed)),
        ("unlinks failed", sum(|s| s.unlinks_failed)),
        ("flushes failed", sum(|s| s.flushes_failed)),
        (
            "durable replaces renamed with their flush failed",
            sum(|s| s.renamed_unflushed),
        ),
        ("appends cut part-way", sum(|s| s.appends_cut)),
        ("writes a full disk refused", sum(|s| s.disk_full_refused)),
        ("spells of a full disk", sum(|s| s.disk_full_spells)),
        (
            "links said down to wait for the disk",
            sum(|s| s.held_for_the_disk),
        ),
        ("quits of aterm", sum(|s| s.quits)),
        ("clean stops of a run", sum(|s| s.clean_stops)),
        ("posts", sum(|s| s.posts)),
        ("power cuts", sum(|s| s.power_cuts)),
        ("generation moves", sum(|s| s.moves)),
        ("torn state files set aside", sum(|s| s.set_aside)),
        (
            "in-flight entries of an earlier boot retired",
            sum(|s| s.retired),
        ),
    ];
    eprintln!("the committed world with write errors over {SEEDS} seeds: {counts:?}");
    for &(what, n) in &counts {
        assert!(n > 0, "the seeds exercised no {what}: {counts:?}");
    }
}

/// **THE WORLD THAT LOSES POWER BOTH WAYS BREAKS NO LAW** ([`both_ways`]),
/// over [`SEEDS`] seeds — finding F2's world, held as the committed one is
/// ([`every_seed_holds_every_law`]): the log loses power on its own, moving
/// its generation under a running bridge, and the machine loses power too,
/// rolling the state dir back and ending aterm's sessions — and the seeds
/// together lost acked records, moved the log, cut the machine's power, set
/// torn files aside and retired the in-flight entries an earlier boot left.
#[test]
fn every_seed_of_the_world_that_loses_power_both_ways_holds_every_law() {
    let outs = run_all(&both_ways, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds of the world that loses power both ways broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let sum = |field: fn(&super::world::Stats) -> u64| -> u64 {
        outs.iter().map(|out| field(&out.stats)).sum()
    };
    let counts = [
        ("posts", sum(|s| s.posts)),
        ("kills", sum(|s| s.kills)),
        ("generation moves", sum(|s| s.moves)),
        ("acked records a power loss took", sum(|s| s.lost_records)),
        ("power cuts", sum(|s| s.power_cuts)),
        ("torn state files set aside", sum(|s| s.set_aside)),
        (
            "in-flight entries of an earlier boot retired",
            sum(|s| s.retired),
        ),
    ];
    eprintln!("the world that loses power both ways over {SEEDS} seeds: {counts:?}");
    for &(what, n) in &counts {
        assert!(n > 0, "the seeds exercised no {what}: {counts:?}");
    }
}

/// **FINDING F2, DECIDED: A POST THE LOG LOST STAYS LOST AFTER A LATER POWER
/// CUT** — never published again out of its sender's order.
///
/// In the world that loses power both ways ([`both_ways`]) the log loses, on
/// its own, a post that had landed and was retired (unescalated: the law
/// allows it); later posts of the same sender land; then the machine's power
/// cut rolls the state dir back to before the post's in-flight entry was
/// removed (an unlink nothing flushed, which aterm-durable's contract lets the
/// media lose). Republished at the next attach (`Bridge::settle_in_flight`),
/// on a log whose dedup no longer holds it, it landed again, after the posts
/// that followed it. Nothing the bridge can read tells that entry from one
/// whose record never landed — a read of the log carries no producer
/// sequence, and the same cut rolls back the later post's reservation and
/// entry — and nothing needs it: the sessions it served ended with the boot.
/// So the first bridge of a boot after an unclean stop retires every whole
/// in-flight entry into the quarantine
/// (`crate::state::StateDir::retire_in_flight_of_an_earlier_boot`).
///
/// NEGATIVE CONTROL: the old republish (`Mutant::RepublishInFlightOfAnEarlierBoot`)
/// breaks the order at some seed of that world, and nothing else, the same way
/// on a replay; the same seed with the entries retired breaks no law and
/// retired some. Seed 8 was the first (67 of 1..=2000 broke the order that
/// way; the test before the decision named 122, the witness it checked first)
/// until a later bridge of a state dir retired its predecessors' ghosts
/// (finding F4), which moved every schedule: 36 since.
#[test]
fn a_post_the_log_lost_stays_lost_after_a_later_power_cut() {
    let republishing = |s: u64| both_ways(s).with(Mutant::RepublishInFlightOfAnEarlierBoot);
    let out = breaking_at(&republishing, "order:", 36, CONTROL_SEEDS)
        .expect("NEGATIVE CONTROL: the old republish puts some post out of its sender's order");
    eprintln!(
        "the old republish at seed {}: {}",
        out.seed,
        out.violations.join("; ")
    );
    assert!(
        out.violations.iter().all(|v| v.starts_with("order:")),
        "the old republish breaks the order, and nothing else: {}",
        report(&out)
    );
    let again = run(&republishing(out.seed));
    assert_eq!(out.trace, again.trace, "the old republish replays exactly");
    let decided = run(&both_ways(out.seed));
    assert!(
        decided.violations.is_empty(),
        "with the entries retired, the same seed breaks no law: {}",
        report(&decided)
    );
    assert!(
        decided.stats.retired > 0,
        "the same seed retired the entries a power cut left: {:?}",
        decided.stats
    );
}

/// **TIER-1 OF `FabricExactlyOnce`** (`aterm-models-session`): every inbox offset the
/// committed worlds' messages took, followed through the model after every step of
/// the world ([`super::conform`]) — the real bridge's deliveries and commits, its
/// kills and relaunches, the broker's moves, the endpoint's dedup and its tell — is a
/// run of the model, and together the seeds explain their steps with every action
/// the committed model has.
///
/// NEGATIVE CONTROLS: a world whose endpoint keeps no dedup, and one whose endpoint
/// speaks lane 2 (forgets every offset at any move), each make a round the model
/// rejects at some seed.
#[test]
fn the_round_refines_its_exactly_once_model() {
    let mut opened = 0;
    let mut checked = 0;
    let mut fired = std::collections::BTreeSet::new();
    for out in committed() {
        assert!(
            !out.violations.iter().any(|v| v.starts_with("model:")),
            "{}",
            report(out)
        );
        opened += out.model.0;
        checked += out.model.1;
        fired.extend(out.model.2.iter().copied());
    }
    eprintln!(
        "FabricExactlyOnce: {opened} offsets followed, {checked} steps held to the model, \
         by {fired:?}"
    );
    assert!(
        opened > 500 && checked > 2000,
        "{opened} offsets, {checked} steps"
    );
    let model = aterm_models_session::fabric_exactly_once_model();
    // `CommitEarly` is the defect 2's alone. `DeliverTaken` — a record delivered
    // after a move took it, before the bridge noticed the move — is rare: the
    // lossiest layout reaches it (below). So, since aterm-0's loop, is `Reuse`
    // — a later record at an offset a move took, before anything delivered the
    // first: seed 122 the first of the committed world's since sim item S7
    // (248 before it, 236 until aterm-0's review fixes moved every schedule),
    // none of its first 64 (below).
    for action in model.actions.iter().map(|a| a.name) {
        if matches!(action, "CommitEarly" | "DeliverTaken" | "Reuse") {
            continue;
        }
        assert!(
            fired.contains(action),
            "no step of the world was the model's {action}: {fired:?}"
        );
    }
    // THE LOSSIEST LAYOUT: a Relaxed log that loses power alone under a live
    // bridge (two devices), the mirror off. The committed rules there deliver a
    // record a move took (seeds 140 and 224 of 400 before aterm-0, 102 the
    // first since; none of the committed mix's 400) and break nothing.
    let lossy = |s| {
        let mut o = Opts::seed(s);
        o.power_cuts = false;
        o.bus_losses = true;
        o.tier = Some(Tier::Relaxed);
        o.mirror = false;
        o
    };
    let taken = witnessed(&lossy, 102, RARE_SEEDS, &|out| {
        out.model.2.contains("DeliverTaken")
    })
    .expect("some run delivered a record a move took");
    assert!(taken.violations.is_empty(), "{}", report(&taken));
    // `Reuse`, in the committed world itself, past its first 64 seeds (above).
    let reused = witnessed(&Opts::seed, 122, RARE_SEEDS, &|out| {
        out.model.2.contains("Reuse")
    })
    .expect("some committed run put a later record at an offset a move took");
    assert!(reused.violations.is_empty(), "{}", report(&reused));
    // Rare by nature (seeds 18 and 333 of 400 in this world): a commit owed
    // through an offset at the very move that takes it, and a later message
    // at that offset before anything delivers it.
    let composed = breaking_at(
        // The clamp the composition takes as given: the bridge's owed commit
        // kept across the move.
        &|s| lossy(s).with(Mutant::KeepOwedCommitAcrossMove),
        "model:",
        18,
        RARE_SEEDS,
    )
    .expect("an owed commit kept across a move that took its offset is rejected");
    eprintln!(
        "the owed commit kept across a move: seed {} -> {}",
        composed.seed,
        composed
            .violations
            .iter()
            .find(|v| v.starts_with("model:"))
            .map_or("", String::as_str)
    );
    for (what, ep, witness) in [
        (
            "no dedup",
            EpRules {
                dedup: false,
                ..EpRules::default()
            },
            1,
        ),
        (
            "lane 2",
            EpRules {
                lane: 2,
                ..EpRules::default()
            },
            2,
        ),
    ] {
        let rejected = breaking_at(
            &move |s| {
                let mut o = Opts::seed(s);
                o.ep = ep;
                o
            },
            "model:",
            witness,
            CONTROL_SEEDS,
        );
        let out = rejected.unwrap_or_else(|| panic!("an endpoint with {what} is never rejected"));
        eprintln!(
            "an endpoint with {what}: seed {} -> {}",
            out.seed,
            out.violations
                .iter()
                .find(|v| v.starts_with("model:"))
                .map_or("", String::as_str)
        );
    }
}
