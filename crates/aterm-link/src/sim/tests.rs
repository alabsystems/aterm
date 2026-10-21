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
//! * the FINDINGS the simulation made, each kept as the seed that shows it:
//!   F1 ([`a_torn_state_file_after_a_power_cut_wedges_the_attach`]) and F2
//!   ([`a_post_the_log_lost_comes_back_out_of_order_after_a_later_power_cut`])
//!   stand, outside the committed worlds; F4
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
/// one device, half two, and half of each with the bus mirror on:
/// [`Opts::seed`]). Measured 2026-10-20: seeds 1..=8064 at 400 steps and
/// 1..=400 at 1500 break no law (after the fixes [`FOUND`] keeps); this many
/// run in a few seconds.
const SEEDS: u64 = 64;

/// The most seeds a negative control may take to show its rule's law broken.
const CONTROL_SEEDS: u64 = 64;

/// The most seeds a control of a RARE interleaving may take
/// ([`each_rule_switched_off_is_caught`]'s list).
const RARE_SEEDS: u64 = 400;

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
/// tabs opening and closing, session logs mirrored onto the bus — and the
/// seeds together exercised every one of those.
#[test]
fn every_seed_holds_every_law() {
    let mut total = super::world::Stats::default();
    let mut tiers = std::collections::BTreeSet::new();
    let mut failures = Vec::new();
    for out in run_all(&Opts::seed, 1..=SEEDS) {
        if !out.violations.is_empty() {
            failures.push(report(&out));
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
        total.ghosts_retired += s.ghosts_retired;
        total.ghosts_left += s.ghosts_left;
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
        ("ghost rows retired", total.ghosts_retired),
    ] {
        assert!(n > 0, "the seeds exercised no {what}: {total:?}");
    }
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
const WORKERS: u64 = 8;

/// Run `opts` for every seed in `seeds`, [`WORKERS`] at a time, and answer the
/// outcomes in seed order — the same outcomes however the threads ran.
fn run_all(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    seeds: std::ops::RangeInclusive<u64>,
) -> Vec<Outcome> {
    let seeds: Vec<u64> = seeds.collect();
    let mut out = Vec::with_capacity(seeds.len());
    for batch in seeds.chunks(usize::try_from(WORKERS).unwrap_or(1)) {
        let batch_out: Vec<Outcome> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|&seed| scope.spawn(move || run(&opts(seed))))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                .collect()
        });
        out.extend(batch_out);
    }
    out
}

/// The first seed in `1..=limit` whose run under `opts` breaks `law`
/// ([`breaks`]).
fn first_breaking_within(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    law: &str,
    limit: u64,
) -> Option<Outcome> {
    first_within(opts, limit, |out| {
        out.violations.iter().any(|v| breaks(v, law))
    })
}

/// The first seed in `1..=limit` whose run under `opts` is `hit`'s: run
/// [`WORKERS`] seeds at a time, and the lowest such seed of the first batch
/// that has one — the same seed however the threads ran.
fn first_within(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    limit: u64,
    hit: impl Fn(&Outcome) -> bool,
) -> Option<Outcome> {
    let mut start = 1;
    while start <= limit {
        let end = (start + WORKERS - 1).min(limit);
        let found = run_all(opts, start..=end).into_iter().find(&hit);
        if found.is_some() {
            return found;
        }
        start = end + 1;
    }
    None
}

