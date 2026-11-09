// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SIMULATION'S SEEDS, its negative controls and its replay.
//!
//! Normal checks run pinned witnesses without searching. To discover replacements, run
//! `ATERM_SIM_DISCOVER_WITNESSES=1 targo --unverified test --profile opt-test -p aterm-link
//! --lib sim::tests:: -- --nocapture`. Discovery retains each original search bound and
//! fails stale pins even when it finds replacements, so the new witnesses must be recorded.

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

/// The search for a rule rarer still: since the asks journal (C04) the first seed that reaches
/// the incarnation lease's collision is 84750, where 54518 had been the first in 60,000 and,
/// before the post journal (B19), 8483 the first in 20,000. Four and sixteen times the power
/// cuts reach it nowhere in the first 8,000 seeds.
const RAREST_SEEDS: u64 = 100_000;

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
        total.deliveries_pipelined += s.deliveries_pipelined;
        total.retires_pipelined += s.retires_pipelined;
        total.exits += s.exits;
        total.log_records += s.log_records;
        total.mirrored += s.mirrored;
        total.mirror_retained_passes += s.mirror_retained_passes;
        total.mirror_unretained_passes += s.mirror_unretained_passes;
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
        total.commits_corked += s.commits_corked;
        total.commits_alone += s.commits_alone;
        total.gap_fenced += s.gap_fenced;
        total.fences_lifted += s.fences_lifted;
        total.kills_holding += s.kills_holding;
        total.writes_failed += s.writes_failed;
        total.renamed_unflushed += s.renamed_unflushed;
        total.disk_full_refused += s.disk_full_refused;
        total.retention_ticks += s.retention_ticks;
        total.retention_cuts += s.retention_cuts;
        total.given_up += s.given_up;
        total.retained_from += s.retained_from;
        total.gaps_noticed += s.gaps_noticed;
        total.gaps_told += s.gaps_told;
        total.days_passed += s.days_passed;
        total.unretained_starts += s.unretained_starts;
        total.changed_starts += s.changed_starts;
        total.mirror_waits += s.mirror_waits;
        total.refills_judged += s.refills_judged;
        total.refill_rows += s.refill_rows;
        total.refills_together += s.refills_together;
        total.refill_together_sessions += s.refill_together_sessions;
        total.refills_below_oldest += s.refills_below_oldest;
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
        (
            "passes of a mirror over a broker that retains it",
            total.mirror_retained_passes,
        ),
        (
            "passes of a mirror over a broker that serves unretained",
            total.mirror_unretained_passes,
        ),
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
        (
            "inbox commits that rode a publish in one write",
            total.commits_corked,
        ),
        ("inbox commits written alone", total.commits_alone),
        (
            "publishes a gap fence refused behind a refused one",
            total.gap_fenced,
        ),
        ("gap fences the bridge lifted", total.fences_lifted),
        (
            "`deliver` lines sent in one write with another",
            total.deliveries_pipelined,
        ),
        (
            "`outbox sent` lines sent in one write with another",
            total.retires_pipelined,
        ),
        ("kills while an inbox commit was held", total.kills_holding),
        ("state-dir writes failed", total.writes_failed),
        (
            "durable replaces renamed with their flush failed",
            total.renamed_unflushed,
        ),
        ("writes a full disk refused", total.disk_full_refused),
        ("retention ticks", total.retention_ticks),
        ("cuts that moved the base", total.retention_cuts),
        ("records retention gave up", total.given_up),
        ("fetches refused `retained-from=`", total.retained_from),
        ("gap notices a tail was told", total.gaps_noticed),
        ("`link gap` lines aterm was told", total.gaps_told),
        ("days that passed at once", total.days_passed),
        ("opens that set no rules", total.unretained_starts),
        ("opens that set changed rules", total.changed_starts),
        (
            "waits of the mirror on its window budget",
            total.mirror_waits,
        ),
        ("refills from a watermark judged", total.refills_judged),
        ("records those refills had to offer", total.refill_rows),
        (
            "reads of the node's inbox for sessions together",
            total.refills_together,
        ),
        (
            "refills of a lane below the node inbox's oldest",
            total.refills_below_oldest,
        ),
    ] {
        assert!(n > 0, "the seeds exercised no {what}: {total:?}");
    }
}

/// THE COMMITTED SEEDS AGAINST A BROKER THAT DOES NOT GAP-FENCE ([`Opts::gap_fences`] off): the
/// world of a broker an older build opened, where a sender keeps one post in flight
/// (`Bridge::settle_sender`) and the bridge lifts nothing. Every seed of the committed sweep
/// fences, so without this run a regression on that path would pass all of them (C12s1's
/// review).
#[test]
fn every_seed_holds_every_law_against_a_broker_that_does_not_gap_fence() {
    let unfenced = |s: u64| {
        let mut o = Opts::seed(s);
        o.gap_fences = false;
        o
    };
    let outs = run_all(&unfenced, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} seeds break a law against a broker that does not gap-fence:\n{}",
        failures.len(),
        outs.len(),
        failures.join("\n")
    );
    let posts: u64 = outs.iter().map(|out| out.stats.posts).sum();
    let fenced: u64 = outs
        .iter()
        .map(|out| out.stats.gap_fenced + out.stats.fences_lifted)
        .sum();
    assert!(posts > 0, "the seeds sent no post");
    assert_eq!(
        fenced, 0,
        "a broker that does not gap-fence fenced or lifted"
    );
}

/// A world of the committed seeds whose broker's log is on the state dir's device
/// ([`Opts::same_device`], critic item F07).
fn same_device(seed: u64) -> Opts {
    let mut o = Opts::seed(seed);
    o.same_device = true;
    o
}

/// THE MIRROR PIN'S FENCE BY THE BROKER'S FULL FLUSH (critic item F07): with the log on the
/// state dir's device, every committed seed holds every law, the `mirror fence:` law among them,
/// while each open's fence goes by the pin's barriers and the broker's settle, the bridge's own
/// full flush of the pin made only where an open met no settle (a recovery's pin, written before
/// any pass). The control is the committed sweep's own worlds, each log on another device: the
/// bridge flushes every fence itself and sends no settle.
#[test]
fn every_seed_holds_every_law_with_the_log_on_the_state_dirs_device() {
    let outs = run_all(&same_device, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} seeds break a law with the log on the state dir's device:\n{}",
        failures.len(),
        outs.len(),
        failures.join("\n")
    );
    let settles: u64 = outs.iter().map(|out| out.stats.settles).sum();
    let durable: u64 = outs.iter().map(|out| out.stats.pin_durable_replaces).sum();
    let other = run_all(&Opts::seed, 1..=SEEDS);
    let other_settles: u64 = other.iter().map(|out| out.stats.settles).sum();
    let other_durable: u64 = other.iter().map(|out| out.stats.pin_durable_replaces).sum();
    assert!(settles > 0, "no open fenced its pin by the broker's settle");
    assert_eq!(
        other_settles, 0,
        "a broker on another device was asked to settle the state dir's"
    );
    assert!(
        other_durable > 0,
        "on another device the bridge flushed no pin itself"
    );
    assert!(
        durable < other_durable,
        "the settle spared no full flush of the pin: {durable} durable replaces on the state \
         dir's device, {other_durable} on another"
    );
}

/// A world of the committed seeds under a rate limit that binds ([`super::world::binding_rate`],
/// critic item E06).
fn rate_bound(seed: u64) -> Opts {
    let mut o = Opts::seed(seed);
    o.rate_limit = Some(super::world::binding_rate());
    o
}

/// THE COMMITTED SEEDS UNDER A RATE LIMIT THAT BINDS (critic item E06): every law holds, `dup
/// post`, `order:` and the law of the limit's wait (`rate:`, `bus::State::rate_laws`: a bridge
/// that read a refusal for the rate sends nothing more until its wait is over) among them, on a
/// broker that refuses the bridge's publishes for the rate in every seed, gap-fenced too, while
/// aterm's own limit refuses none of the committed sweep's. NEGATIVE CONTROLS: a bridge that
/// takes a refusal for the rate for good loses a post, and one that retries it at once publishes
/// inside the wait.
#[test]
fn every_seed_holds_every_law_under_a_rate_limit_that_binds() {
    let outs = run_all(&rate_bound, 1..=COMMITTED_SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} seeds break a law under a rate limit that binds:\n{}",
        failures.len(),
        outs.len(),
        failures.join("\n")
    );
    let limited = outs.iter().filter(|out| out.stats.rate_limited > 0).count();
    let fenced = outs
        .iter()
        .filter(|out| out.stats.rate_limited > 0 && out.stats.gap_fenced > 0)
        .count();
    eprintln!(
        "rate-limited in {limited} of {} seeds, {fenced} of them gap-fenced too",
        outs.len()
    );
    assert!(
        limited == outs.len() && fenced > 0,
        "the limit bound in {limited} of {} seeds, gap-fenced in {fenced}: not every seed it \
         judges",
        outs.len()
    );
    let refused: u64 = committed().iter().map(|out| out.stats.rate_limited).sum();
    assert_eq!(refused, 0, "aterm's own limit refused the committed sweep");

    for (mutant, law, witness) in [
        (Mutant::RateLimitedIsForGood, "retired unlanded|lost", 2),
        (Mutant::RetryRateLimitedAtOnce, "rate:", 1),
    ] {
        let off = breaking_at(
            &move |s| rate_bound(s).with(mutant),
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
            !breaks_law(&run(&rate_bound(off.seed)), law),
            "with the rule on, the same seed keeps `{law}`"
        );
    }
}

/// A LANDED POST'S BOOKKEEPING, ONE GROUP OVER ITS ACK PASS (critic item E07,
/// `Bridge::land_group`): the self-lane log's offset, then the post's own records, then aterm's
/// `outbox sent`, then the in-flight entry's end and the reservation's, each step's posts at
/// once, and every law holds across a kill between any two of them (the committed sweeps hold
/// them; here, the order's three controls). The hole E06 recorded is closed for a post (and,
/// since F15, for a receipt: [`a_landed_receipts_entry_stands_until_aterm_retired_it`]): under
/// ten records a second and a burst of four
/// ([`super::world::binding_rate`]), seed 78 ended a post's entry at its ack, killed the bridge
/// before its `outbox sent`, and left the next one the record to deliver while the post stood
/// queued; once retention gave the record up, the drain's retry at the pin appended a second
/// copy. NEGATIVE CONTROLS: the entry ended at the ack, as on main, delivers twice there; the
/// entry ended ahead of the self-lane log's offset leaves the next bridge an own record it takes
/// for forged; the reservation ended ahead of aterm's `outbox sent` leaves a post aterm lists and
/// no pin holds, published again at a fresh sequence. The sim's endpoint answers `outbox sent`
/// with `OK` or `ERR no such session` alone, so the paths that keep a post queued past an answer
/// (`ERR busy`) or a hold are the unit test's
/// (`bridge::tests::a_post_aterm_keeps_queued_is_held_until_its_retry_so_a_move_delivers_it_once`).
#[test]
fn a_landed_posts_entry_stands_until_its_post_left_aterms_outbox() {
    let fixed = run(&rate_bound(78));
    assert!(
        fixed.violations.is_empty(),
        "seed 78 under ten records a second and a burst of four: {}",
        report(&fixed)
    );
    type Switch = Box<dyn Fn(u64) -> Opts + Sync>;
    let controls: Vec<(Mutant, Switch, &str, u64, u64)> = vec![
        (
            Mutant::LandedBeforeOutboxSent,
            Box::new(|s| rate_bound(s).with(Mutant::LandedBeforeOutboxSent)),
            "dup delivery",
            168,
            // Since a drain reads the subject budget once, not at each lane it opens (critic
            // item G04), no seed within the committed ones meets it: 168 is the first.
            RARE_SEEDS,
        ),
        (
            Mutant::LandedBeforeAcked,
            Box::new(|s| Opts::seed(s).with(Mutant::LandedBeforeAcked)),
            "forged-self",
            11,
            COMMITTED_SEEDS,
        ),
        (
            Mutant::RetireBeforeOutboxSent,
            Box::new(|s| Opts::seed(s).with(Mutant::RetireBeforeOutboxSent)),
            "dup post",
            2,
            COMMITTED_SEEDS,
        ),
    ];
    let shown = aterm_spec::check::each(&controls, |(_, opts, law, witness, limit)| {
        let off = breaking_at(opts.as_ref(), law, *witness, *limit);
        let on = off
            .as_ref()
            .map(|out| run(&with_every_rule(opts(out.seed))));
        (off, on)
    });
    for ((mutant, _, law, _, limit), (off, on)) in controls.iter().zip(&shown) {
        let (Some(off), Some(on)) = (off, on) else {
            panic!("NEGATIVE CONTROL: with {mutant:?}, no seed within {limit} breaks `{law}`");
        };
        eprintln!(
            "{mutant:?} at seed {}: {}",
            off.seed,
            off.violations
                .iter()
                .find(|v| breaks(v, law))
                .map_or("", String::as_str)
        );
        assert!(
            !breaks_law(on, law),
            "with the rule on, seed {} still breaks `{law}`: {}",
            off.seed,
            report(on)
        );
    }
}

