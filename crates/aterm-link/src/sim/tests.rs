// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SIMULATION'S SEEDS, its negative controls and its replay.

use super::bus::{self, ArmMutant, Escalation, Stop, Tier};
use super::device::Device;
use super::endpoint::EpRules;
use super::world::{run, Mutant, Opts, Outcome};
use super::Rng;

const SEEDS: u64 = 64;

const COMMITTED_SEEDS: u64 = 2 * SEEDS;

const CONTROL_SEEDS: u64 = 64;

const SIBLING_SEEDS: u64 = 32;

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
        ("torn state files set aside", total.set_aside),
        ("torn state files told to aterm", total.set_aside_told),
        ("power cuts that tore the mirror's pin", total.pins_torn),
        ("torn mirror pins set aside", total.pins_set_aside),
        (
            "in-flight entries of an earlier boot retired",
            total.retired,
        ),
        ("ghost rows retired", total.ghosts_retired),
        ("acks the broker held", total.acks_held),
        ("reads that found an ack not yet there", total.tries_not_yet),
        ("records read before their ack", total.seen_before_ack),
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
    let waiting = aterm_spec::check::each(&seeds, |&seed| {
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

fn breaks(v: &str, law: &str) -> bool {
    law.split('|').any(|phrase| {
        if v.starts_with("model:") {
            phrase.starts_with("model:")
        } else {
            v.contains(phrase)
        }
    })
}

fn run_all(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    seeds: std::ops::RangeInclusive<u64>,
) -> Vec<Outcome> {
    let seeds: Vec<u64> = seeds.collect();
    aterm_spec::check::each(&seeds, |&seed| run(&opts(seed)))
}

fn committed() -> &'static [Outcome] {
    static RUNS: std::sync::OnceLock<Vec<Outcome>> = std::sync::OnceLock::new();
    RUNS.get_or_init(|| run_all(&Opts::seed, 1..=COMMITTED_SEEDS))
}

fn breaks_law(out: &Outcome, law: &str) -> bool {
    out.violations.iter().any(|v| breaks(v, law))
}

fn first_within(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    limit: u64,
    hit: &(dyn Fn(&Outcome) -> bool + Sync),
) -> Option<Outcome> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let lowest = AtomicU64::new(u64::MAX);
    let seeds: Vec<u64> = (1..=limit).collect();
    aterm_spec::check::each(&seeds, |&seed| {
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

fn breaking_at(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    law: &str,
    witness: u64,
    limit: u64,
) -> Option<Outcome> {
    witnessed(opts, witness, limit, &|out| breaks_law(out, law))
}

fn with_every_rule(mut o: Opts) -> Opts {
    o.mutants.clear();
    o.ep = EpRules::default();
    o.table = Escalation::aterm();
    o.arm = ArmMutant::None;
    o
}

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
        (
            "a later bridge reads its state dir's mark on a row (F4)",
            Box::new(|s| Opts::seed(s).with(Mutant::ReadNoMark)),
            "presence:",
            2,
        ),
        (
            "the attach records the predecessor's will it read",
            Box::new(|s| Opts::with_a_sibling(s).with(Mutant::IgnoreWitnessedWill)),
            "presence:",
            4,
        ),
        (
            "a lower incarnation's row under another state dir's mark is not its own",
            Box::new(|s| Opts::with_a_sibling(s).with(Mutant::RetireEveryEarlierIncarnation)),
            "presence:",
            1,
        ),
        (
            "a bridge tells aterm what its quarantine holds",
            Box::new(|s| Opts::seed(s).with(Mutant::TellMovedNotHeld)),
            "quarantine:",
            1,
        ),
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
        (
            "an attach delivers on a new log only once its witness is written",
            Box::new(|s| both_ways(s).with(Mutant::AttachPastAnUnwrittenWitness)),
            "dup delivery",
            713,
        ),
        (
            "a record the attach published again is delivered once its post is retired",
            Box::new(|s| both_ways_erring(s).with(Mutant::DeliverBeforeTheDrain)),
            "dup delivery",
            18017,
        ),
        (
            "a run whose recovery is owed makes no clean stop",
            Box::new(|s| erring_quitting(s).with(Mutant::CleanStopOverAnOwedRecovery)),
            "mirror",
            1535,
        ),
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
    let shown = aterm_spec::check::each(&rules, |(rule, opts, law, witness)| {
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

const FOUND_BOTH_WAYS_ERRING: &[(u64, usize, &str)] = &[(
    1844,
    400,
    "a record the attach published again delivered while its post was queued",
)];

type FoundIn = (fn(u64) -> Opts, u64, usize, &'static str);

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
    let outs = aterm_spec::check::each(&found, |&(world, seed, steps, what)| {
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
        'sweep: for seed in 1..=24u64 {
            for kill_at in 0..=30u64 {
                if !rule.is_empty() && broken > 0 {
                    break 'sweep;
                }
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

#[test]
fn an_open_ordered_by_the_barrier_keeps_a_clean_record_true() {
    use crate::state::{StartRules, StateDir};
    let rules = |control: &str| match control {
        "atomic" => StartRules {
            flush_record: false,
            flush_root: false,
        },
        "no_root" => StartRules {
            flush_record: true,
            flush_root: false,
        },
        _ => StartRules::ALL,
    };
    let boot = |n| crate::state::IncSeam {
        boot: Some(Some(crate::durability::test_boot(n))),
        off_volume: false,
        kill_at: None,
        mutant: 0,
        span: None,
    };
    for control in ["", "atomic", "no_root"] {
        let (mut clean, mut open, mut open_changed, mut broken, mut ordered) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        for reboot in [false, true] {
            let b = if reboot { 2 } else { 1 };
            for seed in 1..=16u64 {
                for kill_at in 0..=5u64 {
                    let dir = aterm_tempfile::Builder::new()
                        .prefix("sim-start")
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
                    let on = |pid| {
                        let process = super::Process::new(std::sync::Arc::clone(&core), pid);
                        StateDir::open_on(root, crate::fx::Disk::Sim(process))
                            .expect("the state dir on the device")
                    };
                    let first = on(1);
                    first.set_inc_seam(boot(1));
                    first.begin_run().expect("boot 1's run");
                    write_every_torn_kind(&first);
                    first.end_run_clean().expect("its clean stop");
                    drop(first);
                    let left = torn_kinds_as_they_stand(root);
                    if reboot {
                        core.device.power_cut(&mut Rng::new(seed));
                        assert_eq!(
                            torn_kinds_as_they_stand(root),
                            left,
                            "a clean stop keeps its files across a reboot"
                        );
                    }
                    let second = on(2);
                    second.set_inc_seam(boot(b));
                    if kill_at > 0 {
                        core.arm_kill(2, kill_at);
                    }
                    let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        second.begin_run_with(rules(control))
                    }));
                    match started {
                        Err(payload) => {
                            assert!(payload.is::<super::SimKill>(), "a real panic in the start");
                        }
                        Ok(began) => {
                            began.expect("the second start");
                        }
                    }
                    core.disarm_kill();
                    drop(second);
                    let successor = on(3);
                    successor.set_inc_seam(boot(b));
                    let began = successor
                        .begin_run_with(rules(control))
                        .expect("the successor's start");
                    ordered += u64::from((began.ordered, began.full) == (2, 0));
                    rewrite_every_torn_kind(&successor);
                    core.device.power_cut(&mut Rng::new(seed * 64 + kill_at));
                    let kept = torn_kinds_as_they_stand(root) == left;
                    let next = StateDir::open(root).expect("the next boot's look");
                    next.set_inc_seam(boot(b + 1));
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
        }
        eprintln!(
            "start, control {control:?}: {clean} clean, {broken} of them over files the cut \
             changed; {open} open, {open_changed} of them over files the cut changed; {ordered} \
             successors' starts ordered by the barrier"
        );
        if control.is_empty() {
            assert_eq!(broken, 0, "a clean record over files the cut changed");
            assert!(
                clean > 0 && open > 0 && open_changed > 0 && ordered > 0,
                "the law was exercised"
            );
        } else {
            assert!(
                broken > 0,
                "CONTROL {control}: never a clean record over a change"
            );
        }
    }
}

fn rewrite_every_torn_kind(st: &crate::state::StateDir) {
    let pin = |seq| crate::state::PublishPin {
        seq,
        via_key: false,
        identity: None,
        original: None,
        original_off: None,
        owner: None,
    };
    st.set_in_flight(&crate::state::InFlight {
        pin: pin(10),
        subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
        body: b"v=1 t=6 text=again".to_vec(),
        log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
    })
    .expect("a new in-flight entry");
    st.clear_in_flight(7)
        .expect("the clean stop's entry cleared");
    st.set_post_pin("s-a", 1, &pin(11))
        .expect("the reservation rewritten");
    st.set_receipt_pin("s-a", 5, &pin(11))
        .expect("a second receipt's");
    st.set_key_seq("s-a", "k2", 11).expect("a second key's");
    st.set_topic_floors(
        "s-a",
        &std::collections::BTreeMap::from([("t".to_string(), 3), ("u".to_string(), 5)]),
    )
    .expect("a refill floor rewritten");
}

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
    let again = st.begin_run().unwrap();
    assert_eq!(
        (again.ordered, again.full),
        (0, 2),
        "written again in full: visible OPEN does not prove what is on media"
    );
    assert_eq!(
        st.begin_run().unwrap(),
        crate::state::Flushed::default(),
        "CONTROL: a record that finished is not written again"
    );
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

fn both_ways(seed: u64) -> Opts {
    let mut o = Opts::seed(seed);
    o.power_cuts = true;
    o.bus_losses = true;
    o
}

fn on_a_clean_disk(seed: u64) -> Opts {
    Opts {
        write_errors: false,
        ..Opts::seed(seed)
    }
}

fn both_ways_erring(seed: u64) -> Opts {
    Opts {
        write_errors: true,
        ..both_ways(seed)
    }
}

fn erring_quitting(seed: u64) -> Opts {
    Opts {
        write_errors: true,
        quits: true,
        ..Opts::seed(seed)
    }
}

fn both_ways_erring_quitting(seed: u64) -> Opts {
    Opts {
        quits: true,
        ..both_ways_erring(seed)
    }
}

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
    for action in model.actions.iter().map(|a| a.name) {
        if matches!(action, "CommitEarly" | "DeliverTaken" | "Reuse") {
            continue;
        }
        assert!(
            fired.contains(action),
            "no step of the world was the model's {action}: {fired:?}"
        );
    }
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
    let reused = witnessed(&Opts::seed, 122, RARE_SEEDS, &|out| {
        out.model.2.contains("Reuse")
    })
    .expect("some committed run put a later record at an offset a move took");
    assert!(reused.violations.is_empty(), "{}", report(&reused));
    let composed = breaking_at(
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