/// [`first_breaking_within`] the first [`CONTROL_SEEDS`] seeds.
fn first_breaking(opts: impl Fn(u64) -> Opts + Sync, law: &str) -> Option<Outcome> {
    first_breaking_within(&opts, law, CONTROL_SEEDS)
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
    let rules: Vec<(&str, Switch, &str)> = vec![
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
        ),
        (
            "the inbox commit waits for the record's delivery",
            Box::new(|s| Opts::seed(s).with(Mutant::CommitBeforeDelivery)),
            "model:",
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
        ),
        (
            "the attach reads the lineage of a moved generation",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::IgnoreLineage);
                o.tier = Some(Tier::Ordered);
                o
            }),
            "dup delivery",
        ),
        (
            "a post's retry reuses its pinned sequence",
            Box::new(|s| Opts::seed(s).with(Mutant::FreshSeqOnRetry)),
            "dup post",
        ),
        (
            "a receipt's retry reuses its pinned sequence",
            Box::new(|s| Opts::seed(s).with(Mutant::FreshReceiptSeq)),
            "answered 2 times",
        ),
        (
            "one post in flight per sender",
            Box::new(|s| Opts::seed(s).with(Mutant::PipelineSender)),
            "order:",
        ),
        (
            "the incarnation lease is trusted only in the boot that wrote it",
            Box::new(|s| {
                // The lease's boot is judged across a power cut, and the bus's
                // own fence (`max(local, bus) + 1`) holds wherever a record of
                // the incarnation survived: only a world whose log also loses
                // power alone shows the lease alone standing between a reused
                // incarnation and the bus (F2's world, whose own violation is
                // the order, never this alarm).
                let mut o = Opts::seed(s).with(Mutant::LeaseAcrossBoots);
                o.power_cuts = true;
                o.bus_losses = true;
                o
            }),
            "cap-compromised",
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
        ),
        (
            "`session-exited` publishes the session's `exited` row",
            Box::new(|s| Opts::seed(s).with(Mutant::SkipExitedRow)),
            "presence:",
        ),
        (
            "a ghost row is swept",
            Box::new(|s| Opts::seed(s).with(Mutant::NoGhostSweep)),
            "presence:",
        ),
        (
            "the attach records the predecessor's will it read",
            Box::new(|s| Opts::seed(s).with(Mutant::IgnoreWitnessedWill)),
            "presence:",
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
        ),
    ];
    // THE RULES WHOSE LAW A SEED RARELY BREAKS get a longer search, named
    // exactly (never matched by a word of their name). Each stops at the
    // first batch with a hit; the first seeds below are what the test prints
    // (`OFF: … -> seed n`, measured 2026-10-20 with `--nocapture`). The lease
    // is backed up by the bus's own fence in nearly every interleaving (seed
    // 16; its own conformance walk is `state.rs`'s
    // `the_incarnation_lease_refines_its_model`). F3's chain
    // needs a kill on the `link log` line between two bus-only losses (seed
    // 308). The mirror's pin needs a move between the acks that put a batch on
    // the bus and the landing after them, taking what the batch put there
    // (seed 182; the bridge's own model of the batch is
    // `bridge_mirror_tests.rs`'s `LogMirror` Tier-1). The no-loss check and
    // the receipt check see a forgetful endpoint only where a later record
    // lands at an offset a move took AND the endpoint is asked for it (seeds
    // 74 and 120); the model's tap sees the same rule at seed 54.
    const RARE: &[&str] = &[
        "the incarnation lease is trusted only in the boot that wrote it",
        "a session ahead of the bridge's witness keeps below the chain's fork (F3)",
        "a move of the log drops the mirror's pinned batch",
        "the endpoint forgets the offsets a move took (no loss)",
        "the endpoint forgets the offsets a move took (receipts)",
    ];
    for rare in RARE {
        assert!(
            rules.iter().any(|(rule, _, _)| rule == rare),
            "a rare rule names no rule: {rare}"
        );
    }
    let mut missed = Vec::new();
    for (rule, opts, law) in &rules {
        let limit = if RARE.contains(rule) {
            RARE_SEEDS
        } else {
            CONTROL_SEEDS
        };
        match first_breaking_within(opts.as_ref(), law, limit) {
            Some(out) => {
                let committed = run(&with_every_rule(opts(out.seed)));
                eprintln!(
                    "OFF: {rule} -> seed {} breaks `{law}`: {}",
                    out.seed,
                    out.violations
                        .iter()
                        .find(|v| breaks(v, law))
                        .map_or("", String::as_str)
                );
                assert!(
                    !committed.violations.iter().any(|v| breaks(v, law)),
                    "with the rule on, the same seed still breaks `{law}`: {}",
                    report(&committed)
                );
            }
            None => missed.push(format!("{rule} (expected `{law}` within {limit} seeds)")),
        }
    }
    assert!(
        missed.is_empty(),
        "no seed caught these rules switched off:\n  {}",
        missed.join("\n  ")
    );
}