/// A LANDED RECEIPT'S ENTRY STANDS UNTIL ATERM RETIRED IT (critic item F15,
/// `Bridge::receipt_landed`): a receipt is settled by a post's steps (`Bridge::land_group`), so
/// its in-flight entry ends once aterm answered its `deliver <sid> receipt=<rid>`, and every
/// law holds across a kill between its ack and that answer. NEGATIVE CONTROL: the entry ended
/// at the ack, as every build before F15 ended it, answers one decision twice (the `receipt:`
/// law). Under ten records a second and a burst of four ([`super::world::binding_rate`]), seed
/// 478 (515 until a drain read the subject budget once, critic item G04) kills a bridge between
/// its receipt's ack and the retirement; the next bridge's attach hands the record over, its
/// entry ended, a cut gives it up, and the drain's retry at the pin finds no entry to read back
/// from (a receipt's pin holds no head) and lands the answer again. The committed worlds, with
/// no rate limit, reached it in no seed to 20,000 (searched with
/// `ATERM_SIM_DISCOVER_WITNESSES=1`, F15). With the rule on, seed 478 holds every law.
#[test]
fn a_landed_receipts_entry_stands_until_aterm_retired_it() {
    const LAW: &str = "decision on";
    let control = |s| rate_bound(s).with(Mutant::ReceiptRetiredAtAck);
    let off = breaking_at(&control, LAW, 478, RARE_SEEDS).unwrap_or_else(|| {
        panic!(
            "NEGATIVE CONTROL: with ReceiptRetiredAtAck, no seed within {RARE_SEEDS} breaks `{LAW}`"
        )
    });
    eprintln!(
        "ReceiptRetiredAtAck at seed {}: {}",
        off.seed,
        off.violations
            .iter()
            .find(|v| breaks(v, LAW))
            .map_or("", String::as_str)
    );
    let on = run(&with_every_rule(control(off.seed)));
    assert!(
        on.violations.is_empty(),
        "with the rule on, seed {} breaks a law: {}",
        off.seed,
        report(&on)
    );
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
    // A seed whose loop never met an un-acked delivery makes no choice a waiting loop would make
    // otherwise, so the waiting runs take, by the seed's world bits, the first held seed of each
    // kind of world that met one. One seed's count changing moves only the picks it was first for.
    // A law only a waiting loop on a seed left out breaks goes unseen, and no shipped build waits.
    type Kind = (&'static str, fn(&Opts) -> bool);
    let kinds: [Kind; 7] = [
        ("power cuts", |o| o.power_cuts),
        ("bus losses", |o| o.bus_losses),
        ("a mirror", |o| o.mirror),
        ("write errors", |o| o.write_errors),
        ("scaled rules", |o| o.retained && o.scaled),
        ("no retention", |o| !o.retained),
        ("an `auto` mirror", |o| o.mirror && o.auto),
    ];
    let mut seeds: Vec<u64> = kinds
        .iter()
        .map(|&(kind, is)| {
            held.iter()
                .find(|out| out.stats.tries_not_yet > 0 && is(&Opts::seed(out.seed)))
                .unwrap_or_else(|| {
                    panic!("PRECONDITION: no held seed with {kind} met an un-acked delivery")
                })
                .seed
        })
        .collect();
    seeds.sort_unstable();
    seeds.dedup();
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

const WITNESS_DISCOVERY: &str = "ATERM_SIM_DISCOVER_WITNESSES";
const DISCOVER_WITNESSES: &str = "ATERM_SIM_DISCOVER_WITNESSES=1 targo --unverified test --profile opt-test -p aterm-link --lib sim::tests:: -- --nocapture";

#[derive(Clone, Copy, PartialEq, Eq)]
enum WitnessMode {
    Pinned,
    Discover,
}

fn witness_mode() -> WitnessMode {
    if std::env::var_os(WITNESS_DISCOVERY).as_deref() == Some(std::ffi::OsStr::new("1")) {
        WitnessMode::Discover
    } else {
        WitnessMode::Pinned
    }
}

/// Run the pinned world once. Only explicit discovery searches beyond a stale witness;
/// both modes reject a stale pin and explain how to refresh it within the original bound.
fn witness_of(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    witness: u64,
    limit: u64,
    hit: &(dyn Fn(&Outcome) -> bool + Sync),
    mode: WitnessMode,
) -> (Option<Outcome>, Option<String>) {
    assert!(
        (1..=limit).contains(&witness),
        "witness seed {witness} is outside its search 1..={limit}: it would widen the claim"
    );
    let out = run(&opts(witness));
    if hit(&out) {
        return (Some(out), None);
    }
    if mode == WitnessMode::Pinned {
        return (
            None,
            Some(format!(
                "witness seed {witness} no longer shows it; no search ran. Discover within \
                 1..={limit} with `{DISCOVER_WITNESSES}`"
            )),
        );
    }
    eprintln!("STALE WITNESS: seed {witness}; explicitly searching 1..={limit}");
    let found = first_within(opts, limit, hit);
    let stale = found.as_ref().map(|o| {
        format!(
            "witness seed {witness} no longer shows it: pin {}, the first seed within {limit} \
             that does",
            o.seed
        )
    });
    (found, stale)
}

#[test]
fn a_stale_witness_searches_only_in_explicit_discovery_mode() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    for (mode, witness, expected_seed, stale) in [
        (WitnessMode::Pinned, 2, Some(2), false),
        (WitnessMode::Pinned, 1, None, true),
        (WitnessMode::Discover, 1, Some(2), true),
    ] {
        let calls = AtomicUsize::new(0);
        let opts = |seed| {
            assert!((1..=3).contains(&seed), "discovery widened its bound");
            calls.fetch_add(1, Ordering::Relaxed);
            Opts {
                steps: 0,
                ..Opts::seed(seed)
            }
        };
        let (out, note) = witness_of(&opts, witness, 3, &|out| out.seed == 2, mode);
        assert_eq!(out.as_ref().map(|out| out.seed), expected_seed);
        assert_eq!(note.is_some(), stale);
        let calls = calls.load(Ordering::Relaxed);
        if mode == WitnessMode::Pinned {
            assert_eq!(calls, 1, "a normal check ran an unpinned world");
            if stale {
                assert!(note.unwrap().contains(DISCOVER_WITNESSES));
            }
        } else {
            assert!(
                (2..=4).contains(&calls),
                "one pin plus at most three searched worlds"
            );
        }
    }
}

/// [`witness_of`], failing on a stale witness.
fn witnessed(
    opts: &(dyn Fn(u64) -> Opts + Sync),
    witness: u64,
    limit: u64,
    hit: &(dyn Fn(&Outcome) -> bool + Sync),
) -> Option<Outcome> {
    let (out, stale) = witness_of(opts, witness, limit, hit, witness_mode());
    if let Some(stale) = stale {
        panic!("STALE WITNESS: {stale}");
    }
    out
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
            66,
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
            304,
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
            12,
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
            650,
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
            "a sender keeps one post in flight against a broker that does not gap-fence",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::PipelineSender);
                o.gap_fences = false;
                o
            }),
            "order:",
            18,
        ),
        (
            "the bridge asks the broker for the gap fence it pipelines behind",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::NeverAskForTheFence);
                o.gap_fences = true;
                o
            }),
            "order:",
            18,
        ),
        (
            "a post is not sent past an answer that held its sender since its drain reached it",
            Box::new(|s| Opts::seed(s).with(Mutant::SendPastAHeldSender)),
            "order:",
            377,
        ),
        (
            "a pipelining drain skips the posts owed at its peek, not those owed as it goes",
            Box::new(|s| {
                let mut o = Opts::seed(s).with(Mutant::LiveOwedSkip);
                o.refused_batches = false;
                o
            }),
            "dup post",
            39,
        ),
        (
            "the bridge lifts the gap fences its refused writes set",
            Box::new(|s| Opts::seed(s).with(Mutant::KeepGapFences)),
            "liveness",
            29,
        ),
        (
            "a drain sends no listed post that was in flight at its peek",
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
                without_retention(
                    both_ways(s)
                        .with(Mutant::LeaseAcrossBoots)
                        .with(Mutant::TrustTheFloorThatReads),
                )
            }),
            "cap-compromised",
            64594,
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
            306,
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
            24,
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
        // Critic item F17's self-lane log (no append for a post off this node's lanes) moved
        // every seed's state-dir writes, and this pin with six below (108, 7324, 106, 660, 205,
        // 13) off its law; each is the first seed within its bound that shows it again.
        (
            "`session-exited` publishes the session's `exited` row",
            Box::new(|s| Opts::seed(s).with(Mutant::SkipExitedRow)),
            "presence:",
            322,
        ),
        (
            "a session's exit moves the roster's count",
            Box::new(|s| {
                let mut o = Opts::seed(s);
                o.ep = EpRules {
                    exit_moves_roster: false,
                    ..EpRules::default()
                };
                o
            }),
            "routing:",
            23,
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
            32,
        ),
        (
            "a lower incarnation's row under another state dir's mark is not its own",
            Box::new(|s| Opts::with_a_sibling(s).with(Mutant::RetireEveryEarlierIncarnation)),
            "presence:",
            2,
        ),
        (
            "a bridge tells aterm what its quarantine holds",
            Box::new(|s| Opts::seed(s).with(Mutant::TellMovedNotHeld)),
            "quarantine:",
            7,
        ),
        (
            "the recovery a write error stopped is done before the run record",
            Box::new(|s| Opts::seed(s).with(Mutant::BeginRunPastAFailedRecovery)),
            "liveness",
            219,
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
            42,
        ),
        (
            "a mirror takes only once its open made the pin's fence durable (D06)",
            Box::new(|s| Opts::seed(s).with(Mutant::TakeAheadOfTheFence)),
            "mirror fence:",
            2,
        ),
        (
            "a settled fence is held only once the broker answered its settle (F07)",
            Box::new(|s| same_device(s).with(Mutant::FenceBeforeTheSettle)),
            "mirror fence:",
            62,
        ),
        (
            "the pin's settle goes only once its barriers returned (F07)",
            Box::new(|s| same_device(s).with(Mutant::SettleAheadOfTheBarrier)),
            "mirror fence:",
            3,
        ),
        (
            "only a flush of the state dir's own device settles its pin (F07)",
            Box::new(|s| Opts::seed(s).with(Mutant::TrustAnotherDevicesFlush)),
            "mirror fence:",
            2,
        ),
        (
            "a mirror floor covers a batch only once this open made it durable",
            Box::new(|s| Opts::seed(s).with(Mutant::TrustTheFloorThatReads)),
            "mirror total",
            2795,
        ),
        // Explicit discovery on the composed budget scope and per-node hand-over found this
        // first witness within the original 20,000-seed bound.
        (
            "an attach delivers on a new log only once its witness is written",
            Box::new(|s| both_ways(s).with(Mutant::AttachPastAnUnwrittenWitness)),
            "dup delivery",
            1465,
        ),
        // Critic item E07's grouped landing moved this rule's first seed in the world with write
        // errors and quits past 20,000 (to 27907); where power is lost both ways a recovery is
        // owed more often, and its first seed was 9538, which C3b's per-node inbox hand-over
        // (no state-dir write where no node's is held) moved off it to 1307. Composing G04
        // moved that witness to 167, still within the original 20,000-seed bound.
        (
            "a run whose recovery is owed makes no clean stop",
            Box::new(|s| {
                both_ways_erring_quitting(s).with(Mutant::CleanStopOverAnOwedRecovery)
            }),
            "mirror",
            167,
        ),
        (
            "a recovery tried again moves each file once",
            Box::new(|s| both_ways_erring(s).with(Mutant::CopyOnEveryTry)),
            "quarantine copies:",
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
        (
            "a session's refill reads on from a floor retention raised",
            Box::new(|s| Opts::seed(s).with(Mutant::RefillWithoutTheFloor)),
            "liveness",
            6,
        ),
        (
            "an attach's refill of sessions together reads from the lowest start",
            Box::new(|s| Opts::seed(s).with(Mutant::RefillTogetherFromTheHighest)),
            "refill:",
            2,
        ),
        (
            "a session below the node inbox's oldest reads its own lane up to it",
            Box::new(|s| Opts::seed(s).with(Mutant::RefillJoinBelowTheOldest)),
            "refill:",
            5,
        ),
        (
            "the inbox's gap notice moves no commit past the held records in it",
            Box::new(|s| scaled(s).with(Mutant::CommitPastTheGap)),
            "model:",
            10,
        ),
        (
            "the inbox's gap notice is told to aterm",
            Box::new(|s| scaled(s).with(Mutant::TellNoGap)),
            "given up untold",
            190,
        ),
        (
            "the mirror's window takes the broker's oldest=",
            Box::new(|s| windowed(s).with(Mutant::MirrorIgnoresOldest)),
            "mirror",
            1,
        ),
        (
            "a read back that meets its subject's floor reads again told",
            Box::new(|s| Opts::seed(s).with(Mutant::ReadBackWithoutTheHeld)),
            "dup post",
            878,
        ),
        (
            "a read of one offset below its filter's oldest reads again told (F3)",
            Box::new(|s| fetching(s).with(Mutant::ReadAtWithoutTheHeld)),
            "fetch:",
            4,
        ),
        (
            "the deadline sweep reads on from a floor retention raised (F4)",
            Box::new(|s| asking(s).with(Mutant::SweepWithAPlainFetch)),
            "neither answered nor expired",
            20,
        ),
        (
            "a topic replay reads on from a floor retention raised (F4)",
            Box::new(|s| asking(s).with(Mutant::TopicReplayWithAPlainFetch)),
            "topic:",
            5,
        ),
        (
            "a topic replay tells aterm what retention gave up of the topic (F4)",
            Box::new(|s| asking(s).with(Mutant::TopicReplayTellsNoGap)),
            "silent skip",
            14,
        ),
        (
            "a topic catch-up tells the uncounted notice below the first broadcast it delivers (F4)",
            Box::new(|s| asking(s).with(Mutant::TopicReplayDropsAnUncountedNotice)),
            "silent skip",
            583,
        ),
        (
            "the broadcast face's gap notice hands its cursors to the catch-up (F4)",
            Box::new(|s| asking(s).with(Mutant::SayFaceGapUntold)),
            "silent skip",
            14,
        ),
        (
            "the deadline sweep reads the asker's lane for an answer (F4)",
            Box::new(|s| asking(s).with(Mutant::SweepIgnoresAnswers)),
            "which the broker holds",
            1,
        ),
        (
            "a hand-off whose heads the disk refuses keeps the journal (F4)",
            Box::new(|s| asking_erring(s).with(Mutant::HandOffWithoutTheHeads)),
            "dup post|answered 2 times| expired 2 times",
            466,
        ),
        (
            "an answer is delivered only once the deadline it settles is off the disk (F4)",
            Box::new(|s| asking_erring(s).with(Mutant::DeliverOverAnUnwrittenSettle)),
            "was handed",
            614,
        ),
        // This committed witness still reaches the hand-off bug after both scheduling changes.
        (
            "an attach takes a hand-off's files before its incarnation (F4)",
            Box::new(|s| asking_erring(s).with(Mutant::AdoptAfterTheAttach)),
            "dup post|answered 2 times| expired 2 times",
            244,
        ),
        (
            "an adoption names the attach of the hand-off it takes (F4)",
            Box::new(|s| asking_erring(s).with(Mutant::AdoptWithoutTheAttach)),
            "dup post|answered 2 times| expired 2 times",
            2766,
        ),
        (
            "a post retires only once its deadline is on disk (F4)",
            Box::new(|s| asking_erring(s).with(Mutant::RetireOverAnUnwrittenDeadline)),
            "neither answered nor expired",
            19,
        ),
        // The keyed world's own bug (F23): a keyed re-post published past a cut that carried
        // its key's winner unread and dropped its dedup key lands a second copy of the key. The
        // world's first census, before the read back, broke the key law in 311 of 512 seeds, so
        // the bug is a control here, not a found row: its first seed is this row's.
        (
            "a keyed re-post reads its key's winner back before it is published (F23)",
            Box::new(|s| keyed(s).with(Mutant::KeyedRepostWithoutReadingBack)),
            "key:",
            1,
        ),
        // Critic item F21's, in the stray world (F23, [`Opts::strays`]): another writer under
        // the node's producer id at a refused publish's sequence, so a retry at its pin meets
        // a dedup answer it cannot vouch for (case 5).
        (
            "an unverifiable answer holds its own sender (F21)",
            Box::new(|s| strayed(s).with(Mutant::UnverifiableHoldsNothing)),
            "order:",
            6,
        ),
        (
            "an unverifiable answer leaves the link up (F21)",
            Box::new(|s| strayed(s).with(Mutant::UnverifiableDropsTheLink)),
            "order:",
            115,
        ),
        (
            "the attach's replay holds an unverifiable entry's lane (F21)",
            Box::new(|s| strayed(s).with(Mutant::ReplayPastAnUnverifiableEntry)),
            "order:",
            115,
        ),
    ];
    // Critic item F21's other two rules have no sim witness: in 3,000 seeds of the stray world
    // neither `Mutant::HeldRecordWaitsForTheHold` nor `Mutant::HoldPastTheListing` breaks a law
    // (measured 2026-11-05). Each needs the attach's replay to meet an unverifiable entry, a
    // refused publish whose answer a kill lost, and then another entry of its lane: read back
    // on the bus (the first), or whose post left aterm's outbox while retention gave its record
    // up (the second). And the first's harm is a delay no law bounds: the held record's
    // deliveries drop the link only until the hold ends, at the next cut that drops the stray's
    // key. Their controls stay bridge unit tests on a real broker.
    // The replay's drain rule (a record the attach published again is delivered once its
    // post is retired, sim item S7) is held by bridge's
    // `a_replayed_record_waits_for_its_posts_retirement_so_a_move_delivers_it_once`, whose
    // `hold=false` arms are its control: retention's draws (sim item B05) moved the one seed
    // within the rare search that met it (18017), and no seed within 60,000 of these worlds,
    // nor 30,000 of worlds without retention, meets it now.
    const RARE: &[&str] = &[
        "a run whose recovery is owed makes no clean stop",
        "a mirror floor covers a batch only once this open made it durable",
        "an attach delivers on a new log only once its witness is written",
        "`session-exited` publishes the session's `exited` row",
        "the endpoint forgets the offsets a move took (the model's tap)",
        "a session ahead of the bridge's witness keeps below the chain's fork (F3)",
        "a move of the log drops the mirror's pinned batch",
        "the endpoint forgets the offsets a move took (no loss)",
        "the endpoint forgets the offsets a move took (receipts)",
        // One seed in 64 met it (10) while a refill read on at its lane's floor; since the F3
        // review a refill reads below the oldest told on a connection of its own, which moves
        // every scaled world's schedule, and the first seed that meets it is 230.
        "the inbox's gap notice is told to aterm",
        // Since the post journal (B19) a post is four appends to one file, not two
        // files written and two unlinked: fewer writes for a write error to land on.
        "the recovery a write error stopped is done before the run record",
        "an own record is delivered only once its in-flight entry is gone",
        "a read back that meets its subject's floor reads again told",
        // Met where a blocking call inside a drain reads an answer that holds the sender of a
        // post the drain reaches next. Since a drain reads the subject budget once, not at each
        // lane it opens (critic item G04), fewer such calls do: no seed within 64 meets it, and
        // 377 is the first.
        "a post is not sent past an answer that held its sender since its drain reached it",
        // Item F4's, each met at one moment of a kill, a hand-off or a catch-up, a seed in the
        // hundreds or the thousands: a refused write of the deadline table, then a kill before
        // it is written again, the asker still hosted once settled; an uncounted notice below a
        // topic page's first broadcast; the heads of a hand-off the disk refused; an answer
        // delivered over a refused write of the table; and a hand-off whose taker stopped, or
        // handed it on, before its attach came up.
        "a post retires only once its deadline is on disk (F4)",
        "a topic catch-up tells the uncounted notice below the first broadcast it delivers (F4)",
        "a hand-off whose heads the disk refuses keeps the journal (F4)",
        "an answer is delivered only once the deadline it settles is off the disk (F4)",
        "an attach takes a hand-off's files before its incarnation (F4)",
        "an adoption names the attach of the hand-off it takes (F4)",
        // F21's, each first met at seed 115 of the stray world: a link dropped at an
        // unverifiable answer, or a replay past one, sends a sender's later post ahead of it.
        "an unverifiable answer leaves the link up (F21)",
        "the attach's replay holds an unverifiable entry's lane (F21)",
    ];
    const RAREST: &[&str] = &["the incarnation lease is trusted only in the boot that wrote it"];
    for rare in RARE.iter().chain(RAREST) {
        assert!(
            rules.iter().any(|(rule, _, _, _)| rule == rare),
            "a rare rule names no rule: {rare}"
        );
    }
    let mode = witness_mode();
    let shown = aterm_spec::check::each(&rules, |(rule, opts, law, witness)| {
        let limit = if RAREST.contains(rule) {
            RAREST_SEEDS
        } else if RARE.contains(rule) {
            RARE_SEEDS
        } else {
            CONTROL_SEEDS
        };
        let (off, stale) = witness_of(
            opts.as_ref(),
            *witness,
            limit,
            &|out| breaks_law(out, law),
            mode,
        );
        let on = off
            .as_ref()
            .map(|out| run(&with_every_rule(opts(out.seed))));
        (off, on, limit, stale)
    });
    let mut missed = Vec::new();
    let mut stale = Vec::new();
    for ((rule, _, law, _), (off, on, limit, was)) in rules.iter().zip(&shown) {
        if let Some(was) = was {
            stale.push(format!("{rule}: {was}"));
        }
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
            _ => missed.push(format!(
                "{rule} (pin did not show `{law}`; discovery bound {limit})"
            )),
        }
    }
    assert!(
        stale.is_empty() && missed.is_empty(),
        "control witnesses need updating:\n  stale:\n  {}\n  missing:\n  {}",
        stale.join("\n  "),
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

/// A bug [`FOUND_RULES`] can switch back on keeps at most one row per world, here and in the lists
/// below: a seed that holds every law with its rule on and reaches the bug with it off, which
/// [`each_found_bug_is_reached_again_with_its_rule_off`] checks. A change that moves the seed off
/// its bug replaces the row with a seed that reaches it. A bug no [`Mutant`] re-creates (each of
/// [`FOUND`]'s, and 1844's below) keeps every row it found, since nothing says when one of them
/// leaves it.
const FOUND_ERRING: &[(u64, usize, &str)] = &[
    // The E07 review's fix (a landed post's entry stands until the post left aterm's outbox; a
    // read back ends the entry it finds standing) moved 171 and 187 off their bugs (each holds
    // every law still). Each replacement is the first seed to 60,000 that breaks its law with the
    // rule off and holds all with it on ([`found_again`]). Critic item F20's floor, written at
    // the attach, moved 59 to 47, and G04's one subject-budget read a drain 47 to 219.
    (
        219,
        400,
        "a sweep a write error stopped, closed by the run record",
    ),
    (
        15,
        400,
        "a pin judged before the reconcile a write error stopped",
    ),
    // After composing grouped landing with the power-cut repairs, 1435 no longer reaches it.
    // Explicit discovery returned this first witness in the same world and 60,000-seed bound,
    // 443, which critic item F20's floor written at the attach moved to 2795.
    (2795, 400, "a floor whose flush failed trusted as it read"),
    // Sim item B05: a cut dropped the dedup key of a record an in-flight entry named.
    (
        12,
        400,
        "an entry published again at its pin after a cut dropped its key",
    ),
    // The read-back bug (`an entry read back from its head on a log a move continued below it`)
    // has no row since critic item G04: grouped landing moved 31196 off it, the E07 review's fix
    // 12556, and G04's one subject-budget read a drain 26926, and no seed of this world to
    // 60,000 reaches it with its rule off since. A bridge unit test holds the rule with no seed:
    // `bridge::tests::an_entry_a_replay_published_on_a_moved_generation_is_read_back_from_the_fork`.
    //
    // Composing grouped landing with main moved 13 off this bug, and G04 14; the replacement is
    // the first seed to 60,000 that breaks its law with the rule off and holds all with it on.
    // The row before it, "a deduped replay moved the self-lane window down" (seeds 106, 28, 108
    // and 142), went with its rule at critic item F17's review: the window's floor is its own,
    // apart from the offsets it holds (`bridge::self_window`), so a replay's offset below it
    // moves nothing, and no rule is left for a mutant to switch off.
    (
        42,
        400,
        "a record delivered while its in-flight entry stood",
    ),
    // Critic item E07's grouped landing (a landed post's entry stands until aterm answers its
    // `outbox sent`; an ack pass's landings are one write each) moved every seed's journal and lane
    // steps, and every row of this bug off it (each holds every law still); the first seed
    // of a plain search to 60,000 that reaches it with its rule off, and holds every law with it
    // on ([`found_again`]). G04's one subject-budget read a drain moved 25964 to 4633.
    (
        4633,
        400,
        "a record dropped while the disk refused the drop",
    ),
];

const FOUND_BOTH_WAYS: &[(u64, usize, &str)] = &[
    // Grouped landing moved 3273 off it, the E07 review's fix 558, critic item F17's self-lane
    // log 7324, F20's floor written at the attach 2127, and G04's one subject-budget read a
    // drain 4431. With C3b's per-node hand-over, discovery found 1465 first within 60,000.
    (
        1465,
        400,
        "an attach delivered on a log the state dir's witness does not name",
    ),
];

const FOUND_BOTH_WAYS_ERRING: &[(u64, usize, &str)] = &[
    (
        1844,
        400,
        "a record the attach published again delivered while its post was queued",
    ),
    // Critic item E07's grouped landing (a landed post's entry stands until aterm answers its
    // `outbox sent`; an ack pass's landings are one write each) moved every seed's journal and lane
    // steps, and 6751's, 7755's, 735's, 2270's, 3018's and 6371's trajectories off this bug (each
    // holds every law still); the first seed of this world to 60,000 that reaches it with its
    // rule off ([`found_again`]). G04's one subject-budget read a drain moved 210 to 7759;
    // composing C3b moved it to 22763, discovered within the same 60,000-seed bound.
    (
        22763,
        400,
        "a move an `auto` mirror met before its hello left its pinned batch to recover",
    ),
    // The read-back bug (`an entry read back from its head`) has no row here: grouped landing
    // moved 4067 and 36319 off it to 16088, and the E07 review's fix (a read back ends the entry
    // it finds standing) moved 16088 off it, and no seed of this world to 60,000 reaches it with
    // its rule off since. A bridge unit test holds it (`FOUND_ERRING`'s row went at critic item
    // G04).
    // Main replaced the obsolete carried-cut seed 31508 with 1179. Grouped landing moved that
    // row too, the E07 review's fix 462, critic item F17's self-lane log 931, and G04's one
    // subject-budget read a drain 1634. Composed with C3b, the first replacement is 409, found
    // within the original 60,000-seed bound.
    (
        409,
        400,
        "a read back that went on at the floor missed the record a hold rule carried below it",
    ),
];

type FoundIn = (fn(u64) -> Opts, u64, usize, &'static str);

/// Found seeds of the retained worlds that are none of the lists above (aterm-link B05 review).
const FOUND_RETAINED: &[FoundIn] = &[
    // Critic item E04's hold of a take nothing would carry moved 3924's and 3834's trajectories
    // off this bug (each holds every law still); the first windowed seed to 60,000 that reaches
    // it with its rule off.
    (
        windowed,
        7617,
        400,
        "a hello's judge that said the mirror runs kept the wait it no longer showed",
    ),
];

const FOUND_QUITTING: &[FoundIn] = &[
    // The first seed of this world that reached it with its rule off after R6's proof.
    (
        erring_quitting,
        46,
        400,
        "a post retried at its pin after a cut dropped its key",
    ),
    // Grouped landing moved 15410 off it, and C3b's per-node inbox hand-over (no state-dir write
    // where no node's is held) 9538. Composing G04 moved that witness from 1307 to 167,
    // discovered first within the same 60,000-seed bound.
    (
        both_ways_erring_quitting,
        167,
        400,
        "a clean stop over a recovery owed vouched for what nobody reconciled or set aside",
    ),
    // These rare bugs were found scanning the quitting world, which no regular sweep runs.
    // Each row reaches its bug with the rule off and holds every law with it on. After
    // composing G04 and C3b, the first witness still does; explicit discovery found 2375
    // first for the second within the original 60,000-seed bound.
    (
        both_ways_erring_quitting,
        3397,
        400,
        "a clean stop left the heads it kept unflushed",
    ),
    (
        both_ways_erring_quitting,
        2375,
        400,
        "an entry sent ahead of a post of its sender's with none",
    ),
    (
        // Critic item F20's floor written at the attach moved 13679 to 3400, and G04's one
        // subject-budget read a drain 3400 to 55609.
        both_ways_erring_quitting,
        55609,
        400,
        "a retired record's copy ended the memory of its torn file's",
    ),
];

/// Found seeds of the asking world (item F4's review). Not a bridge bug: the topic law's own
/// misreading in the first wide search, which took a follow's `since=` on the broker's log at
/// the follow, not on the log its session was last told, and so judged the bridge's move of a
/// session's cursor to the fork, a move the session had yet to be told, a delivery below its
/// `since` (`Mutant::FollowOnTheBrokersLog` reads it so again). The first seed of 10,000 that
/// meets it on the tree merged with critic item B09.
const FOUND_ASKING: &[FoundIn] = &[(
    asking,
    6340,
    400,
    "a follow judged on a log its session had yet to be told",
)];

/// Every found seed, at the world it broke a law in.
fn found() -> Vec<FoundIn> {
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
        .chain(
            FOUND_BOTH_WAYS_ERRING.iter().map(|&(seed, steps, what)| {
                (both_ways_erring as fn(u64) -> Opts, seed, steps, what)
            }),
        )
        .chain(FOUND_QUITTING.iter().copied())
        .chain(FOUND_RETAINED.iter().copied())
        .chain(FOUND_ASKING.iter().copied())
        .collect()
}

#[test]
fn the_seeds_that_broke_a_law_hold_every_law() {
    let found = found();
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

/// THE RULES whose bugs the found seeds pin, where a [`Mutant`] switches the
/// rule off: `(what the found rows name the bug, the rule off, the law it breaks)`. One is a
/// law's own reading, not a bridge rule ([`FOUND_ASKING`]): its mutant puts the misreading
/// back, and the seed's false violation returns.
const FOUND_RULES: &[(&str, Mutant, &str)] = &[
    (
        "a pin judged before the reconcile a write error stopped",
        Mutant::JudgePinBeforeRecovery,
        "mirror",
    ),
    (
        "a sweep a write error stopped, closed by the run record",
        Mutant::BeginRunPastAFailedRecovery,
        "liveness",
    ),
    (
        "a record delivered while its in-flight entry stood",
        Mutant::DeliverPastAStandingEntry,
        "dup delivery",
    ),
    (
        "an entry published again at its pin after a cut dropped its key",
        Mutant::ReplayWithoutReadingBack,
        "dup post",
    ),
    (
        "a post retried at its pin after a cut dropped its key",
        Mutant::RetryWithoutReadingBack,
        "dup post",
    ),
    (
        "a move an `auto` mirror met before its hello left its pinned batch to recover",
        Mutant::ForgetOnlyAJudgedPin,
        "mirror",
    ),
    (
        "a hello's judge that said the mirror runs kept the wait it no longer showed",
        Mutant::JudgeKeepsAnAnnouncedWait,
        "mirror",
    ),
    (
        "a record dropped while the disk refused the drop",
        Mutant::KeepARefusedDrop,
        "order",
    ),
    (
        "a floor whose flush failed trusted as it read",
        Mutant::TrustTheFloorThatReads,
        "mirror total",
    ),
    (
        "an attach delivered on a log the state dir's witness does not name",
        Mutant::AttachPastAnUnwrittenWitness,
        "dup delivery",
    ),
    (
        "a clean stop over a recovery owed vouched for what nobody reconciled or set aside",
        Mutant::CleanStopOverAnOwedRecovery,
        "mirror",
    ),
    (
        "a read back that went on at the floor missed the record a hold rule carried below it",
        Mutant::ReadBackWithoutTheHeld,
        "dup post",
    ),
    (
        "a clean stop left the heads it kept unflushed",
        Mutant::StopLeavesTheHeadsUnflushed,
        "dup post",
    ),
    (
        "an entry sent ahead of a post of its sender's with none",
        Mutant::EntryAheadOfAPostWithNone,
        "order",
    ),
    (
        "a retired record's copy ended the memory of its torn file's",
        Mutant::OneQuarantineMemoryPerSource,
        "quarantine copies:",
    ),
    (
        "a follow judged on a log its session had yet to be told",
        Mutant::FollowOnTheBrokersLog,
        "it follows it from",
    ),
];

/// **NEGATIVE CONTROL: EACH BUG A FOUND SEED PINS IS REACHED AGAIN WITH ITS
/// RULE OFF** ([`FOUND_RULES`]), by its row at the world and steps
/// [`the_seeds_that_broke_a_law_hold_every_law`] replays it at. A change to a
/// world moves its trajectories, and one that moved a row off its bug would
/// leave that replay holding nothing for the bug.
///
/// One row holds a bug, so a regression only another seed's trajectory would meet goes unseen: no
/// test runs a seed past its fixed ranges and these rows. Explicit discovery searches for a
/// replacement in the failing row's own world; another world's witness cannot replace it.
#[test]
fn each_found_bug_is_reached_again_with_its_rule_off() {
    let found = found();
    let runs: Vec<(usize, FoundIn)> = FOUND_RULES
        .iter()
        .enumerate()
        .flat_map(|(i, &(what, _, _))| {
            let rows: Vec<(usize, FoundIn)> = found
                .iter()
                .filter(|row| row.3 == what)
                .map(|&row| (i, row))
                .collect();
            assert!(!rows.is_empty(), "a found rule names no found seed: {what}");
            rows
        })
        .collect();
    let reached = aterm_spec::check::each(&runs, |&(i, (world, seed, steps, _))| {
        let (_, rule, law) = FOUND_RULES[i];
        let mut o = world(seed).with(rule);
        o.steps = steps;
        (i, seed, breaks_law(&run(&o), law))
    });
    let missed: Vec<String> = reached
        .iter()
        .filter(|&&(_, _, hit)| !hit)
        .map(|&(i, seed, _)| {
            let (what, rule, law) = FOUND_RULES[i];
            let mut line = format!("{what}: {rule:?} breaks no `{law}` at seed {seed}");
            if witness_mode() == WitnessMode::Discover {
                let (_, row) = runs
                    .iter()
                    .find(|(j, row)| *j == i && row.1 == seed)
                    .unwrap();
                line.push_str(&found_again(*row, rule, law));
                eprintln!("{line}");
            }
            line
        })
        .collect();
    assert!(
        missed.is_empty(),
        "found rows that no longer reach their bug with its rule off; replace each with a seed \
         that does:\n  {}",
        missed.join("\n  ")
    );
}

/// How far explicit discovery searches for a found bug's next seed: the bound the searches
/// recorded beside [`FOUND`]'s rows ran to.
const FOUND_SEARCH: u64 = 60_000;

/// EXPLICIT DISCOVERY OF A FOUND BUG'S NEXT SEED ([`each_found_bug_is_reached_again_with_its_rule_off`]):
/// the first seed to [`FOUND_SEARCH`] of the world and steps of the failing row that
/// breaks `law` with `rule` off and holds every law with it on, said for the row to record.
fn found_again((world, last, steps, _): FoundIn, rule: Mutant, law: &str) -> String {
    let at = |seed: u64| Opts {
        steps,
        ..world(seed)
    };
    let next = first_within(&|seed| at(seed).with(rule), FOUND_SEARCH, &|out| {
        breaks_law(out, law) && run(&at(out.seed)).violations.is_empty()
    });
    match next {
        Some(out) => format!(
            "; record seed {} at {steps} steps in the list of seed {last}'s world, the first to \
             {FOUND_SEARCH} that reaches it with its rule off and holds every law with it on",
            out.seed
        ),
        None => format!("; no seed of seed {last}'s world to {FOUND_SEARCH} reaches it"),
    }
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
    let wedged = breaking_at(&refusing, "liveness", 7, CONTROL_SEEDS)
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
    let stopped = breaking_at(&keeping, "stopped for its pin", 15, CONTROL_SEEDS)
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
    // Only a world with a mirror (`seed / 2` odd) and a power cut (`seed` odd) can tear a pin. A pin
    // stop with another cause, in a world that lacks either, goes unseen here.
    let tearable: Vec<u64> = (1..=SEEDS).filter(|s| s % 4 == 3).collect();
    for out in aterm_spec::check::each(&tearable, |&seed| run(&untorn(seed))) {
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
        (Mutant::KeyInThePinAlone, "alias keys", 19),
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

/// ANOTHER LOG (critic item F10): what astream settles for a weak log a power loss left with no
/// record, a fresh id that continues nothing, is a restart the law admits; at a cut that kept a
/// record, or at a stop that loses nothing, it is not.
#[test]
fn the_law_admits_another_log_only_for_a_power_loss_that_kept_no_record() {
    let oracle = Escalation::aterm();
    let restart = |stop: Stop, kept: u64| {
        let broker = bus::Broker::new(Tier::Relaxed, Escalation::aterm());
        broker.name_producer(7, "n-sim");
        for i in 0..3 {
            broker.publish_as(7, i + 1, "/f/sim/pub/n-sim/s-a/ev", b"v=1");
        }
        let mut r = broker.stop_keeping_all(stop);
        r.kept = kept;
        r.generation_after = 0;
        r.lineage_after = Vec::new();
        r.another_log = true;
        r
    };
    let admitted = bus::law_check(&restart(Stop::PowerLoss, 0), &oracle);
    assert!(
        admitted.is_empty(),
        "another log at a cut to 0: {admitted:?}"
    );
    let kept = bus::law_check(&restart(Stop::PowerLoss, 2), &oracle);
    assert!(
        kept.iter().any(|v| v.starts_with("P7: another log")),
        "another log that kept 2 records: {kept:?}"
    );
    for stop in [Stop::Clean, Stop::Crash] {
        let moved = bus::law_check(&restart(stop, 3), &oracle);
        assert!(
            moved.iter().any(|v| v.contains("moved the identity")),
            "another log at a {stop:?}: {moved:?}"
        );
    }
}

#[test]
fn the_device_keeps_a_durable_write_and_may_roll_back_the_rest() {
    use crate::fx::Write;
    let dir = aterm_tempfile::Builder::new()
        .prefix("al-dev")
        .suffix(".noindex")
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

/// A BROKER'S FULL FLUSH ON THE STATE DIR'S DEVICE SETTLES THE PIN ITS BARRIERS ORDERED, AND ONLY
/// THAT (critic item F07, `device::Drain`): the pin replaced by a barrier ahead of its rename and
/// the directory's barrier over the rename, then the broker's full flush, is the media's own pin
/// ([`Device::pin_fenced`]) and every power cut after it keeps it whole on a device that tears.
/// The controls, each over the same writes: a flush issued before the barriers (a settle ahead
/// of them), a pin with no directory barrier over its rename, and a device no broker's flush
/// reaches (the log on another device): none of them fences the pin, and a cut loses it.
#[test]
fn a_brokers_full_flush_settles_the_pin_its_barriers_ordered_on_its_device_only() {
    use crate::fx::Write;
    use aterm_durable::Flush;
    let dir = aterm_tempfile::Builder::new()
        .prefix("sim-drain")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let logpin = root.join(crate::state::LOGPIN);
    let pin = crate::mirror::Pin {
        inc: 1,
        total: 5,
        key: Some([7; 16]),
        ..crate::mirror::Pin::default()
    }
    .render();
    assert!(crate::mirror::Pin::by_this_build(pin.as_bytes()));
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Order {
        Settled,
        FlushAhead,
        NoDirectoryBarrier,
        AnotherDevice,
    }
    let mut rng = Rng::new(23);
    for order in [
        Order::Settled,
        Order::FlushAhead,
        Order::NoDirectoryBarrier,
        Order::AnotherDevice,
    ] {
        let mut lost = false;
        for _ in 0..64 {
            let _ = std::fs::remove_file(&logpin);
            let mut device = Device::new(root);
            device.tears = true;
            let drain = device.drain();
            if order == Order::FlushAhead {
                drain.full_flush();
            }
            std::fs::write(&logpin, &pin).expect("write the pin");
            device.journal(Write::Replace {
                path: &logpin,
                bytes: pin.as_bytes(),
                flush: Some(Flush::Ordered),
                mode: 0o600,
            });
            if order != Order::NoDirectoryBarrier {
                device.journal(Write::SyncDir {
                    path: root,
                    flush: Flush::Ordered,
                });
            }
            assert!(
                !device.pin_fenced(),
                "{order:?}: barriers alone fence nothing"
            );
            if matches!(order, Order::Settled | Order::NoDirectoryBarrier) {
                drain.full_flush();
            }
            assert_eq!(
                device.pin_fenced(),
                order == Order::Settled,
                "{order:?}: the media's own pin"
            );
            device.power_cut(&mut rng);
            let kept = std::fs::read(&logpin).ok();
            if order == Order::Settled {
                assert_eq!(
                    kept.as_deref(),
                    Some(pin.as_bytes()),
                    "a settled pin is kept whole"
                );
            }
            lost |= kept.as_deref() != Some(pin.as_bytes());
        }
        assert_eq!(
            lost,
            order != Order::Settled,
            "{order:?}: a cut lost the pin"
        );
    }
}

#[test]
fn a_durable_replace_flushes_its_directory_for_its_siblings() {
    use crate::fx::Write;
    let dir = aterm_tempfile::Builder::new()
        .prefix("sim-dir")
        .suffix(".noindex")
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

/// A FLUSHED APPEND IS ON THE MEDIA (B19, the post journal): once a flush of an
/// appended file where it is and a flush of its directory are both on the
/// media, every cut keeps the appends the file's flush covered, whole, and a
/// later append may still be lost or torn. CONTROLS: with no flush of the file,
/// some cut loses an append it would have covered; with no flush of its
/// directory, some cut loses the file its first append created.
#[test]
fn a_flushed_append_is_on_the_media_once_its_entry_is() {
    use crate::fx::Write;
    use aterm_durable::Flush;
    let dir = aterm_tempfile::Builder::new()
        .prefix("sim-app")
        .suffix(".noindex")
        .tempdir_in("/tmp")
        .expect("scratch");
    let root = dir.path();
    let sub = root.join("journal");
    std::fs::create_dir_all(&sub).expect("a subdirectory");
    let file = sub.join("posts");
    let whole = b"aa,bb,cc,dd,";
    let mut zeroed = false;
    for (flush_file, flush_dir) in [(true, true), (false, true), (true, false)] {
        let mut rng = Rng::new(23);
        let (mut lost_covered, mut lost_later, mut gone) = (false, false, false);
        for _ in 0..96 {
            let mut device = Device::new(root);
            device.tears = true;
            let _ = std::fs::remove_file(&file);
            let append = |device: &Device, bytes: &[u8]| {
                use std::io::Write as _;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&file)
                    .and_then(|mut f| f.write_all(bytes))
                    .expect("append");
                device.journal(Write::Append { path: &file, bytes });
            };
            append(&device, b"aa,");
            append(&device, b"bb,");
            if flush_file {
                device.journal(Write::Flush {
                    path: &file,
                    flush: Flush::Ordered,
                });
            }
            if flush_dir {
                device.journal(Write::SyncDir {
                    path: &sub,
                    flush: Flush::Ordered,
                });
            }
            append(&device, b"cc,");
            device.journal(Write::SyncDir {
                path: root,
                flush: Flush::Durable,
            });
            append(&device, b"dd,");
            device.power_cut(&mut rng);
            match std::fs::read(&file) {
                Err(_) => gone = true,
                Ok(held) => {
                    let same = held.iter().zip(whole).take_while(|(a, b)| a == b).count();
                    assert!(
                        held.len() <= whole.len() && held[same..].iter().all(|b| *b == 0),
                        "the file holds what was appended, cut or zeroed past a point: {held:?}"
                    );
                    zeroed |= same < held.len();
                    lost_covered |= same < 6;
                    lost_later |= same < whole.len();
                }
            }
        }
        match (flush_file, flush_dir) {
            (true, true) => {
                assert!(!lost_covered && !gone, "a flushed append was lost or torn");
                assert!(lost_later, "an append after the flushes was never lost");
            }
            (false, _) => assert!(lost_covered, "CONTROL: no flush of the file loses nothing"),
            (true, false) => assert!(gone, "CONTROL: no flush of its directory loses nothing"),
        }
    }
    assert!(
        zeroed,
        "a torn append never kept its length with its bytes unwritten"
    );
}

#[test]
fn an_ordered_write_reaches_the_media_before_every_later_one() {
    use crate::fx::Write;
    use aterm_durable::Flush;
    let dir = aterm_tempfile::Builder::new()
        .prefix("sim-ord")
        .suffix(".noindex")
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
                    .suffix(".noindex")
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
            ..StartRules::ALL
        },
        "no_root" => StartRules {
            flush_record: true,
            flush_root: false,
            ..StartRules::ALL
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
                        .suffix(".noindex")
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

/// Once an attach is admitted, its start's OPEN taken and its incarnation after it, a power cut never leaves the clean stop's record standing: the attach may have presented posts to a broker on another volume, and a clean record would vouch for the clean stop's in-flight entries behind them. Boot 1 stops cleanly; a second start in the same boot or the next finishes, is killed at a write, or has its record's rename made and its directory flush fail (in the same boot the durable replace's own flush of the root, in the next boot the root's barrier after the ordered replace, [`Device::fail_after_its_rename`]; the law checks the fault failed the start); a successor starts; a tearing power cut follows. Controls: a visible OPEN of this boot replaced by the barrier (B07 as first merged), and an admission that takes no incarnation (a new boot's first lease is what puts an ordered OPEN on the media).
#[test]
fn once_an_attach_is_admitted_a_power_cut_never_reads_the_clean_stop() {
    use crate::state::{Flushed, StartRules, StateDir};
    let boot = |n| crate::state::IncSeam {
        boot: Some(Some(crate::durability::test_boot(n))),
        off_volume: false,
        kill_at: None,
        mutant: 0,
        span: None,
    };
    for control in ["", "visible_open", "no_incarnation"] {
        let rules = StartRules {
            this_boot_in_full: control != "visible_open",
            ..StartRules::ALL
        };
        let attach = |st: &StateDir| -> Result<Flushed, &'static str> {
            let began = st.begin_run_with(rules).map_err(|_| "its start failed")?;
            if control != "no_incarnation" {
                st.take_incarnation(0)
                    .map_err(|_| "its incarnation failed")?;
            }
            Ok(began)
        };
        let (mut admitted, mut broken, mut ordered) = (0u64, 0u64, 0u64);
        for reboot in [false, true] {
            let b = if reboot { 2 } else { 1 };
            for seed in 1..=16u64 {
                for fault in 0..=5u64 {
                    let dir = aterm_tempfile::Builder::new()
                        .prefix("sim-admit")
                        .suffix(".noindex")
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
                    let on = |pid, n| {
                        let process = super::Process::new(std::sync::Arc::clone(&core), pid);
                        let st = StateDir::open_on(root, crate::fx::Disk::Sim(process))
                            .expect("the state dir on the device");
                        st.set_inc_seam(boot(n));
                        st
                    };
                    let first = on(1, 1);
                    first.begin_run().expect("boot 1's run");
                    first.take_incarnation(0).expect("boot 1's incarnation");
                    write_every_torn_kind(&first);
                    first.end_run_clean().expect("its clean stop");
                    drop(first);
                    if reboot {
                        core.device.power_cut(&mut Rng::new(seed));
                    }
                    let second = on(2, b);
                    match fault {
                        0 => {}
                        5 => core.device.fail_after_its_rename(&root.join("run")),
                        k => core.arm_kill(2, k),
                    }
                    let tried =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attach(&second)));
                    core.disarm_kill();
                    if fault == 5 {
                        assert!(
                            matches!(tried, Ok(Err("its start failed"))),
                            "control {control:?}, reboot={reboot}, seed {seed}: the fault named \
                             fails the second start, whichever tier it takes: {tried:?}"
                        );
                    }
                    let mut any = match tried {
                        Err(payload) => {
                            assert!(payload.is::<super::SimKill>(), "a real panic in the start");
                            false
                        }
                        Ok(began) => began.is_ok(),
                    };
                    drop(second);
                    let successor = on(3, b);
                    if let Ok(began) = attach(&successor) {
                        any = true;
                        ordered += u64::from((began.ordered, began.full) == (2, 0));
                    }
                    drop(successor);
                    core.device.power_cut(&mut Rng::new(seed * 64 + fault));
                    let next = StateDir::open(root).expect("the next boot's look");
                    next.set_inc_seam(boot(b + 1));
                    if any {
                        admitted += 1;
                        broken += u64::from(matches!(next.last_run(), Ok(Some(run)) if run.clean));
                    }
                }
            }
        }
        eprintln!(
            "admission, control {control:?}: {admitted} schedules admitted an attach, {broken} \
             of them read the clean stop after the cut; {ordered} successors admitted by the \
             barrier"
        );
        if control.is_empty() {
            assert_eq!(
                broken, 0,
                "an admitted attach under the clean stop's record"
            );
            assert!(admitted > 0 && ordered > 0, "the law was exercised");
        } else {
            assert!(
                broken > 0,
                "CONTROL {control}: an admitted attach never under the clean stop's record"
            );
        }
    }
}

/// A world's core for the proof laws: a tearing device under the state dir at `root`.
fn proof_world(root: &std::path::Path, seed: u64) -> std::sync::Arc<super::Core> {
    let mut device = Device::new(root);
    device.tears = true;
    super::Core::new(
        seed,
        bus::Broker::new(Tier::Strict, Escalation::aterm()),
        device,
    )
}

/// The state dir on `core`'s device, opened by process `pid` in boot `n`, its proofs made and read
/// by `rule`.
fn proof_open(
    core: &std::sync::Arc<super::Core>,
    root: &std::path::Path,
    pid: u64,
    n: u8,
    rule: crate::state::ProofRule,
) -> crate::state::StateDir {
    let process = super::Process::new(std::sync::Arc::clone(core), pid);
    let st = crate::state::StateDir::open_on(root, crate::fx::Disk::Sim(process))
        .expect("the state dir on the device");
    st.set_inc_seam(crate::state::IncSeam {
        boot: Some(Some(crate::durability::test_boot(n))),
        off_volume: false,
        kill_at: None,
        mutant: 0,
        span: None,
    });
    st.set_proof_rule(rule);
    st
}

/// R6: A START THE PROOF ADMITTED LEAVES NO CLEAN STOP UNDER A POWER CUT. A successor that finds
/// the OPEN it would write, with a `run.durable` proof standing for it, writes nothing, so the law of
/// [`once_an_attach_is_admitted_a_power_cut_never_reads_the_clean_stop`] must hold with the proof
/// standing in for that write. Boot 1 runs and stops cleanly; a second start, in the same boot or
/// the next, finishes, is killed at a write, or has its record's rename made and its directory
/// flush fail; two successors in turn, each a handoff's or a restart's, attach in that boot;
/// a tearing power cut follows. Every schedule in which an attach was admitted reads an open
/// record after the cut. Controls: the proof made as soon as the rename is visible, whatever its
/// flush does ([`crate::state::ProofRule::AtTheRename`]), and a proof read by its presence under
/// the bare stem, whatever the name holds ([`crate::state::ProofRule::ByPresence`]): each leaves an admitted attach
/// under the clean stop's record. The law also counts successors the proof admitted, after a
/// start in full in the same boot and, after a reboot, after a start by the barrier that the
/// boot's first incarnation flushed.
#[test]
fn a_start_its_proof_admitted_never_leaves_the_clean_stop_under_a_power_cut() {
    use crate::state::{Flushed, ProofRule, StateDir};
    for (control, rule) in [
        ("", ProofRule::Ships),
        ("at_the_rename", ProofRule::AtTheRename),
        ("by_presence", ProofRule::ByPresence),
    ] {
        let attach = |st: &StateDir| -> Result<Flushed, &'static str> {
            let began = st.begin_run().map_err(|_| "its start failed")?;
            st.take_incarnation(0)
                .map_err(|_| "its incarnation failed")?;
            Ok(began)
        };
        let (mut admitted, mut broken) = (0u64, 0u64);
        let mut proven = [0u64; 2];
        for reboot in [false, true] {
            let b = if reboot { 2 } else { 1 };
            for seed in 1..=16u64 {
                for fault in 0..=5u64 {
                    let dir = aterm_tempfile::Builder::new()
                        .prefix("sim-proof-run")
                        .suffix(".noindex")
                        .tempdir_in("/tmp")
                        .expect("scratch");
                    let root = dir.path();
                    let core = proof_world(root, seed);
                    let first = proof_open(&core, root, 1, 1, rule);
                    first.begin_run().expect("boot 1's run");
                    first.take_incarnation(0).expect("boot 1's incarnation");
                    write_every_torn_kind(&first);
                    first.end_run_clean().expect("its clean stop");
                    drop(first);
                    if reboot {
                        core.device.power_cut(&mut Rng::new(seed));
                    }
                    let second = proof_open(&core, root, 2, b, rule);
                    match fault {
                        0 => {}
                        5 => core.device.fail_after_its_rename(&root.join("run")),
                        k => core.arm_kill(2, k),
                    }
                    let tried =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| attach(&second)));
                    core.disarm_kill();
                    if fault == 5 {
                        assert!(
                            matches!(tried, Ok(Err("its start failed"))),
                            "control {control:?}, reboot={reboot}, seed {seed}: the fault named \
                             fails the second start: {tried:?}"
                        );
                    }
                    let mut any = match tried {
                        Err(payload) => {
                            assert!(payload.is::<super::SimKill>(), "a real panic in the start");
                            false
                        }
                        Ok(began) => began.is_ok(),
                    };
                    drop(second);
                    for pid in [3, 4] {
                        let successor = proof_open(&core, root, pid, b, rule);
                        if let Ok(began) = attach(&successor) {
                            any = true;
                            proven[usize::from(reboot)] += u64::from(began.proven);
                        }
                    }
                    core.device.power_cut(&mut Rng::new(seed * 64 + fault));
                    let next = StateDir::open(root).expect("the next boot's look");
                    next.set_inc_seam(crate::state::IncSeam {
                        boot: Some(Some(crate::durability::test_boot(b + 1))),
                        ..crate::state::IncSeam::default()
                    });
                    if any {
                        admitted += 1;
                        broken += u64::from(matches!(next.last_run(), Ok(Some(run)) if run.clean));
                    }
                }
            }
        }
        eprintln!(
            "proof of the run, control {control:?}: {admitted} schedules admitted an attach, \
             {broken} of them read the clean stop after the cut; successors admitted by the \
             proof: {} in the same boot, {} after a reboot",
            proven[0], proven[1]
        );
        if control.is_empty() {
            assert_eq!(
                broken, 0,
                "an admitted attach under the clean stop's record"
            );
            assert!(
                admitted > 0 && proven[0] > 0 && proven[1] > 0,
                "the law was exercised, the proof after a start in full and after one the \
                 barrier ordered"
            );
        } else {
            assert!(
                broken > 0,
                "CONTROL {control}: an admitted attach never under the clean stop's record"
            );
        }
    }
}

/// R6: A FLOOR ITS PROOF ADMITTED IS NEVER TAKEN BACK BY A POWER CUT. Every floor a raise answers
/// is one the mirror sends batches under, so a later boot must read one at least as high. A
/// first open raises the floor; a second raises it again and finishes, is killed at a write, or
/// has its rename made and its directory flush fail; two fresh opens of the same boot, each a
/// successor's, raise to a top below it, admitting the floor that reads where a `logfloor.durable`
/// proof stands for it ([`crate::state::StateDir::admit_logfloor_by_its_proof`]) and writing it again
/// where it does not; a tearing power cut follows. Controls as for the run's proof
/// ([`a_start_its_proof_admitted_never_leaves_the_clean_stop_under_a_power_cut`]): each lets a
/// cut take back a floor a batch was sent under.
#[test]
fn a_floor_its_proof_admitted_is_never_taken_back_by_a_power_cut() {
    use crate::state::{ProofRule, StateDir};
    for (control, rule) in [
        ("", ProofRule::Ships),
        ("at_the_rename", ProofRule::AtTheRename),
        ("by_presence", ProofRule::ByPresence),
    ] {
        let (mut runs, mut broken, mut proven) = (0u64, 0u64, 0u64);
        for seed in 1..=16u64 {
            for fault in 0..=5u64 {
                let dir = aterm_tempfile::Builder::new()
                    .prefix("sim-proof-floor")
                    .suffix(".noindex")
                    .tempdir_in("/tmp")
                    .expect("scratch");
                let root = dir.path();
                let core = proof_world(root, seed);
                let first = proof_open(&core, root, 1, 1, rule);
                let mut sent_under = first.raise_logfloor(100).expect("the first floor");
                drop(first);
                let second = proof_open(&core, root, 2, 1, rule);
                match fault {
                    0 => {}
                    5 => core
                        .device
                        .fail_after_its_rename(&root.join(crate::state::LOGFLOOR)),
                    k => core.arm_kill(2, k),
                }
                let tried = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    second.raise_logfloor(200)
                }));
                core.disarm_kill();
                match tried {
                    Err(payload) => {
                        assert!(payload.is::<super::SimKill>(), "a real panic in the raise");
                    }
                    Ok(Ok(floor)) => sent_under = sent_under.max(floor),
                    Ok(Err(_)) => {}
                }
                if fault == 5 {
                    assert!(
                        !second.logfloor_admitted(),
                        "control {control:?}, seed {seed}: the fault named fails the raise"
                    );
                }
                drop(second);
                for pid in [3, 4] {
                    let successor = proof_open(&core, root, pid, 1, rule);
                    let before = successor.ops().fsyncs;
                    if let Ok(floor) = successor.raise_logfloor(150) {
                        sent_under = sent_under.max(floor);
                        proven += u64::from(successor.ops().fsyncs == before);
                    }
                }
                core.device.power_cut(&mut Rng::new(seed * 64 + fault));
                let after = StateDir::open(root)
                    .expect("the next boot's look")
                    .logfloor()
                    .ok()
                    .flatten()
                    .unwrap_or(0);
                runs += 1;
                broken += u64::from(after < sent_under);
            }
        }
        eprintln!(
            "proof of the floor, control {control:?}: {runs} schedules, {broken} of them read a \
             floor below one a batch was sent under after the cut; {proven} raises admitted by \
             the proof"
        );
        if control.is_empty() {
            assert_eq!(broken, 0, "a floor a batch was sent under, taken back");
            assert!(proven > 0, "the law was exercised");
        } else {
            assert!(
                broken > 0,
                "CONTROL {control}: no cut ever takes back a floor a batch was sent under"
            );
        }
    }
}

