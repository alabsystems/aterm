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
//!   ([`a_post_the_log_lost_stays_lost_after_a_later_power_cut`]); F4
//!   ([`a_ghost_row_outlives_the_bridges_that_could_retire_it`]) stands inside
//!   them, counted by the presence oracle rather than judged; F3, an endpoint
//!   that forgot every offset when a killed attach left it a generation ahead
//!   of the next bridge's witness, is fixed in aterm-gui-fabric
//!   (`LogClock::keep_below`) and is a negative control here;
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
/// 67 of 1..=2000, and nothing else. This many run in a few seconds.
const SEEDS: u64 = 64;

/// The most seeds a negative control may take to show its rule's law broken.
const CONTROL_SEEDS: u64 = 64;

/// The most seeds a control of a RARE interleaving may take
/// ([`each_rule_switched_off_is_caught`]'s list). 2000 since aterm-0: the
/// loop that stopped parking on its own commits and acks moved F3's chain —
/// a kill on the `link log` line between two bus-only losses — from seed 308
/// to 1730, the first within 6000.
const RARE_SEEDS: u64 = 2000;

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

/// **THE COMMITTED WORLD BREAKS NO LAW**, over [`SEEDS`] seeds — every tier,
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
        total.retired += s.retired;
        total.ghosts_retired += s.ghosts_retired;
        total.ghosts_left += s.ghosts_left;
        total.acks_held += s.acks_held;
        total.tries_not_yet += s.tries_not_yet;
        total.seen_before_ack += s.seen_before_ack;
    }
    eprintln!("the committed world over {SEEDS} seeds: {total:?}");
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds broke a law:\n{}",
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

/// THE COMMITTED WORLD over seeds `1..=SEEDS`, run once for every test that
/// reads it — its laws ([`every_seed_holds_every_law`]), the model's Tier-1
/// ([`the_round_refines_its_exactly_once_model`]), F4's ghost
/// ([`a_ghost_row_outlives_the_bridges_that_could_retire_it`]) and the replay
/// of a passing seed: a seed is the same run however often it is asked for
/// ([`a_failing_seed_replays_exactly`]), so asking again only costs a run.
fn committed() -> &'static [Outcome] {
    static RUNS: std::sync::OnceLock<Vec<Outcome>> = std::sync::OnceLock::new();
    RUNS.get_or_init(|| run_all(&Opts::seed, 1..=SEEDS))
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
            120,
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
            10,
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
                // after a power cut; 546 since.
                both_ways(s).with(Mutant::LeaseAcrossBoots)
            }),
            "cap-compromised",
            546,
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
            10,
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
            66,
        ),
        (
            "a ghost row is swept",
            Box::new(|s| Opts::seed(s).with(Mutant::NoGhostSweep)),
            "presence:",
            3,
        ),
        (
            "the attach records the predecessor's will it read",
            Box::new(|s| Opts::seed(s).with(Mutant::IgnoreWitnessedWill)),
            "presence:",
            3,
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
    // its posts' answers), seed 66 since.
    const RARE: &[&str] = &[
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

/// **THE SEEDS THAT BROKE A LAW HOLD EVERY LAW** ([`FOUND`]): a receipt
/// journaled while the link was down that wedged every later attach; a
/// refused post or a dropped receipt a power cut brought back and the next
/// attach published; a refused `exited` row nothing wrote again; and the
/// model's tap reading a commit owed on a replaced log as the offset's own.
#[test]
fn the_seeds_that_broke_a_law_hold_every_law() {
    let outs = par_map(FOUND, |&(seed, steps, what)| {
        let mut o = Opts::seed(seed);
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
        FOUND.len(),
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
/// dir's quarantine; a torn mirror pin stops the mirror, as rulings Q6 and Q13
/// have it) and break no law, the liveness law included
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
        "F1's law: {} of {SEEDS} committed seeds set {} torn files aside and broke no law",
        torn.len(),
        torn.iter().map(|out| out.stats.set_aside).sum::<u64>()
    );
    let refusing = |s| Opts::seed(s).with(Mutant::RefuseTornAfterPowerCut);
    let wedged = breaking_at(&refusing, "liveness", 1, CONTROL_SEEDS)
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
    let kept = breaking_at(&by_the_reader, "liveness", 217, RARE_SEEDS).expect(
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

/// FINDING F4 ([`super::world`]'s presence oracle): A GHOST ROW OUTLIVES THE
/// BRIDGES THAT COULD RETIRE IT. A session that left with nobody to say so —
/// its tab closed while no bridge ran, its instance gone in a power cut, or
/// its `exited` row taken by a lossy cut — keeps a `state=live` row. The
/// bridge's sweep retires such a row only from the incarnation that wrote it
/// or the one whose will it witnessed on the node face (`adoptable`, so a live
/// sibling's row under a shared node id is never touched), and only after
/// five minutes of its own sight. When that bridge dies inside the five
/// minutes, every later one is a stranger to the row: it stands `live` until
/// `aterm fabric doctor --retire-ghosts`, and mail to its sid routes nowhere.
/// A fix — a state dir that remembers the chain of incarnations it succeeded,
/// so a later bridge may retire an earlier one's rows — changes who may write
/// another incarnation's row, which is the owner's to decide; this keeps the
/// seed that shows it, so a fix turns it red and is decided, not drifted into.
/// (Over 400 committed seeds, whose kills are far denser than a desk's, rows
/// left outnumber rows retired about twenty to one.)
#[test]
fn a_ghost_row_outlives_the_bridges_that_could_retire_it() {
    let out = committed()
        .iter()
        .find(|o| o.stats.ghosts_left > 0)
        .expect("no seed of the committed world left a ghost row to the operator");
    eprintln!(
        "F4 at seed {}: {} ghost rows left to the operator, {} retired",
        out.seed, out.stats.ghosts_left, out.stats.ghosts_retired
    );
    assert!(
        out.violations.is_empty(),
        "F4's seed breaks no law the oracle judges: {}",
        report(out)
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
                durable,
                mode: 0o666,
            });
        };
        replace(&device, &node, b"n-1\n", true);
        replace(&device, &seq, b"1\n", true);
        replace(&device, &seq, b"2\n", false);
        replace(&device, &gone, b"x\n", true);
        std::fs::remove_file(&gone).expect("unlink");
        device.journal(Write::Remove { path: &gone });
        device.journal(Write::SyncDir { path: &sub });
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
        durable: false,
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
                durable,
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
/// retired some. Seed 8 is the first (67 of 1..=2000 broke the order that
/// way; the test before the decision named 122, the witness it checked first).
#[test]
fn a_post_the_log_lost_stays_lost_after_a_later_power_cut() {
    let republishing = |s: u64| both_ways(s).with(Mutant::RepublishInFlightOfAnEarlierBoot);
    let out = breaking_at(&republishing, "order:", 8, CONTROL_SEEDS)
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
    // first: seed 248 the first of the committed world's (236 until aterm-0's
    // review fixes moved every schedule), none of its first 64 (below).
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
    let reused = witnessed(&Opts::seed, 248, RARE_SEEDS, &|out| {
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