/// THE SEEDS A WIDER SEARCH FOUND BREAKING A LAW (2026-10-20: the committed
/// world over seeds 65..=8064 at 400 steps, and 1..=400 at 1500), each a
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
    let mut failures = Vec::new();
    for batch in FOUND.chunks(usize::try_from(WORKERS).unwrap_or(1)) {
        let outs: Vec<(Outcome, &str)> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|&(seed, steps, what)| {
                    scope.spawn(move || {
                        let mut o = Opts::seed(seed);
                        o.steps = steps;
                        (run(&o), what)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                .collect()
        });
        for (out, what) in outs {
            if !out.violations.is_empty() {
                failures.push(format!("(once: {what}) {}", report(&out)));
            }
        }
    }
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
    let failing = first_breaking(|s| Opts::seed(s).with(Mutant::FreshSeqOnRetry), "dup post")
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
    let a = run(&Opts::seed(SEEDS + 1));
    let b = run(&Opts::seed(SEEDS + 1));
    assert_eq!(a.trace, b.trace, "a passing seed replays exactly too");
    assert!(
        a.trace.len() > 500,
        "a real run: {} trace lines",
        a.trace.len()
    );
}

/// FINDING F1 ([`super::device`]'s module doc): with POSIX's worst case for an
/// unflushed file — a power cut that TEARS it — some seed's bridge never
/// attaches again: a torn in-flight entry or reservation is refused as
/// damaged at every attach of every later boot. The committed model does not
/// tear; this keeps the seed that shows what tearing does, so a fix of the
/// refusal turns this test red and is decided, not drifted into.
#[test]
fn a_torn_state_file_after_a_power_cut_wedges_the_attach() {
    let out = first_breaking(
        |s| {
            let mut o = Opts::seed(s);
            o.torn_device = true;
            o
        },
        "liveness",
    )
    .expect("a torn state file wedges some seed's attach");
    eprintln!("F1 at seed {}: {}", out.seed, out.violations.join("; "));
    let mut untorn = Opts::seed(out.seed);
    untorn.torn_device = false;
    assert!(
        run(&untorn).violations.is_empty(),
        "the same seed without tearing passes"
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
/// (Measured 2026-10-20: 2362 rows left and 125 retired over 400 committed
/// seeds, whose kills are far denser than a desk's.)
#[test]
fn a_ghost_row_outlives_the_bridges_that_could_retire_it() {
    let mut start = 1;
    let out = loop {
        assert!(
            start <= CONTROL_SEEDS,
            "no seed left a ghost row to the operator"
        );
        let end = start + WORKERS - 1;
        if let Some(out) = run_all(&Opts::seed, start..=end)
            .into_iter()
            .find(|o| o.stats.ghosts_left > 0)
        {
            break out;
        }
        start = end + 1;
    };
    eprintln!(
        "F4 at seed {}: {} ghost rows left to the operator, {} retired",
        out.seed, out.stats.ghosts_left, out.stats.ghosts_retired
    );
    assert!(
        out.violations.is_empty(),
        "F4's seed breaks no law the oracle judges: {}",
        report(&out)
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
/// and a torn device can leave an empty file.
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
            b"" => assert!(device.tears, "only a torn device leaves an empty file"),
            other => panic!("seq holds {other:?}"),
        }
    }
    assert!(seen_old && seen_new, "both versions were chosen");
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

/// FINDING F2: in a world where the broker's log loses power ALONE and the
/// whole machine loses power too ([`Opts::bus_losses`] and
/// [`Opts::power_cuts`] at once — a log on another volume than the state
/// dir), a post is resurrected out of its sender's order. The log loses a
/// post that had landed and was retired (unescalated: the law allows it);
/// later posts of the same sender land; then the machine's power cut rolls
/// the state dir back to before the post's in-flight entry was removed (an
/// unlink nothing flushed, which aterm-durable's contract lets the media
/// lose), and the next attach republishes that entry at its sequence
/// (`Bridge::settle_in_flight`) — on a log whose dedup no longer holds it, so
/// it lands again, after the posts that followed it. The committed worlds
/// keep the two kinds of loss apart; this keeps the seed that shows them
/// together, so a fix turns it red and is decided, not drifted into.
#[test]
fn a_post_the_log_lost_comes_back_out_of_order_after_a_later_power_cut() {
    let both = |s: u64| {
        let mut o = Opts::seed(s);
        o.power_cuts = true;
        o.bus_losses = true;
        o
    };
    let out = first_breaking(both, "order:").expect("some seed shows F2");
    eprintln!("F2 at seed {}: {}", out.seed, out.violations.join("; "));
    assert!(
        out.violations.iter().all(|v| v.starts_with("order:")),
        "F2 is the order, and nothing else: {}",
        report(&out)
    );
    let again = run(&both(out.seed));
    assert_eq!(out.trace, again.trace, "the finding replays exactly");
    assert!(
        run(&Opts::seed(out.seed)).violations.is_empty(),
        "the committed world of the same seed breaks no law"
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
    for out in run_all(&Opts::seed, 1..=SEEDS) {
        assert!(
            !out.violations.iter().any(|v| v.starts_with("model:")),
            "{}",
            report(&out)
        );
        opened += out.model.0;
        checked += out.model.1;
        fired.extend(out.model.2);
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
    // lossiest layout reaches it (below).
    for action in model.actions.iter().map(|a| a.name) {
        if matches!(action, "CommitEarly" | "DeliverTaken") {
            continue;
        }
        assert!(
            fired.contains(action),
            "no step of the world was the model's {action}: {fired:?}"
        );
    }
    // THE LOSSIEST LAYOUT: a Relaxed log that loses power alone under a live
    // bridge (two devices), the mirror off. The committed rules there deliver a
    // record a move took (measured 2026-10-20: seeds 140 and 224 of 400; none
    // of the committed mix's 400) and break nothing.
    let lossy = |s| {
        let mut o = Opts::seed(s);
        o.power_cuts = false;
        o.bus_losses = true;
        o.tier = Some(Tier::Relaxed);
        o.mirror = false;
        o
    };
    let taken = first_within(&lossy, RARE_SEEDS, |out| {
        out.model.2.contains("DeliverTaken")
    })
    .expect("some run delivered a record a move took");
    assert!(taken.violations.is_empty(), "{}", report(&taken));
    // Rare by nature (measured 2026-10-20: seeds 18 and 333 of 400 in this
    // world): a commit owed through an offset at the very move that takes it,
    // and a later message at that offset before anything delivers it.
    let composed = first_breaking_within(
        // The clamp the composition takes as given: the bridge's owed commit
        // kept across the move.
        &|s| lossy(s).with(Mutant::KeepOwedCommitAcrossMove),
        "model:",
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
    for (what, ep) in [
        (
            "no dedup",
            EpRules {
                dedup: false,
                ..EpRules::default()
            },
        ),
        (
            "lane 2",
            EpRules {
                lane: 2,
                ..EpRules::default()
            },
        ),
    ] {
        let rejected = first_breaking(
            move |s| {
                let mut o = Opts::seed(s);
                o.ep = ep;
                o
            },
            "model:",
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