/// What `tar`, `ditto` and `rsync -aH` each make of the top of a state dir: every regular file of
/// `from` a new inode at its name in `to`, renamed in and never flushed, on `core`'s device, and
/// two names of one inode in `from` two names of one inode in `to`. A hard link's other name is
/// not journaled, as [`crate::state::StateDir`]'s own proof is not (a control's proof is such a
/// link): it stands whatever the cut.
fn copy_keeping_its_links(from: &std::path::Path, to: &std::path::Path, core: &super::Core) {
    use std::os::unix::fs::MetadataExt as _;
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .expect("the dir to copy")
        .flatten()
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut copied = std::collections::BTreeMap::new();
    for entry in entries {
        let meta = entry.metadata().expect("the entry's metadata");
        if !meta.is_file() {
            continue;
        }
        let target = to.join(entry.file_name());
        if let Some(first) = copied.get(&meta.ino()) {
            let _ = std::fs::remove_file(&target);
            std::fs::hard_link(first, &target).expect("the second name of one inode");
            continue;
        }
        let bytes = std::fs::read(entry.path()).expect("the copied bytes");
        let staged = to.join(".copying");
        std::fs::write(&staged, &bytes).expect("the copy's new inode");
        std::fs::rename(&staged, &target).expect("renamed in");
        core.device.journal(crate::fx::Write::Replace {
            path: &target,
            bytes: &bytes,
            flush: None,
            mode: crate::state::STATE_FILE_MODE,
        });
        copied.insert(meta.ino(), target);
    }
}

/// What `cp -R` and, `keeping_times`, `cp -a` each make of the top of a state dir over the names a
/// dir already holds: every regular file of `from` written INTO the file at its name in `to`, its
/// inode kept (a name `to` lacks is a new file), never flushed, on `core`'s device; `cp -a` then
/// puts the source's modification time back on it. A proof's name is not journaled, as
/// [`crate::state::StateDir`]'s own proof is not. `state::tests`'
/// `a_restore_written_into_the_files_a_dir_holds_proves_nothing` runs the real tool.
fn copy_in_place(
    from: &std::path::Path,
    to: &std::path::Path,
    core: &super::Core,
    keeping_times: bool,
) {
    use std::io::Write as _;
    crate::state::past_the_clock_tick(to);
    let mut entries: Vec<_> = std::fs::read_dir(from)
        .expect("the dir to copy")
        .flatten()
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let meta = entry.metadata().expect("the entry's metadata");
        if !meta.is_file() {
            continue;
        }
        let named = entry.file_name();
        let target = to.join(&named);
        let bytes = std::fs::read(entry.path()).expect("the copied bytes");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&target)
            .expect("the file at its name");
        file.write_all(&bytes).expect("the bytes written into it");
        if keeping_times {
            file.set_modified(meta.modified().expect("the source's time"))
                .expect("the source's time put back");
        }
        let proof = [crate::state::RUN_PROOF, crate::state::LOGFLOOR_PROOF]
            .iter()
            .any(|stem| named.as_encoded_bytes().starts_with(stem.as_bytes()));
        if !proof {
            core.device.journal(crate::fx::Write::Replace {
                path: &target,
                bytes: &bytes,
                flush: None,
                mode: crate::state::STATE_FILE_MODE,
            });
        }
    }
}

/// R6r: NO COPY OF THE DIR, NOR ANY RESTORE OVER IT, PROVES ANYTHING. A bridge of boot 1 has its
/// OPEN and its floor proved in one dir, and a copy in the same boot, with no flush, puts them in
/// another or back in that one. `tar`, `ditto` and `rsync -aH` each make a new inode: into a fresh
/// dir (the media holds nothing), over a dir whose media holds a clean stop of its own, or, from a
/// copy taken while it ran, back over the very dir that made them after it stopped cleanly. `cp -R`
/// and `cp -a` write into the inode the name holds: back over the dir of a bridge that died with
/// its OPEN proved, its own bytes and, under `cp -a`, its own modification time; or another dir's
/// over a dir whose floor was proved and whose bridge stopped cleanly. A bridge attaches there and
/// a tearing power cut follows. Its OPEN must read after the cut, and the floor its raise answered.
/// CONTROLS, each a proof that vouches for bytes no flush covered, so that the start writes nothing
/// or the raise flushes nothing and the cut brings back the clean stop, no record, a torn one or a
/// lower floor: [`crate::state::ProofRule::AnyDirectory`], R6 as first merged, in every arm;
/// [`crate::state::ProofRule::ThisDirectory`], a name that says the directory and not the inode,
/// in every arm that lands on the dir that made the proof; and
/// [`crate::state::ProofRule::ThisInode`], a name that says the inode and not its state, in every
/// arm `cp` writes.
#[test]
fn no_copy_of_the_dir_nor_restore_over_it_proves_anything_under_a_power_cut() {
    use crate::state::{ProofRule, StateDir};
    const ARMS: [&str; 6] = [
        "a new inode: a fresh dir",
        "a new inode: over a clean stop",
        "a new inode: restored over its own dir",
        "cp -R: back over a bridge that died",
        "cp -a: back over a bridge that died",
        "cp -R: another dir's over a clean stop",
    ];
    let run = |st: &StateDir, floor: u64| {
        st.begin_run().expect("its start");
        st.take_incarnation(0).expect("its incarnation");
        st.raise_logfloor(floor).expect("its floor");
    };
    let held = |&(starts, raises, lost): &(u64, u64, u64)| (starts, raises, lost) == (0, 0, 0);
    let lied = |&(starts, raises, lost): &(u64, u64, u64)| starts + raises > 0 && lost > 0;
    // Under each rule the arms before `lies_from` hold on all 32 schedules. Each arm from it on
    // lies, and stops at the schedule that shows it, since one is all that claim needs.
    for (control, rule, lies_from, law) in [
        (
            "",
            ProofRule::Ships,
            ARMS.len(),
            "a copy's or a restore's proof vouched",
        ),
        (
            "any_directory",
            ProofRule::AnyDirectory,
            0,
            "CONTROL any_directory: some copy's proof never vouched for what the cut took",
        ),
        (
            "this_directory",
            ProofRule::ThisDirectory,
            2,
            "CONTROL this_directory: a copy elsewhere proves nothing, what lands on the dir that \
             made the proof does",
        ),
        (
            "this_inode",
            ProofRule::ThisInode,
            3,
            "CONTROL this_inode: a new inode proves nothing, bytes written into the old one do",
        ),
    ] {
        let mut counts = [(0u64, 0u64, 0u64); ARMS.len()];
        let mut schedules = [0u64; ARMS.len()];
        for (arm, count) in counts.iter_mut().enumerate() {
            for seed in 1..=32u64 {
                let scratch = |prefix: &str| {
                    aterm_tempfile::Builder::new()
                        .prefix(prefix)
                        .suffix(".noindex")
                        .tempdir_in("/tmp")
                        .expect("scratch")
                };
                let (elsewhere, target) = (scratch("sim-copy-from"), scratch("sim-copy-to"));
                let core = proof_world(target.path(), seed);
                let other_core = proof_world(elsewhere.path(), seed);
                match arm {
                    2..=4 => {
                        let earlier = proof_open(&core, target.path(), 1, 1, rule);
                        run(&earlier, 300);
                        copy_keeping_its_links(target.path(), elsewhere.path(), &other_core);
                        if arm == 2 {
                            earlier.end_run_clean().expect("its clean stop");
                        }
                    }
                    _ => {
                        run(&proof_open(&other_core, elsewhere.path(), 1, 1, rule), 300);
                        if arm != 0 {
                            let earlier = proof_open(&core, target.path(), 1, 1, rule);
                            run(&earlier, 100);
                            earlier.end_run_clean().expect("its clean stop");
                        }
                    }
                }
                match arm {
                    0..=2 => copy_keeping_its_links(elsewhere.path(), target.path(), &core),
                    _ => copy_in_place(elsewhere.path(), target.path(), &core, arm == 4),
                }
                let copy = proof_open(&core, target.path(), 2, 1, rule);
                let began = copy.begin_run().expect("the copy's start");
                copy.take_incarnation(0).expect("the copy's incarnation");
                let before = copy.ops().fsyncs;
                let sent_under = copy.raise_logfloor(150).expect("the copy's floor");
                let raise_flushed = copy.ops().fsyncs != before;
                if control.is_empty() {
                    assert_eq!(
                        (began.proven, began.ordered, began.full, raise_flushed),
                        (false, 0, 2, true),
                        "{}, seed {seed}: a copy's start and raise are written in full",
                        ARMS[arm]
                    );
                }
                core.device.power_cut(&mut Rng::new(seed * 64 + 3));
                let next = StateDir::open(target.path()).expect("the next boot's look");
                let open_reads = matches!(next.last_run(), Ok(Some(run)) if !run.clean);
                let floor = next.logfloor().ok().flatten().unwrap_or(0);
                count.0 += u64::from(began.proven);
                count.1 += u64::from(!raise_flushed);
                count.2 += u64::from(!open_reads || floor < sent_under);
                schedules[arm] = seed;
                if arm >= lies_from && lied(count) {
                    break;
                }
            }
        }
        for ((arm, (starts, raises, lost)), ran) in ARMS.iter().zip(counts).zip(schedules) {
            eprintln!(
                "a copy or a restore, control {control:?}, {arm}: {ran} schedules, the proof \
                 admitted {starts} starts and {raises} floors, {lost} lost the OPEN or the floor \
                 at the cut"
            );
        }
        assert!(
            counts[..lies_from].iter().all(held) && counts[lies_from..].iter().all(lied),
            "{law}: {counts:?}"
        );
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
        at: None,
    };
    st.set_in_flight(&crate::state::InFlight {
        pin: pin(10),
        subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
        body: b"v=1 t=6 text=again".to_vec(),
        log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
        at: None,
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

/// **THE HEADS A CLEAN STOP KEPT STAND THROUGH A POWER CUT** (publication-exactly-once): a stop
/// hands a record in flight to the files a build before the journal reads and keeps its heads
/// beside them, its record then vouching for what it wrote. Over every cut drawn, the next boot's
/// attach takes the record with the head it reads it back from. NEGATIVE CONTROL
/// (`stop_leaves_the_heads_unflushed`, as before): the clean record stands, so the record is not
/// retired, and a cut that took the heads leaves it to be published again at its pin unread, a
/// second copy past a cut that dropped its key (`FOUND_QUITTING`).
#[test]
fn the_heads_a_clean_stop_kept_stand_through_a_power_cut() {
    use crate::state::{InFlight, PublishPin, StateDir};
    let boot = |n| crate::state::IncSeam {
        boot: Some(Some(crate::durability::test_boot(n))),
        off_volume: false,
        kill_at: None,
        mutant: 0,
        span: None,
    };
    let head_after = |unflushed: bool, cut: u64| -> Option<u64> {
        let dir = aterm_tempfile::Builder::new()
            .prefix("sim-handed-heads")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = dir.path();
        let mut device = Device::new(root);
        device.tears = true;
        let core = super::Core::new(
            cut,
            bus::Broker::new(Tier::Strict, Escalation::aterm()),
            device,
        );
        let open = |n: u8| {
            let process = super::Process::new(std::sync::Arc::clone(&core), u64::from(n));
            let st = StateDir::open_on(root, crate::fx::Disk::Sim(process))
                .expect("the state dir on the device");
            st.set_inc_seam(boot(n));
            st
        };
        let st = open(1);
        st.stop_leaves_the_heads_unflushed(unflushed);
        st.begin_run().expect("an open run");
        st.take_incarnation(0).expect("an attach");
        let entry = InFlight {
            pin: PublishPin {
                seq: 7,
                via_key: false,
                identity: None,
                original: None,
                original_off: None,
                owner: None,
                at: Some(40),
            },
            subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
            body: b"v=1 t=5 text=hello".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
            at: Some(50),
        };
        st.set_post_pin("s-a", 1, &entry.pin).expect("reserved");
        st.set_in_flight(&entry).expect("in flight");
        assert_eq!(st.hand_posts_to_files().expect("handed"), 2);
        st.end_run_clean().expect("a clean stop");
        drop(st);
        let _ = core.device.power_cut(&mut Rng::new(cut));
        let next = open(2);
        assert!(
            next.last_run().expect("reads").is_some_and(|run| run.clean),
            "the clean stop's record is on the media"
        );
        next.begin_run().expect("the next boot's run");
        next.take_incarnation(0).expect("its attach");
        let entries = next.in_flight().expect("reads");
        assert_eq!(
            entries.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
            [7],
            "cut {cut}: a clean stop's record in flight is not retired"
        );
        entries[0].1.at
    };
    for cut in 0..32 {
        assert_eq!(
            head_after(false, cut),
            Some(50),
            "cut {cut}: the record is read back from its head"
        );
    }
    assert!(
        (0..32).any(|cut| head_after(true, cut).is_none()),
        "NEGATIVE CONTROL: unflushed, some cut takes the heads and keeps the file"
    );
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
        .suffix(".noindex")
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
            .suffix(".noindex")
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
            .suffix(".noindex")
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
            assert!(
                !fresh.logfloor_admitted(),
                "a fresh open has admitted none by a write of its own"
            );
            let before = fresh.ops().fsyncs;
            assert_eq!(fresh.raise_logfloor(60).unwrap(), 100);
            assert_eq!(
                fresh.ops().fsyncs,
                before,
                "its first raise admits the floor its proof names, written by none (R6)"
            );
            assert!(fresh.logfloor_admitted());
            std::fs::remove_file(
                fresh
                    .proof_of_what_reads(LOGFLOOR)
                    .expect("the floor reads"),
            )
            .expect("the proof");
            let (_bare_core, bare) = open(root, 4);
            let before = bare.ops().fsyncs;
            assert_eq!(bare.raise_logfloor(60).unwrap(), 100);
            assert_eq!(
                bare.ops().fsyncs,
                before + 2,
                "CONTROL: with no proof, a fresh open's first raise writes it"
            );
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
            .suffix(".noindex")
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
        at: None,
    };
    for seq in [7, 8] {
        st.set_in_flight(&crate::state::InFlight {
            pin: pin(seq),
            subject: "/f/sim/in/n-1/s-a/n-1/note".to_string(),
            body: b"v=1 t=5 text=hello".to_vec(),
            log: astream_broker::LogIdentity::parse(&format!("{}.3", "5a".repeat(16))),
            at: None,
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
    for place in ["inflight", "sent", "acks", "keys", "unhandled", "journal"] {
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

/// A world retained by aterm's rules at bytes a few records reach ([`super::world::SCALED`]):
/// unread mail goes for its class's bytes as well as its age.
fn scaled(seed: u64) -> Opts {
    let mut o = Opts::seed(seed);
    o.retained = true;
    o.scaled = true;
    o
}

/// `o` over a broker that retains nothing, its mirror `turns` at the whole window: none of
/// retention's draws (sim item B05). A control whose rare seed met its bug before those
/// draws keeps that world, where the seed still meets it: the draws move every world they
/// touch, and in them no seed within the rare search does.
fn without_retention(mut o: Opts) -> Opts {
    o.retained = false;
    o.scaled = false;
    o.window = None;
    o.auto = false;
    o.cuts = false;
    o
}

/// [`scaled`], mirroring, its window budget a few records: the mirror waits on it often.
fn windowed(seed: u64) -> Opts {
    let mut o = scaled(seed);
    o.mirror = true;
    o.window = Some(256);
    o
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

/// The keyed world (critic item F23, [`Opts::keys`]): the seed's world, its notes keyed half the
/// time and re-posted under their keys, a cut landing after one dedup answer in two.
fn keyed(seed: u64) -> Opts {
    Opts {
        keys: true,
        ..Opts::seed(seed)
    }
}

/// The stray world (critic item F23, [`Opts::strays`]): the seed's world over a retained broker,
/// another writer under the node's producer id taking the sequence of one refused publish in
/// two, a cut landing after one dedup answer in two. Retained, as every broker aterm ships: on
/// one that set no rules nothing drops a stray record's key, so nothing ends the hold it
/// puts on a sender (the limit `Bridge::unverified_publication` states).
fn strayed(seed: u64) -> Opts {
    Opts {
        strays: true,
        retained: true,
        ..Opts::seed(seed)
    }
}

/// The unverifiable answers the outcomes met, by case (index 0 is case 1).
fn unverifiable_by_case(outs: &[Outcome]) -> [u64; 5] {
    outs.iter().fold([0u64; 5], |mut all, out| {
        for (sum, n) in all.iter_mut().zip(out.stats.unverifiable) {
            *sum += n;
        }
        all
    })
}

/// **A SENDER AN UNVERIFIABLE ANSWER HOLDS WAITS ALONE, AND ITS POSTS LAND IN ORDER** (the F23
/// sim's stray world, [`Opts::strays`]): another writer under the node's producer id takes the
/// sequence of one refused publish in two, so the node's retry at its pin meets a dedup answer
/// with a record of none of the pin's faces (`Bridge::unverified_publication`'s case 5), or,
/// a cut landing after the answer, one whose record retention gave up before its read back
/// (case 4). Each holds only its own sender's lane ([`crate::unverified_hold`]) until the cut
/// that drops the stray's key, and every law holds: the sender's posts on the bus in its order,
/// every post and receipt retired once settled, the world settling while others' posts land.
/// The answers are counted ([`crate::fx::ReservationTap`]); F21's mutants are found here
/// ([`each_rule_switched_off_is_caught`]).
#[test]
fn every_seed_of_the_stray_world_holds_every_law() {
    let outs = run_all(&strayed, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds of the stray world broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let sum = |field: fn(&super::world::Stats) -> u64| -> u64 {
        outs.iter().map(|out| field(&out.stats)).sum()
    };
    let unverifiable = unverifiable_by_case(&outs);
    let counts = [
        ("stray records", sum(|s| s.strays)),
        (
            "cuts between a dedup answer and the next request",
            sum(|s| s.cuts_after_dedup),
        ),
        ("unverifiable answers of case 5", unverifiable[4]),
    ];
    eprintln!(
        "the stray world over {SEEDS} seeds: {counts:?}; unverifiable by case {unverifiable:?}, \
         verified {}, collisions {}, weeks the settle let pass for a stray {}",
        sum(|s| s.reservations_verified),
        sum(|s| s.reservation_collisions),
        sum(|s| s.stray_weeks)
    );
    for &(what, n) in &counts {
        assert!(n > 0, "the seeds met no {what}: {counts:?}");
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

/// The committed world, retained, its sessions fetching records of their lanes by offset
/// (finding F3, [`Opts::fetches`]): retention's draws cut under that traffic, and its opens
/// under changed rules leave mail held below every lane's oldest.
fn fetching(seed: u64) -> Opts {
    Opts {
        retained: true,
        fetches: true,
        ..Opts::seed(seed)
    }
}

/// **A FETCH BY OFFSET IS ANSWERED AS THE BROKER HOLDS ITS RECORD** (finding F3): over a
/// retaining broker cutting under the traffic, every session's fetch of a record of its lane
/// is answered with the record while the broker holds it, a record it carries below the
/// lane's oldest included (the bridge reads a refused read again told, `Bridge::read_at`),
/// `retained` only once retention gave it up, and `no-record` only where the lane holds none.
/// The control, `Mutant::ReadAtWithoutTheHeld` (the refusal taken for no rows), is
/// [`each_rule_switched_off_is_caught`]'s.
#[test]
fn every_seed_of_the_fetching_world_holds_every_law() {
    let outs = run_all(&fetching, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds of the fetching world broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let sum = |field: fn(&super::world::Stats) -> u64| -> u64 {
        outs.iter().map(|out| field(&out.stats)).sum()
    };
    let counts = [
        ("fetches asked", sum(|s| s.fetches)),
        ("fetches answered with the record", sum(|s| s.fetched)),
        (
            "records fetched from below their lane's oldest",
            sum(|s| s.fetched_below_oldest),
        ),
        ("fetches answered `retained`", sum(|s| s.fetched_retained)),
        ("fetches answered `no-record`", sum(|s| s.fetched_none)),
        ("records retention gave up", sum(|s| s.given_up)),
        ("opens under changed rules", sum(|s| s.changed_starts)),
    ];
    eprintln!("the fetching world over {SEEDS} seeds: {counts:?}");
    for &(what, n) in &counts {
        assert!(n > 0, "the seeds exercised no {what}: {counts:?}");
    }
}

/// **A KEY'S POSTS ARE ONE POST ON THE BUS** (the F23 sim's keyed world, [`Opts::keys`]): half
/// the notes carry an idempotency key and are posted again under it, a cut lands after one
/// dedup answer in two, and every law holds, the key's own ([`super::world`]'s `key_copies`)
/// among them: a re-post is retired at its key's earlier record (`dup=1`) while retention
/// holds it, read back first (`Bridge::keyed_winner_at`, whose control is
/// [`each_rule_switched_off_is_caught`]'s), and lands a copy of its own only once retention
/// gave that record up. The reservations its worlds publish at their pins are counted
/// ([`crate::fx::ReservationTap`]): the read backs leave a dedup answer only to a re-post whose
/// key pin names no winner's offset yet, its winner still in aterm's outbox (its landing not
/// yet reconciled, or the disk refused that write), so the winner's record is undelivered and
/// unread, and aterm's rules give it up only past the mail rule's bytes or a week: a cut after
/// the answer gives it up only where those bytes are a few records (the scaled seeds,
/// [`Opts::scaled`]), so case 4 is rare here, not impossible. No seed of these, nor of
/// 1..=2,000 (measured), meets an answer it cannot verify (`Bridge::unverified_publication`;
/// the stray world does: [`every_seed_of_the_stray_world_holds_every_law`]), nor a re-post's
/// in-flight entry the attach's replay settles at its key's winner (counted too; its rule's
/// tests are bridge's `a_keyed_repost_past_a_cut_that_carried_its_winner_is_retired_at_the_winner`
/// and `a_keyed_repost_whose_own_copy_the_replay_read_back_is_retired_at_that_copy`).
#[test]
fn every_seed_of_the_keyed_world_holds_every_law() {
    let outs = run_all(&keyed, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds of the keyed world broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let sum = |field: fn(&super::world::Stats) -> u64| -> u64 {
        outs.iter().map(|out| field(&out.stats)).sum()
    };
    let counts = [
        ("keyed re-posts", sum(|s| s.reposts)),
        (
            "re-posts retired at their key's earlier record",
            sum(|s| s.reposts_deduped),
        ),
        (
            "re-posts that landed past their key's record given up",
            sum(|s| s.reposts_appended),
        ),
        (
            "cuts between a dedup answer and the next request",
            sum(|s| s.cuts_after_dedup),
        ),
        ("records retention gave up", sum(|s| s.given_up)),
    ];
    let reservations = (
        sum(|s| s.reservations_verified),
        sum(|s| s.reservation_collisions),
        outs.iter().fold([0u64; 5], |mut all, out| {
            for (sum, n) in all.iter_mut().zip(out.stats.unverifiable) {
                *sum += n;
            }
            all
        }),
        sum(|s| s.replayed_at_the_winner),
    );
    eprintln!(
        "the keyed world over {SEEDS} seeds: {counts:?}; reservations at their pins \
         (verified, collisions, unverifiable by case) and entries replayed at their key's \
         winner: {reservations:?}"
    );
    for &(what, n) in &counts {
        assert!(n > 0, "the seeds exercised no {what}: {counts:?}");
    }
}

/// The committed world, retained, its sessions asking with deadlines and following broadcast
/// topics the peer says on, aterm handing its sessions off now and then (item F4,
/// [`Opts::deadlines`], [`Opts::topics`], [`Opts::handoffs`]): retention's draws cut under
/// that traffic, past what a deadline sweep and a topic replay read.
fn asking(seed: u64) -> Opts {
    Opts {
        retained: true,
        deadlines: true,
        topics: true,
        handoffs: true,
        ..Opts::seed(seed)
    }
}

/// [`asking`], its state dir's device refusing writes in every seed ([`Opts::write_errors`]).
fn asking_erring(seed: u64) -> Opts {
    Opts {
        write_errors: true,
        ..asking(seed)
    }
}

/// **A DEADLINE FIRES ONCE FOR EVERY ASK NOBODY ANSWERED, AND A FOLLOWER SEES EVERY BROADCAST
/// IT IS OWED OR AN HONEST GAP** (item F4): over a retaining broker cutting under the traffic,
/// across bridge kills, handoffs, power cuts and broker restarts, every ask with a deadline
/// is answered or expired once its deadline passed, an `expired` names an ask nobody answered
/// first, and it lands once; a session following a topic is handed every broadcast of it the
/// broker holds from where it follows it, in offset order, once each, and a `link gap …
/// topic=` covers each one retention gave up. The controls (the sweep's and the replay's
/// plain fetch among them) are [`each_rule_switched_off_is_caught`]'s.
///
/// THE WIDE SEARCH (item F4, 2026-10-26): seeds 1..=10000 of this world and of
/// [`asking_erring`] break no law. The first such sweep found 26 and 47 failing. All but one
/// class were bugs these worlds' handoffs and write errors reached for the first time, each
/// fixed with its control above (a hand-off's lost heads, an answer or an `expired` delivered
/// over an unwritten deadline table). The one was the topic law's own: it read a follow's
/// `since=` on the broker's log, not on the log its session was last told, and judged the
/// bridge's move of the cursor to the fork, a move that session had yet to be told, a delivery
/// below `since`. The law was corrected, not the bridge, and a found seed pins the correction
/// ([`FOUND_ASKING`], whose `Mutant::FollowOnTheBrokersLog` puts the old reading back). The
/// sweeps after each fix found 9 and 18, then 1 and 2, then none. Run again on the tree merged
/// with critic item B09 and the F4 review's deadline-table fixes (2026-10-27): none of the
/// 10,000 of either world, and the old reading fails one (6340). Each world's 10,000 seeds take
/// about seven minutes here.
#[test]
fn every_seed_of_the_asking_world_holds_every_law() {
    let outs = run_all(&asking, 1..=SEEDS);
    let failures: Vec<String> = outs
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {SEEDS} seeds of the asking world broke a law:\n{}",
        failures.len(),
        failures.join("\n")
    );
    let sum = |field: fn(&super::world::Stats) -> u64| -> u64 {
        outs.iter().map(|out| field(&out.stats)).sum()
    };
    let counts = [
        (
            "asks that landed with a deadline",
            sum(|s| s.asks_with_deadline),
        ),
        ("asks answered in time", sum(|s| s.answered_in_time)),
        ("asks that expired", sum(|s| s.expired)),
        ("asks answered after they expired", sum(|s| s.answered_late)),
        (
            "expiries a later bridge published",
            sum(|s| s.expired_across_incarnations),
        ),
        ("broadcasts said", sum(|s| s.says)),
        ("follows", sum(|s| s.follows)),
        (
            "follows from below the broker's base",
            sum(|s| s.follows_below_the_base),
        ),
        ("broadcasts handed", sum(|s| s.broadcasts_handed)),
        (
            "broadcasts given up with a gap told",
            sum(|s| s.broadcasts_given_up_told),
        ),
        ("handoffs", sum(|s| s.handoffs)),
        ("kills", sum(|s| s.kills)),
        ("power cuts", sum(|s| s.power_cuts)),
        ("records retention gave up", sum(|s| s.given_up)),
    ];
    eprintln!("the asking world over {SEEDS} seeds: {counts:?}");
    for &(what, n) in &counts {
        assert!(n > 0, "the seeds exercised no {what}: {counts:?}");
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
        (
            "quits whose closes the bus held before their bye",
            sum(|s| s.quit_closes_landed),
        ),
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
    // Unretained: over a retaining broker the old republish also lands a post again past a
    // cut that dropped its dedup key (sim item B05), and this control is the order's alone.
    let world = |s: u64| {
        let mut o = both_ways(s);
        o.retained = false;
        o
    };
    let republishing = move |s: u64| world(s).with(Mutant::RepublishInFlightOfAnEarlierBoot);
    let out = breaking_at(&republishing, "order:", 561, RARE_SEEDS)
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
    let decided = run(&world(out.seed));
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
    let taken = witnessed(&lossy, 288, RARE_SEEDS, &|out| {
        out.model.2.contains("DeliverTaken")
    })
    .expect("some run delivered a record a move took");
    assert!(taken.violations.is_empty(), "{}", report(&taken));
    let reused = witnessed(&Opts::seed, 236, RARE_SEEDS, &|out| {
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

/// An aterm that names no roster count (`EpRules::names_roster`, an older build) is read before
/// every drain as it always was, and every law holds against it, on eight seeds spread over the
/// world bits of [`Opts::seed`] (51 and 52 unretained, 64 `auto`). 23 is the one seed of the first
/// 128 where a bridge that trusts a stale count there breaks a law (routing). The arm itself is
/// held by `bridge_roster_tests`; a law it breaks only on a seed left out goes unseen here.
#[test]
fn an_aterm_that_names_no_roster_count_holds_every_law() {
    const SPREAD: [u64; 8] = [23, 25, 30, 42, 45, 51, 52, 64];
    let older = |s| {
        let mut o = Opts::seed(s);
        o.ep = EpRules {
            names_roster: false,
            ..EpRules::default()
        };
        o
    };
    let broke: Vec<String> = aterm_spec::check::each(&SPREAD, |&seed| run(&older(seed)))
        .iter()
        .filter(|out| !out.violations.is_empty())
        .map(report)
        .collect();
    assert!(
        broke.is_empty(),
        "{} of {} seeds broke a law:\n{}",
        broke.len(),
        SPREAD.len(),
        broke.join("\n")
    );
}

/// The sessions the reclaim scenario opens beside aterm's two.
const RECLAIM_SESSIONS: u64 = 8;

/// What the node may still hold past its baseline once the reclaim ran: the subjects a
/// departed session never held that its traffic opened on the node's own faces.
const RECLAIM_SLACK: u64 = 4;

/// **A DEPARTED SESSION'S SUBJECTS GO BACK TO THE NODE'S BUDGET** (sim item B02): eight
/// sessions open, each mails the next and is mailed by the peer, all leave, and once the
/// reclaim's wait has passed the node holds what it held before them, within a constant;
/// every law of a world and every retire's ([`super::world::run_reclaim`]) holds, on every
/// tier. NEGATIVE CONTROL: a sweep that gives nothing back ([`Mutant::NoSubjectRetire`])
/// keeps two subjects or more of each session.
#[test]
fn a_departed_sessions_subjects_go_back_to_the_budget() {
    let n = RECLAIM_SESSIONS;
    let runs = aterm_spec::check::each(&[Tier::Strict, Tier::Ordered, Tier::Relaxed], |tier| {
        super::world::run_reclaim(&Opts::reclaiming(3, *tier), n as usize, false)
    });
    for (tier, out) in [Tier::Strict, Tier::Ordered, Tier::Relaxed]
        .iter()
        .zip(&runs)
    {
        assert!(
            out.violations.is_empty(),
            "{tier}: {:#?}\n{}",
            out.violations,
            out.trace.join("\n")
        );
        assert!(
            out.peak >= out.baseline + 2 * n,
            "{tier}: the sessions spent {} subjects, not their rows and lanes: {out:?}",
            out.peak - out.baseline
        );
        assert!(
            out.after <= out.baseline + RECLAIM_SLACK,
            "{tier}: {} subjects past the baseline stayed spent once the sessions left: {:?}",
            out.after - out.baseline,
            (out.baseline, out.peak, out.after)
        );
        eprintln!(
            "{tier}: spent {} -> {} with {n} sessions -> {} once they left",
            out.baseline, out.peak, out.after
        );
    }
    let off = super::world::run_reclaim(
        &Opts::reclaiming(3, Tier::Relaxed).with(Mutant::NoSubjectRetire),
        n as usize,
        false,
    );
    assert!(off.violations.is_empty(), "{:#?}", off.violations);
    assert!(
        off.after >= off.baseline + 2 * n,
        "CONTROL NoSubjectRetire: the node held {} subjects past its baseline with nothing \
         given back, not two a session",
        off.after - off.baseline
    );
}

/// **A SESSION REVIVED BETWEEN THE SWEEP'S READ AND ITS RETIRE KEEPS ITS ROW AND ITS
/// MAIL** (sim item B02): a successor's `live` row reaches the broker just ahead of the
/// retire of the `exited` row the sweep read, and aterm serves the session again. The
/// conditional retire is moot, the row stays in the index, and the peer's mail to it
/// since is delivered. NEGATIVE CONTROL: a sweep that gives the row back with the
/// unconditional `/a/retire` ([`Mutant::RetireUnconditionally`]) takes the successor's
/// `live` row out of the index.
#[test]
fn a_session_revived_between_the_read_and_the_retire_keeps_its_row_and_its_mail() {
    let out = super::world::run_reclaim(&Opts::reclaiming(5, Tier::Strict), 3, true);
    let sid = out
        .revived
        .clone()
        .unwrap_or_else(|| panic!("no retire met the revival:\n{}", out.trace.join("\n")));
    assert!(
        out.violations.is_empty(),
        "{sid}: {:#?}\n{}",
        out.violations,
        out.trace.join("\n")
    );
    assert!(
        out.trace
            .iter()
            .any(|l| l.contains(&format!("{sid}/presence if @")) && l.ends_with(" moot")),
        "the retire of {sid}'s row was not moot:\n{}",
        out.trace.join("\n")
    );
    let off = super::world::run_reclaim(
        &Opts::reclaiming(5, Tier::Strict).with(Mutant::RetireUnconditionally),
        3,
        true,
    );
    assert!(off.revived.is_some(), "CONTROL: no retire met the revival");
    assert!(
        off.violations
            .iter()
            .any(|v| v.starts_with("reclaim:") && v.contains("Some(\"live\")")),
        "CONTROL RetireUnconditionally: the successor's live row was not given back: {:#?}",
        off.violations
    );
}

/// **A RESERVATION A POWER CUT LEFT HOLDS NOTHING BACK PAST THE NEXT BOOT** (item C14): a
/// session's post is pinned and lands, and the power goes before its retire does; the next
/// boot's first start retires the reservation of that session, which no drain will retire, so
/// once the reclaim's wait has passed the node has given back both sessions' exited presence
/// rows and the lanes it wrote to them, that post's lane among them
/// ([`super::world::run_reclaim_across_a_cut`]); every law of a world holds. NEGATIVE
/// CONTROL: a sweep that retires no reservation ([`Mutant::KeepEarlierBootReservations`])
/// keeps every one of them charged.
#[test]
fn a_reservation_a_power_cut_left_holds_nothing_back_past_the_next_boot() {
    let fixed = super::world::run_reclaim_across_a_cut(&Opts::reclaiming(7, Tier::Strict));
    let control = super::world::run_reclaim_across_a_cut(
        &Opts::reclaiming(7, Tier::Strict).with(Mutant::KeepEarlierBootReservations),
    );
    for (arm, out, held_after) in [("fixed", &fixed, false), ("CONTROL", &control, true)] {
        held_across_a_cut(arm, out, held_after);
    }
}

/// **OFF THE BOOT VOLUME TOO** (item RZ2): the world of
/// [`a_reservation_a_power_cut_left_holds_nothing_back_past_the_next_boot`] with its state dir
/// off the boot volume ([`Opts::off_boot_volume`]), whose page cache vouches for no lease and
/// where every lease names the kernel's boot all the same: the next boot's first start finds
/// the earlier boot, retires the reservation, and the subjects go back; every law of a world
/// holds. NEGATIVE CONTROL: the leases name no boot there, as before RZ2
/// ([`Mutant::LeaseNamesNoBootOffTheBootVolume`]), and every subject stays charged.
#[test]
fn off_the_boot_volume_a_reservation_a_power_cut_left_holds_nothing_back_past_the_next_boot() {
    let off = |opts: Opts| Opts {
        off_boot_volume: true,
        ..opts
    };
    let fixed = super::world::run_reclaim_across_a_cut(&off(Opts::reclaiming(7, Tier::Strict)));
    let control = super::world::run_reclaim_across_a_cut(&off(
        Opts::reclaiming(7, Tier::Strict).with(Mutant::LeaseNamesNoBootOffTheBootVolume)
    ));
    for (arm, out, held_after) in [("fixed", &fixed, false), ("CONTROL", &control, true)] {
        held_across_a_cut(arm, out, held_after);
    }
}

/// Every law of the world held, the post was reserved and unretired when the power went, and
/// each subject it charged then is charged once the next boot's reclaim was due iff `held_after`.
fn held_across_a_cut(arm: &str, out: &super::world::ReclaimedAcrossACut, held_after: bool) {
    assert!(
        out.violations.is_empty(),
        "{arm}: {:#?}\n{}",
        out.violations,
        out.trace.join("\n")
    );
    assert!(
        out.pinned_at_the_cut,
        "{arm}: the post was not reserved, on the bus and unretired when the power went:\n{}",
        out.trace.join("\n")
    );
    assert!(!out.subjects.is_empty(), "{arm}: no subject was judged");
    for (subject, then, now) in &out.subjects {
        assert!(
            *then,
            "{arm}: {subject} was not charged when the power went"
        );
        assert_eq!(
            *now,
            held_after,
            "{arm}: {subject} once the next boot's reclaim was due:\n{}",
            out.trace.join("\n")
        );
    }
}
