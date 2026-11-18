// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault log rooted at a final checkpoint (tracker FL-148c): it evaluates as the whole log
//! does and computes the next checkpoint as it does, a prune of a union is the union of the
//! prunes, it takes no dropped record back, it places a record that follows a dropped one in one
//! walk over the stubs, and a log pruned at one side of a split merges the other side's records.

use super::*;
use crate::membership_log::checkpoint::tests::{
    A, B, C, D, HOUR, Mac, NOW, SPLIT, T, draft, draft_after, epoch, found, hold, name, one,
    share_at, supersedes, two,
};
use crate::membership_log::checkpoint::{DAY_MS, MIN_AGE_MS, checkpoint_id, plan_at, propose};
use crate::membership_log::{
    Draft, EpochAudience, Inserted, Kind, RecordBudgets, Statement, Store, budgets,
};
use crate::text::{Audience, Entry};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

/// A seeded xorshift: the DAGs are the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// How a write states what it read: version 1 (`None`, 40%), or version 2, misstating it as
/// [`lie`] draws.
fn statement(rng: &mut Rng) -> Option<Lie> {
    if rng.chance(40) { None } else { Some(lie(rng)) }
}

/// The truth (70%), or a statement that names nothing (10%), claims a carry of the oldest op the
/// writer read, of whatever kind (10%; a `Delete` names that op alone), or states no carry (10%).
fn lie(rng: &mut Rng) -> Lie {
    match rng.below(10) {
        0 => Lie::Missing,
        1 => Lie::FalseCarry,
        2 => Lie::NoCarry,
        _ => Lie::Truth,
    }
}

/// What one op wrote, as the generator knows it.
#[derive(Clone, Debug)]
struct Op {
    register: (EpochAudience, String),
    /// The value, `None` for a `Delete`.
    value: Option<Vec<u8>>,
    /// The op whose value it carries on, itself where it writes a new one.
    rep: RecordId,
}

/// Three Macs writing a vault on their own logs and syncing now and then.
pub(in crate::membership_log) struct World {
    macs: [Mac; 3],
    logs: Vec<Log>,
    ops: BTreeMap<RecordId, Op>,
    pub(in crate::membership_log) clock: u64,
    next_value: u64,
    removed: bool,
    /// The Mac that sleeps across the cut, if any: B, silent from 42 days before it until after
    /// it, so it is no recent seat, and wakes on the log it last took.
    sleeper: Option<usize>,
    asleep: bool,
    /// Whether B has signed its share of an undo of A's removal of C.
    undo_shared: bool,
    /// Each version-2 op whose statement misstates what its writer read, and how.
    lies: BTreeMap<RecordId, Lie>,
}

fn label(mac: Mac) -> String {
    format!("m{}", mac.0)
}

pub(in crate::membership_log) fn entry(
    audience: &EpochAudience,
    name: &str,
    value: &[u8],
) -> Entry {
    Entry {
        name: name.to_owned(),
        value: crate::SecretBytes::from(value),
        audience: audience.as_text_audience().expect("a vault audience"),
        if_absent: false,
        anchor: None,
    }
}

impl World {
    fn new(start: u64) -> Self {
        let mut base = found(&[A, B, C]);
        epoch(&mut base, A, start, EpochAudience::Everyone, 1, &[A, B, C]);
        for mac in [A, B, C] {
            let hosts = EpochAudience::Hosts(vec![label(mac)]);
            epoch(&mut base, mac, start + 1, hosts, 1, &[mac]);
        }
        Self {
            macs: [A, B, C],
            logs: vec![base.clone(), base.clone(), base],
            ops: BTreeMap::new(),
            clock: start + 2,
            next_value: 0,
            removed: false,
            sleeper: None,
            asleep: false,
            undo_shared: false,
            lies: BTreeMap::new(),
        }
    }

    /// The newest epoch of `audience` in Mac `m`'s log that it holds the key of.
    fn epoch_of(
        &self,
        m: usize,
        audience: &EpochAudience,
    ) -> Option<(RecordId, Zeroizing<[u8; 32]>)> {
        let log = &self.logs[m];
        let keys = log.unwrap_epochs(&self.macs[m].identity()).ok()?;
        log.newest_epochs(audience)
            .into_iter()
            .find_map(|e| keys.get(&e).map(|k| (e, k.clone())))
    }

    /// The ops of `register` Mac `m`'s log holds, and of them those no other one follows.
    fn register_ops(
        &self,
        m: usize,
        register: &(EpochAudience, String),
    ) -> (Vec<RecordId>, Vec<RecordId>) {
        let log = &self.logs[m];
        let all: Vec<RecordId> = self
            .ops
            .iter()
            .filter(|(id, op)| op.register == *register && log.get(id).is_some())
            .map(|(id, _)| *id)
            .collect();
        let frontier = all
            .iter()
            .filter(|op| {
                !all.iter()
                    .any(|other| other != *op && log.reaches(&[*other], op))
            })
            .copied()
            .collect();
        (all, frontier)
    }

    fn hold_local(&mut self, m: usize, record: Record) -> RecordId {
        *hold(&mut self.logs[m], record).id()
    }

    /// Mac `m` writes `value` (a new one, or the one `carried` carries) to `register`, or deletes
    /// it, as record version 1, or as version 2 stating what it read, truly or as `lie` says.
    fn write(
        &mut self,
        m: usize,
        register: &(EpochAudience, String),
        value: Option<Vec<u8>>,
        carried: Option<RecordId>,
        version_two: Option<Lie>,
    ) {
        let Some((epoch_id, key)) = self.epoch_of(m, &register.0) else {
            return;
        };
        let mac = self.macs[m];
        let log = &self.logs[m];
        let drafted = match &value {
            Some(value) => log.draft_put(
                &mac.id(),
                self.clock,
                epoch_id,
                &key,
                &entry(&register.0, &register.1, value),
            ),
            None => log.draft_delete(&mac.id(), self.clock, epoch_id, &key, &register.1),
        };
        let Ok(drafted) = drafted else {
            return;
        };
        let (all, _) = self.register_ops(m, register);
        let mut named: Vec<RecordId> = all
            .into_iter()
            .filter(|op| log.reaches(&drafted.parents, op))
            .collect();
        named.sort_unstable();
        let carries = carried.map(|c| self.ops[&c].rep);
        let record = match version_two {
            None => one(drafted, mac),
            Some(lie) => {
                let statement = match lie {
                    Lie::Truth => Statement {
                        supersedes: named,
                        carries,
                    },
                    Lie::Missing => Statement::default(),
                    // A `Delete` carries nothing: it names the oldest op it read alone.
                    Lie::FalseCarry if value.is_none() => Statement {
                        supersedes: named.first().copied().into_iter().collect(),
                        carries: None,
                    },
                    // The oldest op it read, of whatever kind, claimed as the value it seals.
                    Lie::FalseCarry => Statement {
                        carries: named.first().copied(),
                        supersedes: named,
                    },
                    Lie::NoCarry => Statement {
                        supersedes: named,
                        carries: None,
                    },
                };
                two(drafted, mac, Some(&statement))
            }
        };
        let id = *record.id();
        let rep = carried.map_or(id, |c| self.ops[&c].rep);
        if let Some(lie) = version_two.filter(|lie| *lie != Lie::Truth) {
            self.lies.insert(id, lie);
        }
        self.hold_local(m, record);
        self.ops.insert(
            id,
            Op {
                register: register.clone(),
                value,
                rep,
            },
        );
    }

    fn step(&mut self, rng: &mut Rng, before_cut: bool) {
        let m = usize::try_from(rng.below(3)).unwrap();
        let mac = self.macs[m];
        if self.asleep && self.sleeper == Some(m) {
            self.clock += 1 + rng.below(6 * HOUR);
            return;
        }
        if !before_cut && self.removed && !self.undo_shared && m == 1 {
            // B signs its share of an undo of A's removal of C, at the cutoff its log reads: A's
            // `Put` just before the removal, which A superseded before the cut.
            if let Some(cutoff) = self.logs[1].undo_cutoff(&A.id()) {
                let body = Body::Remove {
                    member: A.id(),
                    cutoff,
                };
                let record = one(draft(&self.logs[1], B, self.clock, body), B);
                self.hold_local(1, record);
                self.undo_shared = true;
            }
        }
        let names = ["k0", "k1", "k2"];
        let audience = if rng.chance(80) {
            EpochAudience::Everyone
        } else {
            EpochAudience::Hosts(vec![label(mac)])
        };
        let register = (
            audience.clone(),
            names[usize::try_from(rng.below(3)).unwrap()].to_owned(),
        );
        match rng.below(100) {
            0..40 => {
                self.next_value += 1;
                let value = format!("v{}", self.next_value).into_bytes();
                let two = statement(rng);
                self.write(m, &register, Some(value), None, two);
            }
            40..50 => {
                if !self.register_ops(m, &register).0.is_empty() {
                    let two = statement(rng);
                    self.write(m, &register, None, None, two);
                }
            }
            50..62 => {
                // A carry: the one live op's value sealed again under the newest epoch.
                let (_, frontier) = self.register_ops(m, &register);
                if let [live] = frontier[..]
                    && let Some(value) = self.ops[&live].value.clone()
                    && let Some((newest, _)) = self.epoch_of(m, &audience)
                    && !self.logs[m].get(&live).is_some_and(
                        |record| matches!(record.body(), Body::Put { epoch, .. } if *epoch == newest),
                    )
                {
                    let lie = lie(rng);
                    self.write(m, &register, Some(value), Some(live), Some(lie));
                }
            }
            62..70 => {
                // A rotation of `[everyone]` to the seats this Mac's log holds.
                let log = &self.logs[m];
                let generation = log
                    .records()
                    .filter_map(|r| match r.body() {
                        Body::Epoch {
                            audience: EpochAudience::Everyone,
                            generation,
                            ..
                        } => Some(*generation),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0)
                    + 1;
                let seated = log.membership();
                let to: Vec<Mac> = self
                    .macs
                    .iter()
                    .copied()
                    .filter(|mac| seated.members().contains_key(&mac.id()))
                    .collect();
                if to.contains(&mac) {
                    epoch(
                        &mut self.logs[m],
                        mac,
                        self.clock,
                        EpochAudience::Everyone,
                        generation,
                        &to,
                    );
                }
            }
            70..78 => {
                let log = &self.logs[m];
                let record = one(
                    draft(
                        log,
                        mac,
                        self.clock,
                        name(&format!("n{}", self.clock % 997)),
                    ),
                    mac,
                );
                self.hold_local(m, record);
            }
            78..81 if before_cut && !self.removed && m == 0 => {
                // A writes `k0`, removes C at C's newest record it holds, rotates `[everyone]`
                // without it, and writes `k0` again: the cutoff an undo of the removal signs is
                // the first `Put`, which the second supersedes.
                let k0 = (EpochAudience::Everyone, "k0".to_owned());
                self.next_value += 1;
                let value = format!("v{}", self.next_value).into_bytes();
                self.write(0, &k0, Some(value), None, Some(Lie::Truth));
                self.clock += 1;
                let log = &self.logs[0];
                let cutoff = log
                    .records()
                    .filter(|r| r.kind() != Kind::Genesis && r.actor() == &C.id())
                    .max_by_key(|r| r.sequence())
                    .map_or(*log.network(), |r| *r.id());
                let remove = Body::Remove {
                    member: C.id(),
                    cutoff,
                };
                let record = one(draft(log, A, self.clock, remove), A);
                self.hold_local(0, record);
                self.removed = true;
                let generation = 100;
                epoch(
                    &mut self.logs[0],
                    A,
                    self.clock + 1,
                    EpochAudience::Everyone,
                    generation,
                    &[A, B],
                );
                self.clock += 2;
                self.next_value += 1;
                let value = format!("v{}", self.next_value).into_bytes();
                self.write(0, &k0, Some(value), None, Some(Lie::Truth));
            }
            _ => {
                let from = usize::try_from(rng.below(3)).unwrap();
                if from != m {
                    let other = self.logs[from].clone();
                    self.logs[m].merge(&other).unwrap();
                }
            }
        }
        self.clock += 1 + rng.below(6 * HOUR);
    }

    /// The sleeper wakes on the log it last took, before it takes any other: it removes C at
    /// C's newest `Put` or `Delete` there, which C may since have superseded, and for each
    /// register whose ops there leave one live `Put` it seals that value again (version 2,
    /// stating the carry, or misstating it as [`lie`] draws), where the Macs that stayed awake
    /// may since have replaced it; and it deletes one register, stating what it read as replaced
    /// (or misstating it).
    fn wake(&mut self, rng: &mut Rng) {
        let Some(s) = self.sleeper else {
            return;
        };
        self.asleep = false;
        let mac = self.macs[s];
        let stale_cutoff = self.logs[s]
            .records()
            .filter(|r| r.actor() == &C.id() && matches!(r.kind(), Kind::Put | Kind::Delete))
            .max_by_key(|r| r.sequence())
            .map(|r| *r.id());
        if let Some(cutoff) = stale_cutoff
            && s != 2
        {
            let body = Body::Remove {
                member: C.id(),
                cutoff,
            };
            let record = one(draft(&self.logs[s], mac, self.clock, body), mac);
            self.hold_local(s, record);
            self.clock += 1;
        }
        for name in ["k0", "k1", "k2"] {
            let register = (EpochAudience::Everyone, name.to_owned());
            let (_, frontier) = self.register_ops(s, &register);
            if let [live] = frontier[..]
                && let Some(value) = self.ops[&live].value.clone()
            {
                let lie = lie(rng);
                self.write(s, &register, Some(value), Some(live), Some(lie));
                self.clock += 1;
            }
        }
        let register = (
            EpochAudience::Everyone,
            ["k0", "k1", "k2"][usize::try_from(rng.below(3)).unwrap()].to_owned(),
        );
        if !self.register_ops(s, &register).0.is_empty() {
            let lie = lie(rng);
            self.write(s, &register, None, None, Some(lie));
            self.clock += 1;
        }
    }

    /// Every Mac takes every other's log and signs a share of `body`, so each log that holds the
    /// shares holds it settled ([`checkpoint::settled`]) whatever its finality, and a Mac that
    /// prunes at it drops only what a settled prune drops ([`checkpoint::gone`]).
    /// A Mac that already signed a share of `body` ([`World::share`]) signs none again.
    pub(in crate::membership_log) fn settle(&mut self, body: &Body) {
        let whole = self.whole();
        for m in 0..self.macs.len() {
            self.share_from(m, &whole, body);
        }
        self.clock += 1;
    }

    /// Mac `m` alone takes every other's log and signs a share of `body`: a minority share,
    /// which settles nothing where the cut's other recent seats sign none.
    fn share(&mut self, m: usize, body: &Body) {
        let whole = self.whole();
        self.share_from(m, &whole, body);
        self.clock += 1;
    }

    /// Mac `m` takes `whole` and signs a share of `body`, unless it holds one of its own.
    fn share_from(&mut self, m: usize, whole: &Log, body: &Body) {
        let mac = self.macs[m];
        self.logs[m].merge(whole).unwrap();
        let id = checkpoint_id(body);
        if self.logs[m]
            .records()
            .any(|r| r.actor() == &mac.id() && checkpoint_id(r.body()) == id)
        {
            return;
        }
        let record = two(
            draft(&self.logs[m], mac, self.clock, body.clone()),
            mac,
            None,
        );
        assert_eq!(self.logs[m].insert(record).unwrap(), Inserted::Held);
    }

    /// `steps` more steps after the cut, from a generator of their own.
    pub(in crate::membership_log) fn run(&mut self, seed: u64, steps: usize) {
        let mut rng = Rng(seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1);
        for _ in 0..steps {
            self.step(&mut rng, false);
        }
    }

    /// Every Mac's log merged.
    pub(in crate::membership_log) fn whole(&self) -> Log {
        let mut whole = self.logs[0].clone();
        for log in &self.logs[1..] {
            whole.merge(log).unwrap();
        }
        whole
    }
}

/// A seeded vault: three Macs writing from 50 days before `T` for about a week, then (after a
/// quiet fortnight) for the 20 days before it (about `before` steps in all), and `after` steps
/// after it. In every other seed B sleeps from the quiet fortnight until after `T`, so it is no
/// recent seat, and wakes on its stale log ([`World::wake`]).
pub(in crate::membership_log) fn random_vault(seed: u64, before: usize, after: usize) -> World {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut world = World::new(T - 50 * DAY_MS);
    let early = before / 3;
    for _ in 0..early {
        if world.clock >= T - 42 * DAY_MS {
            break;
        }
        world.step(&mut rng, true);
    }
    if seed % 2 == 0 {
        world.sleeper = Some(1);
        world.asleep = true;
    }
    world.clock = world.clock.max(T - 20 * DAY_MS);
    for _ in early..before {
        if world.clock >= T - HOUR {
            break;
        }
        world.step(&mut rng, true);
    }
    world.clock = world.clock.max(T + HOUR);
    world.wake(&mut rng);
    for _ in 0..after {
        world.step(&mut rng, false);
    }
    world
}

/// What a log evaluates to for each Mac: the seats, the Macs out with their removals, the quorum
/// `Remove`s, the cutoff an undo of each Mac would sign, and each Mac's opened entries (names,
/// values, audiences), conflicts, whether an epoch is hidden from it, and what of them C, if out,
/// could read after its cutoff. The epochs a Mac unwrapped are left out: a prune drops the old
/// ones.
pub(in crate::membership_log) fn evaluated(log: &Log) -> String {
    let membership = log.membership();
    let mut out = format!(
        "seats {:?}\nout {:?}\nquorums {:?}\n",
        membership.members().keys().collect::<Vec<_>>(),
        membership.removed(),
        membership.quorums(),
    );
    for mac in [A, B, C] {
        let exposed: Vec<(Audience, String)> =
            log.read_after_cutoff(&mac.identity(), &C.id()).unwrap();
        out.push_str(&format!(
            "{} undo {:?} exposed to C {exposed:?}\n",
            label(mac),
            log.undo_cutoff(&mac.id()),
        ));
    }
    for mac in [A, B, C] {
        let opened = log.open_entry_set(&mac.identity()).unwrap();
        let read = opened.map(|o| {
            let entries: Vec<(String, Audience, Vec<u8>)> = o
                .entries
                .iter()
                .map(|e| {
                    (
                        e.name.clone(),
                        e.audience.clone(),
                        e.value.as_slice().to_vec(),
                    )
                })
                .collect();
            (entries, o.conflicts, o.hidden_epochs)
        });
        out.push_str(&format!("{}: {read:?}\n", label(mac)));
    }
    out
}

/// What else a log's registers and keyed epochs read for each Mac, less every register named in
/// `skip`: the op each winner stands for, the winning `Put`s and `Delete`s, the registers with
/// concurrent siblings, the epochs sealing a winner (counting and keyed), the uncovered keyed
/// epochs and keyed `Delete`s, the host audience that wins each name, each held carry's rep; and
/// where an undo of each Mac stands.
fn evaluated_registers(log: &Log, skip: &BTreeSet<String>) -> String {
    let mut out = String::new();
    for mac in [A, B, C] {
        out.push_str(&format!("{} undo {:?}\n", label(mac), log.undo(&mac.id())));
    }
    for mac in [A, B, C] {
        let Some(o) = log.open_audience_entries(&mac.identity()).unwrap() else {
            out.push_str(&format!("{}: none\n", label(mac)));
            continue;
        };
        let keep = |key: &(Audience, String)| !skip.contains(&key.1);
        let by_key = |map: BTreeMap<(Audience, String), RecordId>| {
            map.into_iter()
                .filter(|(key, _)| keep(key))
                .collect::<BTreeMap<_, _>>()
        };
        // The siblings as a set: their order is the log's, which follows the order its records
        // came in, so two logs that took the same records otherwise list them otherwise.
        let concurrent: BTreeMap<(Audience, String), BTreeSet<RecordId>> = o
            .concurrent
            .into_iter()
            .filter(|(key, _)| keep(key))
            .map(|(key, siblings)| (key, siblings.into_iter().collect()))
            .collect();
        let carried: BTreeMap<RecordId, RecordId> = o
            .carried
            .into_iter()
            .filter(|(carry, _)| log.get(carry).is_some())
            .collect();
        out.push_str(&format!(
            "{}: ops {:?}\nputs {:?}\ndeletes {:?}\nconcurrent {concurrent:?}\nkeyed {:?}\nuncovered {:?}\nkeyed deletes {:?}\nhosts {:?}\n",
            label(mac),
            by_key(o.winning_ops),
            by_key(o.winning_puts),
            by_key(o.winning_deletes),
            o.keyed_winning,
            o.uncovered_keyed,
            o.keyed_deletes,
            o.winning_host_audience
                .into_iter()
                .filter(|(name, _)| !skip.contains(name))
                .collect::<BTreeMap<_, _>>(),
        ));
        if skip.is_empty() {
            out.push_str(&format!(
                "epochs {:?}\ncarried {carried:?}\n",
                o.winning_epochs
            ));
        }
    }
    out
}

/// Whether `rooted` evaluates as `whole`: membership, entries and registers, the heads and their
/// digest.
pub(in crate::membership_log) fn assert_evaluates_as(
    seed: u64,
    phase: &str,
    rooted: &Log,
    whole: &Log,
) {
    assert_eq!(
        rooted.heads_digest(),
        whole.heads_digest(),
        "seed {seed} ({phase}): the heads digest is the whole log's"
    );
    assert_eq!(
        rooted.heads(),
        whole.heads(),
        "seed {seed} ({phase}): the heads stand"
    );
    assert_eq!(
        evaluated(rooted),
        evaluated(whole),
        "seed {seed} ({phase}): the rooted log evaluates as the whole log"
    );
    assert_eq!(
        evaluated_registers(rooted, &BTreeSet::new()),
        evaluated_registers(whole, &BTreeSet::new()),
        "seed {seed} ({phase}): the rooted log's registers and keyed epochs read as the whole log's"
    );
}

/// Each held record with the held records in its causal past: the order a log's evaluators read.
fn shape(log: &Log) -> BTreeMap<RecordId, BTreeSet<RecordId>> {
    let ids: Vec<RecordId> = log.records().map(|r| *r.id()).collect();
    ids.iter()
        .map(|id| {
            let past = ids
                .iter()
                .filter(|other| *other != id && log.reaches(&[*id], other))
                .copied()
                .collect();
            (*id, past)
        })
        .collect()
}

/// What the random vaults exercise, so the law is not met vacuously.
#[derive(Debug, Default)]
struct Coverage {
    pruned: usize,
    dropped: usize,
    v1_dropped: usize,
    kept_after_a_dropped_parent: usize,
    concurrent_after_a_dropped_parent: usize,
    removals: usize,
    /// Roots a `Remove` at a dropped cutoff took a share's count from, so that they are settled
    /// and no longer final ([`checkpoint::gone`] alone keeps the dropped ops dropped).
    settled_not_final: usize,
    /// Held version-2 ops whose statement misstates what their writer read, and those of them
    /// beside the cut.
    false_statements: usize,
    false_statements_beside_the_cut: usize,
    /// Held `Remove`s whose cutoff the prune dropped: B's share of an undo of A, and the
    /// sleeper's `Remove` of C.
    undo_at_a_dropped_cutoff: usize,
    sleeper_remove_at_a_dropped_cutoff: usize,
    /// Held version-2 ops beside the cut that state a carry of a dropped op, or a `Delete` that
    /// names one as replaced: what the register reads by statement.
    stated_carries_of_dropped: usize,
    stated_deletes_over_dropped: usize,
    /// Dropped `Delete`s whose stub keeps the rep their statement reads.
    dropped_delete_reps: usize,
    /// Keyed epochs with a dependent the prune dropped.
    keyed_with_a_dropped_dependent: usize,
    /// Stubs, each checked to take fewer bytes than its record.
    stubs_smaller_than_records: usize,
    /// Rooted logs extended past the cut whose next proposal was compared with the whole log's,
    /// and those then pruned again at it.
    next_proposals: usize,
    second_prunes: usize,
    /// Stubs in a second cut that the whole log's prune keeps (a rep a kept op carries, a kept
    /// `Remove`'s cutoff), which the second prune does not drop again.
    kept_stubs: usize,
}

pub(in crate::membership_log) const SEEDS: u64 = 32;

/// Over seeded random vaults (three Macs, version-1 and version-2 `Put`s and `Delete`s, carries
/// that name their representative, `[everyone]` rotations, host audiences, a removal before the
/// cut and the epochs a removed Mac minted past its cutoff, and, in every other seed, B asleep
/// across the cut and waking on its stale log: its `Remove` of C at a cutoff C has since
/// superseded, its carries of values the others replaced and a `Delete`; and B's share of an undo
/// of A's removal of C at A's `Put` before it, which A superseded), the log rooted at its
/// proposed checkpoint evaluates as the whole log does for every Mac (seats, removals, quorums,
/// the cutoff an undo of each Mac signs and where it stands, what the removed Mac reads after
/// its cutoff, and each Mac's entries, conflicts and hidden epochs, the op each winner stands
/// for, the winning `Put`s and `Delete`s, the concurrent siblings, the counting and keyed epochs
/// sealing a winner, the uncovered keyed epochs, the keyed `Delete`s, each name's winning host
/// audience and each held carry's rep), each held record's causal past is its past in the whole
/// log less the dropped records, the heads and their digest are the whole log's, each stub takes
/// fewer bytes than its record, the counts fall by what was dropped, and the rooted log computes
/// its own root at the cut as the whole log did. Then the vault runs on for 150 steps: the
/// rooted log, given the new records, still evaluates as the whole log, proposes the whole log's
/// next checkpoint, and pruned at it holds settled the checkpoints the whole log holds settled
/// (the first root among them, whose cut kept ops the second prune dropped) and evaluates as the
/// whole log and as the whole log pruned once there.
///
/// Every Mac shares each checkpoint the law prunes at ([`World::settle`]), so the whole log holds
/// it settled, and a `Remove` at a dropped cutoff (B's undo of A, the sleeper's of C) revives no
/// dropped op ([`checkpoint::gone`]): every register is compared, in every seed, and
/// `settled_not_final` counts the roots such a `Remove` took a share's count from. Writes state
/// what they read falsely too ([`lie`]): naming nothing, claiming a carry of the oldest op read,
/// whatever its kind, or stating no carry; every log reads each by its statement as far as
/// its names could be true. Record ids hold random salts, so which seeds conflict differs from
/// run to run; the coverage floors hold in every run.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail (record ids hold
/// random salts, so the failing seed may differ from run to run):
/// - `Mutant::NoPlace` (a stub resolves to no held record, so a record that follows a dropped
///   one resolves nowhere): ``assertion `left == right` failed: seed 1: each record's past is its
///   whole past less the dropped records``.
/// - `plan_of` with the stubs in the cut left out of `dropped`: ``assertion `left == right`
///   failed: seed 1: the rooted log computes its own root as the whole log does``.
/// - `Mutant::HeldCutoffs` (the evaluators read held records alone): ``assertion `left ==
///   right` failed: seed 3 (pruned twice): the rooted log's registers and keyed epochs read as
///   the whole log's``.
/// - `Mutant::RootedStatementsOnly` (a whole log reads a version-2 op by its frontier, a rooted
///   one by its statement, as before this slice): ``assertion `left == right` failed: seed 1
///   (first): the rooted log evaluates as the whole log``.
/// - `Mutant::Revives` (a record a settled prune dropped counts as its evaluation alone says):
///   ``assertion `left == right` failed: seed 4 (first): the rooted log's registers and keyed
///   epochs read as the whole log's`` in one run, ``seed 11 (pruned twice): the rooted log
///   evaluates as the whole log`` in another.
/// - `Mutant::HeldProposal` (the next checkpoint is proposed from the held records alone):
///   ``seed 1: the rooted log proposes the whole log's next checkpoint``.
/// - `Mutant::KeepNestedRoots` (a prune keeps every earlier root): ``seed 1: the second root
///   takes the place of the first, whose cut it nests``.
/// - `Mutant::StubsDroppedForGood` (a plan keeps a dropped op only for a carry, a cutoff or an
///   owed epoch): ``assertion `left == right` failed: seed 1: the log pruned twice holds settled
///   what the whole log does``, `left` the second root alone, `right` the first too.
///
/// `checkpoint::Mutant::CrossSigner` (a name supersedes another signer's op) passes it, as it
/// must: every log applies it alike, and what it loses is a value, which
/// `vault_checkpoint_a_stolen_key_drops_no_other_signers_op` holds.
///
/// `Mutant::FrontierOnly`, `Mutant::DeleteStandsForItself` and `Mutant::KeyedHeldOnly` no longer
/// fail it: what a settled prune dropped counts nowhere, so no frontier, `Delete` or keyed epoch
/// beside the cut reads a dropped op; `vault_checkpoint_a_false_statement_reads_alike_in_every_log`
/// and `vault_checkpoint_a_dropped_delete_stands_for_its_rep` hold the first two.
#[test]
fn vault_checkpoint_pruning_preserves_the_evaluated_state_over_random_dags() {
    let mut coverage = Coverage::default();
    for seed in 1..=SEEDS {
        let mut world = random_vault(seed, 100, 30);
        let Some(plan) = propose(&world.whole(), NOW) else {
            continue;
        };
        world.settle(&plan.body);
        let whole = world.whole();
        let settled = |log: &Log| {
            checkpoint::settled(log)
                .iter()
                .any(|p| Some(p.id) == checkpoint_id(&plan.body))
        };
        if !settled(&whole) {
            // No seat was heard in the month before the cut, so no share makes it final: no Mac
            // prunes there.
            assert!(
                checkpoint::judge(&whole, &plan.body)
                    .unwrap()
                    .recent
                    .is_empty()
            );
            continue;
        }
        let rooted = whole.rooted_at(&plan.body).unwrap();
        coverage.pruned += 1;
        coverage.dropped += plan.dropped.len();
        coverage.v1_dropped += plan
            .dropped
            .iter()
            .filter(|id| whole.get(id).unwrap().version() == 1)
            .count();
        coverage.removals += usize::from(world.removed);
        let heads = heads_of(&plan.body);
        let beside_cut = |record: &Record| !heads.iter().all(|h| whole.reaches(&[*record.id()], h));
        for record in rooted.records() {
            match record.body() {
                Body::Remove { member, cutoff } if rooted.is_dropped(cutoff) => {
                    if *member == A.id() {
                        coverage.undo_at_a_dropped_cutoff += 1;
                    } else if world.sleeper.is_some() && record.actor() == &B.id() {
                        coverage.sleeper_remove_at_a_dropped_cutoff += 1;
                    }
                }
                Body::Put { .. } if beside_cut(record) => {
                    if record
                        .statement()
                        .and_then(|s| s.carries)
                        .is_some_and(|c| rooted.is_dropped(&c))
                    {
                        coverage.stated_carries_of_dropped += 1;
                    }
                }
                Body::Delete { .. } if beside_cut(record) => {
                    if record
                        .statement()
                        .is_some_and(|s| s.supersedes.iter().any(|x| rooted.is_dropped(x)))
                    {
                        coverage.stated_deletes_over_dropped += 1;
                    }
                }
                _ => {}
            }
        }
        for record in whole.records() {
            if world.lies.contains_key(record.id()) {
                coverage.false_statements += 1;
                coverage.false_statements_beside_the_cut += usize::from(beside_cut(record));
            }
        }
        let rooting = rooted.rooting().unwrap();
        for (id, stub) in &rooting.stubs {
            assert!(
                stub.bytes() < whole.get(id).unwrap().wire_len(),
                "seed {seed}: a stub takes fewer bytes than its record"
            );
            coverage.stubs_smaller_than_records += 1;
            coverage.dropped_delete_reps +=
                usize::from(stub.kind == Kind::Delete && stub.carries.is_some());
        }
        coverage.keyed_with_a_dropped_dependent += whole
            .keyed_epochs()
            .iter()
            .filter(|k| k.dependents.iter().any(|d| rooted.is_dropped(d)))
            .count();
        let cut = whole.past_of(&heads);
        for record in rooted.records() {
            if record.parents().iter().any(|p| rooted.is_dropped(p)) {
                if cut.holds(record.id()) {
                    coverage.kept_after_a_dropped_parent += 1;
                } else if !heads.iter().all(|h| whole.reaches(&[*record.id()], h)) {
                    coverage.concurrent_after_a_dropped_parent += 1;
                }
            }
        }
        let dropped: BTreeSet<RecordId> = plan.dropped.iter().copied().collect();
        let expected: BTreeMap<RecordId, BTreeSet<RecordId>> = shape(&whole)
            .into_iter()
            .filter(|(id, _)| !dropped.contains(id))
            .map(|(id, past)| (id, past.difference(&dropped).copied().collect()))
            .collect();
        assert_eq!(
            shape(&rooted),
            expected,
            "seed {seed}: each record's past is its whole past less the dropped records"
        );
        assert_evaluates_as(seed, "first", &rooted, &whole);
        let Body::Checkpoint { cut_ms, .. } = plan.body else {
            panic!("a checkpoint");
        };
        assert_eq!(
            checkpoint_id(&plan_at(&rooted, cut_ms, &heads).unwrap().body),
            checkpoint_id(&plan.body),
            "seed {seed}: the rooted log computes its own root as the whole log does"
        );
        assert_eq!(rooted.held_count() + plan.dropped.len(), whole.held_count());
        let (r, w) = (rooted.counts(), whole.counts());
        assert!(r.held() <= w.held() && r.keyed <= w.keyed && r.weighed == w.weighed);

        // The vault runs on past the cut; the rooted Mac takes the new records.
        world.run(seed, 150);
        let whole = world.whole();
        let mut rooted = rooted;
        rooted.merge(&whole).unwrap();
        assert_evaluates_as(seed, "after the cut", &rooted, &whole);
        assert!(
            settled(&whole) && settled(&rooted),
            "seed {seed}: both logs hold the root settled"
        );
        coverage.settled_not_final +=
            usize::from(!checkpoint::judge(&whole, &plan.body).unwrap().is_final);
        let now = world.clock + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
        let next = propose(&whole, now);
        assert_eq!(
            propose(&rooted, now).map(|plan| checkpoint_id(&plan.body)),
            next.as_ref().map(|plan| checkpoint_id(&plan.body)),
            "seed {seed}: the rooted log proposes the whole log's next checkpoint"
        );
        let Some(next) = next else {
            continue;
        };
        world.settle(&next.body);
        let whole = world.whole();
        if !checkpoint::settled(&whole)
            .iter()
            .any(|p| Some(p.id) == checkpoint_id(&next.body))
        {
            assert!(
                checkpoint::judge(&whole, &next.body)
                    .unwrap()
                    .recent
                    .is_empty()
            );
            continue;
        }
        coverage.next_proposals += 1;
        let mut rooted = rooted;
        rooted.merge(&whole).unwrap();
        let again = rooted
            .rooted_at(&next.body)
            .unwrap_or_else(|e| panic!("seed {seed}: the rooted log prunes at it again: {e}"));
        coverage.second_prunes += 1;
        let next_heads = heads_of(&next.body);
        coverage.kept_stubs += rooted
            .stubs_in_past(&next_heads)
            .iter()
            .filter(|id| !next.dropped.contains(id))
            .count();
        let once = whole.rooted_at(&next.body).unwrap();
        let settled_ids = |log: &Log| -> Vec<RecordId> {
            checkpoint::settled(log).iter().map(|s| s.id).collect()
        };
        assert_eq!(
            settled_ids(&again),
            settled_ids(&whole),
            "seed {seed}: the log pruned twice holds settled what the whole log does"
        );
        assert_evaluates_as(seed, "pruned twice", &again, &whole);
        assert_evaluates_as(seed, "pruned once there", &once, &whole);
        assert_eq!(
            again.rooting().unwrap().roots.len(),
            1,
            "seed {seed}: the second root takes the place of the first, whose cut it nests"
        );
    }
    assert!(
        coverage.pruned * 4 >= usize::try_from(SEEDS).unwrap() * 3,
        "{coverage:?}"
    );
    assert!(
        coverage.dropped > 0 && coverage.v1_dropped > 0,
        "{coverage:?}"
    );
    assert!(coverage.kept_after_a_dropped_parent > 0, "{coverage:?}");
    assert!(
        coverage.concurrent_after_a_dropped_parent > 0,
        "{coverage:?}"
    );
    assert!(coverage.removals > 0, "{coverage:?}");
    assert!(coverage.undo_at_a_dropped_cutoff > 0, "{coverage:?}");
    assert!(
        coverage.sleeper_remove_at_a_dropped_cutoff > 0,
        "{coverage:?}"
    );
    assert!(coverage.stated_carries_of_dropped > 0, "{coverage:?}");
    assert!(coverage.settled_not_final > 0, "{coverage:?}");
    assert!(coverage.false_statements_beside_the_cut > 0, "{coverage:?}");
    assert!(coverage.stated_deletes_over_dropped > 0, "{coverage:?}");
    assert!(coverage.stubs_smaller_than_records > 0, "{coverage:?}");
    assert!(
        coverage.next_proposals * 2 >= coverage.pruned
            && coverage.second_prunes == coverage.next_proposals,
        "{coverage:?}"
    );
    eprintln!("{coverage:?}");
}

fn heads_of(body: &Body) -> Vec<RecordId> {
    match body {
        Body::Checkpoint { heads, .. } => heads.clone(),
        _ => panic!("a checkpoint"),
    }
}

/// What two rooted logs hold, as a merge must make them equal: their records and the order over
/// them, their dropped ids, their roots and their evaluation.
fn same_log(left: &Log, right: &Log) -> bool {
    let stubs = |log: &Log| log.rooting().map(|r| r.stubs.clone());
    let roots = |log: &Log| {
        log.rooting()
            .map(|r| r.roots.iter().map(|root| root.id).collect::<Vec<_>>())
    };
    shape(left) == shape(right)
        && stubs(left) == stubs(right)
        && roots(left) == roots(right)
        && left.counts() == right.counts()
        && evaluated(left) == evaluated(right)
}

/// A Mac that slept across the cut: C saw A's first `Put` of `a.key` and wrote on it after the
/// cut's day, when A's version-2 `Put` had superseded that first one and the prune drops it. `L1`
/// lacks C's record and `L2` holds it; both hold the cut.
pub(in crate::membership_log) struct Sleeper {
    pub(in crate::membership_log) l1: Log,
    pub(in crate::membership_log) l2: Log,
    pub(in crate::membership_log) body: Body,
    pub(in crate::membership_log) slept: RecordId,
    pub(in crate::membership_log) dropped: RecordId,
}

pub(in crate::membership_log) fn sleeper() -> Sleeper {
    let mut log = found(&[A, B, C]);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "a.key", value),
        )
        .unwrap()
    };
    let p1 = one(put(&log, A, t0 + HOUR, b"one"), A);
    let p1 = hold(&mut log, p1);
    let b1 = one(draft(&log, B, t0 + 2 * HOUR, name("b1")), B);
    hold(&mut log, b1);
    let p2 = two(
        put(&log, A, t0 + 3 * HOUR, b"two"),
        A,
        Some(&supersedes(&[&p1], None)),
    );
    let p2 = hold(&mut log, p2);
    let b2 = one(draft(&log, B, t0 + 4 * HOUR, name("b2")), B);
    hold(&mut log, b2);
    let l1 = log.clone();
    // C, silent in the month before the cut, writes on what it saw: A's first `Put`.
    let slept = one(
        draft_after(&log, C, T + DAY_MS, &[*p1.id()], name("slept")),
        C,
    );
    let slept = hold(&mut log, slept);
    let plan = propose(&log, NOW).expect("a proposal");
    assert_eq!(plan.dropped, vec![*p1.id()]);
    assert!(
        !log.reaches(&[*slept.id()], p2.id()),
        "C's record is concurrent with the cut"
    );
    Sleeper {
        l1,
        l2: log,
        body: plan.body,
        slept: *slept.id(),
        dropped: *p1.id(),
    }
}

/// A prune of the union of two logs is the union of their prunes: over seeded random vaults
/// (each of two Macs' logs merged with the third's, so each holds the cut and records the other
/// lacks), and for a Mac that slept across the cut, whose record follows a dropped one and not
/// the cut, which the rooted log places by the dropped record's stub.
///
/// NEGATIVE CONTROL: `Mutant::RefuseConcurrent` (M7 over a dropped-id set alone: outside a prune
/// insert refuses a record with a dropped parent that does not follow the cut, while the prune
/// keeps what the whole log held), run from the test's first line and seen to fail with
/// `assertion failed: union_of_prunes.get(&slept.slept).is_some()`: the prune of the union holds
/// C's record and the union of the prunes refused it.
#[test]
fn vault_checkpoint_prune_of_a_union_equals_the_union_of_prunes() {
    let slept = sleeper();
    let prune_of_union = slept.l2.rooted_at(&slept.body).unwrap();
    let mut union_of_prunes = slept.l1.rooted_at(&slept.body).unwrap();
    union_of_prunes.merge(&prune_of_union).unwrap();
    assert!(union_of_prunes.get(&slept.slept).is_some());
    assert!(union_of_prunes.is_dropped(&slept.dropped));
    assert!(same_log(&union_of_prunes, &prune_of_union));
    // The other way round, and by the records alone.
    let mut other = prune_of_union.clone();
    other
        .merge(&slept.l1.rooted_at(&slept.body).unwrap())
        .unwrap();
    assert!(same_log(&other, &prune_of_union));
    let mut by_records = slept.l1.rooted_at(&slept.body).unwrap();
    for record in slept.l2.records() {
        let _ = by_records.insert(record.clone());
    }
    assert!(same_log(&by_records, &prune_of_union));

    let mut checked = 0;
    for seed in 1..=SEEDS {
        let world = random_vault(seed, 100, 30);
        let mut l1 = world.logs[0].clone();
        l1.merge(&world.logs[1]).unwrap();
        let mut l2 = world.logs[2].clone();
        l2.merge(&world.logs[1]).unwrap();
        let mut union = l1.clone();
        union.merge(&l2).unwrap();
        let Some(plan) = propose(&union, NOW) else {
            continue;
        };
        let heads = heads_of(&plan.body);
        if !heads
            .iter()
            .all(|h| l1.get(h).is_some() && l2.get(h).is_some())
        {
            continue;
        }
        let (Ok(p1), Ok(p2)) = (l1.rooted_at(&plan.body), l2.rooted_at(&plan.body)) else {
            continue;
        };
        let mut merged = p1;
        merged.merge(&p2).unwrap();
        assert!(
            same_log(&merged, &union.rooted_at(&plan.body).unwrap()),
            "seed {seed}: the union of the prunes is the prune of the union"
        );
        checked += 1;
    }
    assert!(checked > 0, "some seed has a cut both logs hold");

    MUTANT.set(Some(Mutant::RefuseConcurrent));
    let prune_of_union = slept.l2.rooted_at(&slept.body).unwrap();
    let mut union_of_prunes = slept.l1.rooted_at(&slept.body).unwrap();
    union_of_prunes.merge(&prune_of_union).unwrap();
    MUTANT.set(None);
    assert!(
        !same_log(&union_of_prunes, &prune_of_union),
        "RefuseConcurrent loses the sleeper's record from one side of the union"
    );
}

/// A store whose log is `log`.
pub(in crate::membership_log) fn store_of(log: Log) -> Store {
    let mut store = Store::with_compiled(None);
    store.insert(log.genesis().clone()).unwrap();
    store.log = Some(log);
    store
}

/// A pruned Mac takes no dropped record back: not by a merge of the whole log, not by an insert
/// of the record itself, and not by a store ingesting the whole log's records file. A store holds
/// and contains each dropped id, so it neither asks a peer for one nor is sent one past a `have`
/// that names it. A record that follows a dropped one is placed and held.
///
/// NEGATIVE CONTROL: `Mutant::TakeBack` (insert takes a dropped record that arrives again), run
/// from the test's first line and seen to fail with ``assertion `left == right` failed``,
/// `left: Held` against `right: Known`, at the insert of the dropped record.
#[test]
fn vault_checkpoint_a_pruned_mac_takes_no_dropped_record_back() {
    let slept = sleeper();
    let whole = slept.l2.clone();
    let dropped = whole.get(&slept.dropped).unwrap().clone();
    let mut pruned = slept.l1.rooted_at(&slept.body).unwrap();
    assert_eq!(pruned.insert(dropped.clone()).unwrap(), Inserted::Known);
    pruned.merge(&whole).unwrap();
    assert!(pruned.get(&slept.dropped).is_none());
    assert!(pruned.knows(&slept.dropped) && pruned.is_dropped(&slept.dropped));
    assert!(
        pruned.get(&slept.slept).is_some(),
        "the record after a dropped one is held"
    );
    assert_eq!(pruned.held_count() + 1, whole.held_count());

    let mut store = store_of(slept.l1.rooted_at(&slept.body).unwrap());
    // The whole log's records, as a peer's reply brings them (decoded at format two).
    store
        .ingest_records(whole.records().cloned().collect())
        .unwrap();
    assert!(store.record(&slept.dropped).is_none());
    assert!(store.holds(&slept.dropped) && store.contains(&slept.dropped));
    assert!(store.inventory().contains(&slept.dropped));
    assert!(!store.insert(dropped.clone()).unwrap());
    assert!(store.holds(&slept.slept));
    // A whole peer whose `have` names the dropped record is not sent what it follows.
    let sent = store.missing(&[slept.dropped]);
    assert!(
        sent.iter()
            .all(|r| !whole.reaches(&[slept.dropped], r.id()))
    );

    MUTANT.set(Some(Mutant::TakeBack));
    let mut taken = slept.l1.rooted_at(&slept.body).unwrap();
    taken.merge(&whole).unwrap();
    MUTANT.set(None);
    assert!(
        taken.get(&slept.dropped).is_some(),
        "TakeBack holds a dropped record"
    );
}

/// A record whose `previous` was dropped is checked against the stub: the next sequence of the
/// same signer is held, as the whole log holds it, and placed where the dropped record stood;
/// another signer is refused. A draft follows a signer's newest record when a prune dropped it,
/// and so never takes its sequence again. A `Remove` whose cutoff was dropped is held where the
/// cutoff is the removed Mac's record in its past, and refused where it is not in its past.
#[test]
fn vault_checkpoint_a_rooted_log_checks_a_record_against_a_dropped_one() {
    let mut log = found(&[A, B]);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B]);
    let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "k", value),
        )
        .unwrap()
    };
    let record = one(put(&log, B, t0 + 1, b"one"), B);
    let b1 = hold(&mut log, record);
    let record = one(draft(&log, A, t0 + 2, name("a")), A);
    let a1 = hold(&mut log, record);
    let b2 = two(
        put(&log, B, t0 + 3, b"two"),
        B,
        Some(&supersedes(&[&b1], None)),
    );
    let b2 = hold(&mut log, b2);
    let plan = plan_at(&log, T, &log.heads()).unwrap();
    assert_eq!(plan.dropped, vec![*b1.id()]);
    let rooted = log.rooted_at(&plan.body).unwrap();
    assert!(rooted.is_dropped(b1.id()) && rooted.get(b2.id()).is_some());
    let dropped: BTreeSet<RecordId> = plan.dropped.iter().copied().collect();
    let less = |log: &Log, id: &RecordId| -> BTreeSet<RecordId> {
        shape(log)[id].difference(&dropped).copied().collect()
    };

    // B's next record follows b2; one that names b1 as its previous at b2's sequence forks, and
    // is checked against b1's stub as the whole log checks it against b1.
    let next = one(draft(&rooted, B, T + HOUR, name("next")), B);
    assert_eq!(next.sequence(), b2.sequence() + 1);
    let after_b1 = |by: Mac, body: Body| {
        let mut fork = draft_after(&log, by, T + HOUR, &[], body);
        fork.previous = Some(*b1.id());
        fork.sequence = b1.sequence() + 1;
        fork.parents = vec![*b1.id()];
        one(fork, by)
    };
    let fork = after_b1(B, name("fork"));
    let mut whole = log.clone();
    assert_eq!(whole.insert(fork.clone()).unwrap(), Inserted::Held);
    let mut placed = rooted.clone();
    assert_eq!(placed.insert(fork.clone()).unwrap(), Inserted::Held);
    assert_eq!(less(&placed, fork.id()), less(&whole, fork.id()));
    assert!(
        !placed.reaches(&[*fork.id()], a1.id()),
        "the fork follows only b1's place"
    );
    let wrong = after_b1(A, name("wrong"));
    assert!(log.clone().insert(wrong.clone()).is_err());
    assert!(rooted.clone().insert(wrong).is_err());

    // A removes B at b1, which it saw, and at b1 from a record that follows only its own.
    let at_b1 = Body::Remove {
        member: B.id(),
        cutoff: *b1.id(),
    };
    let remove = one(draft(&log, A, T + HOUR, at_b1.clone()), A);
    assert_eq!(log.clone().insert(remove.clone()).unwrap(), Inserted::Held);
    let mut removing = rooted.clone();
    assert_eq!(removing.insert(remove).unwrap(), Inserted::Held);
    let blind = one(draft_after(&log, A, T + HOUR, &[*a1.id()], at_b1), A);
    let mut blind_whole = log.clone();
    let mut blind_rooted = rooted;
    if log.reaches(&[*a1.id()], b1.id()) {
        assert!(blind_whole.insert(blind.clone()).is_ok());
        assert!(blind_rooted.insert(blind).is_ok());
    } else {
        assert!(blind_whole.insert(blind.clone()).is_err());
        assert!(blind_rooted.insert(blind).is_err());
    }
}

/// The common past of a split and the epoch both sides write under: A's first `Put` of `a.key`,
/// `a0`, and a `Name` of B's.
pub(in crate::membership_log) struct Common {
    pub(in crate::membership_log) log: Log,
    pub(in crate::membership_log) a0: Record,
    pub(in crate::membership_log) put: Box<dyn Fn(&Log, Mac, u64, &str, &[u8]) -> Draft>,
}

pub(in crate::membership_log) fn common() -> Common {
    let mut log = found(&[A, B, C]);
    let (e, key) = epoch(
        &mut log,
        A,
        D - 5 * HOUR,
        EpochAudience::Everyone,
        1,
        &[A, B, C],
    );
    let put = move |log: &Log, by: Mac, at: u64, name: &str, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, name, value),
        )
        .unwrap()
    };
    let record = one(draft(&log, B, D - 4 * HOUR, name("b0")), B);
    hold(&mut log, record);
    let record = one(put(&log, A, D - 3 * HOUR, "a.key", b"a0"), A);
    let a0 = hold(&mut log, record);
    Common {
        log,
        a0,
        put: Box::new(put),
    }
}

/// B's side of a split a month on: `b2`, then `b3` superseding it, then a `Name` after its cut's
/// day; B's checkpoint at that `Name` (it drops `b2`), shared by B.
pub(in crate::membership_log) struct SideB {
    pub(in crate::membership_log) log: Log,
    pub(in crate::membership_log) b2: Record,
    pub(in crate::membership_log) b3: Record,
    pub(in crate::membership_log) body: Body,
}

pub(in crate::membership_log) fn side_b(common: &Common) -> SideB {
    let put = &common.put;
    let mut log = common.log.clone();
    let record = one(put(&log, B, SPLIT - 2 * DAY_MS, "b.key", b"b2"), B);
    let b2 = hold(&mut log, record);
    assert!(b2.parents().contains(common.a0.id()));
    let b3 = two(
        put(&log, B, SPLIT - DAY_MS, "b.key", b"b3"),
        B,
        Some(&supersedes(&[&b2], None)),
    );
    let b3 = hold(&mut log, b3);
    let record = one(draft(&log, B, SPLIT + HOUR, name("b4")), B);
    let b4 = hold(&mut log, record);
    let cut = plan_at(&log, SPLIT + DAY_MS, &[*b4.id()]).unwrap();
    assert_eq!(cut.dropped, vec![*b2.id()]);
    share_at(&mut log, B, &cut.body, SPLIT + 2 * DAY_MS);
    SideB {
        log,
        b2,
        b3,
        body: cut.body,
    }
}

/// A split of more than 14 days after a quiet month: A alone is recent at its side's cut and B
/// at its, each side's checkpoint final, and neither cut holds the other's head. A merge takes no
/// root from the other side: a Mac pruned at A's cut that merges a log pruned at B's takes B's
/// records it can place and keeps those that follow `b2`, which B's prune dropped and it lacks,
/// waiting; once a log that holds B's side whole serves it, it holds every record of B's side
/// the whole log holds, places `b2` (which follows `a0`, which A's prune dropped) by `a0`'s stub,
/// takes back neither `a0` nor anything else either side dropped, and reads what the whole log
/// reads. It computes and adopts B's checkpoint, whose cut keeps `a0`, which it dropped and keeps
/// as a stub. Where neither side dropped a record of the common past, it adopts both too, and has
/// no newest root until it adopts a checkpoint whose cut nests both, which it computes as the
/// whole log does (its stubs in the cut are dropped again).
///
/// NEGATIVE CONTROL: `Mutant::RefuseConcurrent` (M7 over a dropped-id set alone), run from the
/// test's first line and seen to fail with ``assertion `left == right` failed: the pruned Mac holds
/// every record of the other side the whole log holds``: `b2` follows `a0`, which A's prune
/// dropped, and not A's cut, so it and every record after it are refused. And `plan_of` with the
/// stubs in the cut left out of `dropped`, seen to fail with ``assertion `left == right` failed:
/// a rooted log computes the nesting checkpoint as the whole log does``.
#[test]
fn vault_checkpoint_two_pruned_sides_of_a_split_merge_with_no_newest_root() {
    let common = common();
    let put = &common.put;
    let a0 = &common.a0;
    // A's side supersedes `a0`, so its prune drops a record of the common past.
    let mut side_a = common.log.clone();
    let a1 = two(
        put(&side_a, A, SPLIT - DAY_MS, "a.key", b"a1"),
        A,
        Some(&supersedes(&[a0], None)),
    );
    let a1 = hold(&mut side_a, a1);
    let cut_a = plan_at(&side_a, SPLIT, &[*a1.id()]).unwrap();
    assert_eq!(cut_a.dropped, vec![*a0.id()]);
    share_at(&mut side_a, A, &cut_a.body, SPLIT + 2 * DAY_MS);
    let side = side_b(&common);
    let (b2, b3) = (&side.b2, &side.b3);
    let mut x = side_a.adopt(&cut_a.body).unwrap();
    let y = side.log.adopt(&side.body).unwrap();
    assert_eq!(x.newest_root().map(|r| r.id), checkpoint_id(&cut_a.body));

    x.merge(&y).unwrap();
    assert_eq!(x.rooting().unwrap().roots.len(), 1, "a merge takes no root");
    assert!(!x.knows(b2.id()) && x.get(b3.id()).is_none());
    assert!(x.waiting.contains_key(b3.id()), "b3 waits for b2");
    x.merge(&side.log).unwrap();
    let mut whole = side_a.clone();
    whole.merge(&side.log).unwrap();
    assert!(x.get(a0.id()).is_none() && x.is_dropped(a0.id()));
    let held_by = |log: &Log| -> BTreeSet<RecordId> { log.records().map(|r| *r.id()).collect() };
    let mut expected = held_by(&whole);
    expected.remove(a0.id());
    assert_eq!(
        held_by(&x),
        expected,
        "the pruned Mac holds every record of the other side the whole log holds"
    );
    assert!(x.get(b2.id()).is_some() && x.get(b3.id()).is_some());
    assert_eq!(
        evaluated(&x),
        evaluated(&whole),
        "both sides' values read as the whole log's"
    );
    // B's cut keeps `a0`, which this log dropped: it computes B's checkpoint as the whole log
    // does, reading `a0` at its stub, and adopting it keeps that stub.
    let both = x.adopt(&side.body).unwrap();
    assert_eq!(both.rooting().unwrap().roots.len(), 2);
    assert!(both.get(a0.id()).is_none() && both.is_dropped(a0.id()));
    assert_eq!(evaluated(&both), evaluated(&whole));

    // Neither side drops a record of the common past: A's supersedes its own first `Put` after
    // the split.
    let mut side_a = common.log.clone();
    let record = one(put(&side_a, A, SPLIT - 2 * DAY_MS, "a.key", b"a1"), A);
    let a1 = hold(&mut side_a, record);
    let a2 = two(
        put(&side_a, A, SPLIT - DAY_MS, "a.key", b"a2"),
        A,
        Some(&supersedes(&[&a1], None)),
    );
    let a2 = hold(&mut side_a, a2);
    let cut_a = plan_at(&side_a, SPLIT, &[*a2.id()]).unwrap();
    assert_eq!(cut_a.dropped, vec![*a1.id()]);
    share_at(&mut side_a, A, &cut_a.body, SPLIT + 2 * DAY_MS);
    let mut x = side_a.adopt(&cut_a.body).unwrap();
    x.merge(&side.log).unwrap();
    let mut x = x.adopt(&side.body).unwrap();
    assert_eq!(x.rooting().unwrap().roots.len(), 2);
    assert!(
        x.newest_root().is_none(),
        "neither side's cut holds the other's"
    );
    let mut whole = side_a.clone();
    whole.merge(&side.log).unwrap();
    x.merge(&whole).unwrap();
    for id in [a1.id(), b2.id()] {
        assert!(
            x.get(id).is_none() && x.is_dropped(id),
            "no dropped record comes back"
        );
    }
    assert_eq!(evaluated(&x), evaluated(&whole));

    // A cut that holds both sides' heads nests both: this log computes it as the whole log does,
    // and it is the newest once adopted.
    let tips = whole.heads();
    let both = one(
        draft_after(&whole, C, SPLIT + 3 * DAY_MS, &tips, name("both")),
        C,
    );
    hold(&mut whole, both.clone());
    assert_eq!(x.insert(both.clone()).unwrap(), Inserted::Held);
    let nest = plan_at(&x, SPLIT + 4 * DAY_MS, &[*both.id()]).unwrap();
    assert_eq!(
        checkpoint_id(&nest.body),
        checkpoint_id(
            &plan_at(&whole, SPLIT + 4 * DAY_MS, &[*both.id()])
                .unwrap()
                .body
        ),
        "a rooted log computes the nesting checkpoint as the whole log does"
    );
    let nested = x.rooted_at(&nest.body).unwrap();
    assert_eq!(
        nested.newest_root().map(|r| r.id),
        checkpoint_id(&nest.body)
    );
    for id in [a1.id(), b2.id()] {
        assert!(nested.get(id).is_none() && nested.is_dropped(id));
    }
    assert_eq!(evaluated(&nested), evaluated(&whole));

    MUTANT.set(Some(Mutant::RefuseConcurrent));
    let common_a = {
        let mut log = common.log.clone();
        let a1 = two(
            put(&log, A, SPLIT - DAY_MS, "a.key", b"a1"),
            A,
            Some(&supersedes(&[a0], None)),
        );
        let a1 = hold(&mut log, a1);
        let cut = plan_at(&log, SPLIT, &[*a1.id()]).unwrap();
        share_at(&mut log, A, &cut.body, SPLIT + 2 * DAY_MS);
        log.adopt(&cut.body).unwrap()
    };
    let mut refused = common_a;
    refused.merge(&side.log).unwrap();
    MUTANT.set(None);
    assert!(
        refused.get(b2.id()).is_none(),
        "RefuseConcurrent refuses B's side"
    );
}

/// A stub's place resolves in one walk over the stubs, each taken once: a lattice of 20 levels
/// of two stubs, each placed at both stubs of the level below, resolves to the genesis alone, and
/// stubs whose places name each other resolve to nothing (a record that follows one waits).
///
/// NEGATIVE CONTROL: the recursive `placed` this replaced (each place resolved afresh, with no
/// set of what it took), seen to fail with ``assertion `left == right` failed: the lattice
/// resolves to the genesis once`` (it resolves the genesis once per path, 2^20 times), and, with
/// the cycle asked first, to abort the test binary with ``has overflowed its stack``.
#[test]
fn vault_checkpoint_a_stub_place_resolves_in_one_walk() {
    let mut log = found(&[A]);
    let genesis = *log.genesis().id();
    let id = |level: u8, k: u8| -> RecordId {
        let mut id = [0xAB; 32];
        id[0] = level;
        id[1] = k;
        id
    };
    let stub = |parents: Vec<RecordId>| Stub {
        actor: A.id(),
        sequence: 1,
        previous: None,
        authored_ms: T,
        kind: Kind::Name,
        carries: None,
        epoch: None,
        supersedes: Vec::new(),
        shares: None,
        parents,
        frame: None,
    };
    let levels = 20u8;
    let mut stubs = BTreeMap::new();
    for level in 0..levels {
        let below = if level + 1 == levels {
            vec![genesis]
        } else {
            vec![id(level + 1, 0), id(level + 1, 1)]
        };
        for k in 0..2 {
            stubs.insert(id(level, k), stub(below.clone()));
        }
    }
    let (x, y) = ([0xC0; 32], [0xC1; 32]);
    stubs.insert(x, stub(vec![y]));
    stubs.insert(y, stub(vec![x]));
    log.rooting = Some(Arc::new(Rooting {
        roots: Vec::new(),
        stubs,
        earlier: Vec::new(),
    }));
    assert_eq!(
        log.placed(&id(0, 0)),
        Some(vec![0]),
        "the lattice resolves to the genesis once"
    );
    assert_eq!(log.placed(&x), None, "a cycle of stubs resolves to nothing");
}

/// A log adopts only a checkpoint final in it, and only one it computes itself.
#[test]
fn vault_checkpoint_a_log_adopts_only_a_final_checkpoint_it_computes() {
    let slept = sleeper();
    let mut log = slept.l2.clone();
    assert!(log.adopt(&slept.body).is_err(), "no shares yet");
    let at = T + MIN_AGE_MS + HOUR;
    share_at(&mut log, A, &slept.body, at);
    share_at(&mut log, B, &slept.body, at);
    let adopted = log.adopt(&slept.body).unwrap();
    assert!(adopted.is_dropped(&slept.dropped));
    assert_eq!(
        adopted.newest_root().map(|r| r.id),
        checkpoint_id(&slept.body)
    );
    let Body::Checkpoint {
        predecessor,
        cut_ms,
        heads,
        dropped,
        ..
    } = slept.body.clone()
    else {
        panic!("a checkpoint");
    };
    let forged = Body::Checkpoint {
        predecessor,
        cut_ms,
        heads,
        state: [0x77; 32],
        dropped,
    };
    assert!(
        log.rooted_at(&forged).is_err(),
        "a body this log computes otherwise"
    );
}

/// Hold the record `$record` makes from `$log` in `$log`, and hand it back.
macro_rules! held {
    ($log:ident, $record:expr $(,)?) => {{
        let record = $record;
        hold(&mut $log, record)
    }};
}

/// What the membership evaluators read of a log: its seats, removals and quorums, and the
/// cutoff an undo of each Mac signs.
fn membership_of(log: &Log) -> String {
    let membership = log.membership();
    let mut out = format!(
        "seats {:?}\nout {:?}\nquorums {:?}\n",
        membership.members().keys().collect::<Vec<_>>(),
        membership.removed(),
        membership.quorums(),
    );
    for mac in [A, B, C] {
        out.push_str(&format!(
            "{} undo {:?}\n",
            label(mac),
            log.undo_cutoff(&mac.id())
        ));
    }
    out
}

/// A vault whose records after the cut name a dropped record as a `Remove`'s cutoff. A, alone
/// heard in the month before the cut, writes `a.key`, removes C (B and C silent, so nobody
/// acknowledges it) and writes `a.key` again, superseding its first `Put`: the cut is A's, and
/// the prune drops that first `Put`, the cutoff an undo of A's removal of C signs. After the
/// cut's day B signs that undo's `Remove` of A at it (an undo share), and C, waking on the log
/// it last took before A removed it, removes A at the same `Put`, the newest record of A's it
/// saw (a sleeper's `Remove`).
struct DroppedCutoff {
    whole: Log,
    plan: checkpoint::Plan,
    /// A's first `Put`: the cutoff of both `Remove`s, which the prune drops.
    cutoff: RecordId,
}

fn dropped_cutoff() -> DroppedCutoff {
    let mut log = found(&[A, B, C]);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, at: u64, value: &[u8]| {
        log.draft_put(
            &A.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "a.key", value),
        )
        .unwrap()
    };
    let first = held!(log, one(put(&log, t0 + HOUR, b"one"), A));
    let stale = log.clone();
    let remove = |log: &Log, by: Mac, at: u64, member: Mac, cutoff: RecordId| {
        one(
            draft(
                log,
                by,
                at,
                Body::Remove {
                    member: member.id(),
                    cutoff,
                },
            ),
            by,
        )
    };
    let network = *log.network();
    held!(log, remove(&log, A, t0 + 2 * HOUR, C, network));
    held!(
        log,
        two(
            put(&log, t0 + 3 * HOUR, b"two"),
            A,
            Some(&supersedes(&[&first], None))
        )
    );
    assert_eq!(
        log.undo_cutoff(&A.id()),
        Some(*first.id()),
        "an undo of A's removal of C signs A's first `Put`"
    );
    held!(log, remove(&log, B, T + DAY_MS, A, *first.id()));
    hold(
        &mut log,
        remove(&stale, C, T + DAY_MS + HOUR, A, *first.id()),
    );
    let plan = propose(&log, NOW).expect("a proposal");
    assert_eq!(plan.dropped, vec![*first.id()]);
    DroppedCutoff {
        whole: log,
        plan,
        cutoff: *first.id(),
    }
}

/// A `Remove` whose cutoff a prune dropped counts in the rooted log as in the whole log: an undo
/// share (B's) and a sleeper's `Remove` (C's) of A, each at A's `Put` the prune dropped, take A
/// out by quorum with that cutoff in both, and an undo of A signs the dropped `Put` in both (every
/// seat's removal, quorum and undo cutoff alike). The
/// evaluators read the cutoff at its stub's index past every held record's, and a held record
/// lies in its past where it lies in the past of the stub's place.
///
/// NEGATIVE CONTROL: `Mutant::HeldCutoffs` (the evaluators read held records alone, as slice 1
/// did: a `Remove` whose cutoff was dropped is no `Remove`, and an undo's cutoff comes from the
/// held records), run from the test's first line and seen to fail with ``assertion `left ==
/// right` failed: the removal of A counts as in the whole log``, `left: None` against `right:
/// Some(Removal { by: {..B, ..C}, quorum: true })`.
#[test]
fn vault_checkpoint_a_remove_whose_cutoff_was_dropped_counts() {
    let DroppedCutoff {
        whole,
        plan,
        cutoff,
    } = dropped_cutoff();
    let rooted = whole.rooted_at(&plan.body).unwrap();
    assert!(rooted.is_dropped(&cutoff));
    let removal = whole.membership().removed().get(&A.id()).cloned();
    assert_eq!(
        removal,
        Some(crate::membership_log::Removal {
            by: [B.id(), C.id()].into_iter().collect(),
            quorum: true,
        }),
        "the whole log counts both"
    );
    assert_eq!(
        rooted.membership().removed().get(&A.id()).cloned(),
        removal,
        "the removal of A counts as in the whole log"
    );
    let quorum = |log: &Log| {
        log.membership()
            .quorums()
            .iter()
            .map(|q| (q.member, q.cutoff, q.signers.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        quorum(&whole),
        vec![(A.id(), cutoff, [B.id(), C.id()].into_iter().collect())]
    );
    assert_eq!(quorum(&rooted), quorum(&whole));
    assert_eq!(rooted.undo_cutoff(&A.id()), Some(cutoff));
    assert_eq!(membership_of(&rooted), membership_of(&whole));
    // No seat shared this checkpoint, so no log holds it settled and nothing it dropped is gone
    // ([`checkpoint::gone`]): the quorum voids A's second `Put`, after the cutoff, and the whole
    // log reads A's first `Put` again, whose ciphertext the prune dropped, while the rooted log
    // reads no value of `a.key`, and never the void one. A Mac prunes only at a final, so
    // settled, checkpoint, where both read none
    // (`vault_checkpoint_a_remove_behind_a_final_cut_revives_nothing`).
    let values = |log: &Log| {
        log.open_entry_set(&B.identity()).unwrap().map(|opened| {
            opened
                .entries
                .iter()
                .map(|e| e.value.as_slice().to_vec())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(values(&whole), Some(vec![b"one".to_vec()]));
    assert_eq!(values(&rooted), Some(vec![]));
}

/// A vault where a Mac that slept across the cut carries a value whose ops the prune dropped:
/// A writes `a.key` (version 2), reseals it (a carry of that first `Put`), and writes it again
/// superseding both, which the cut holds: another value, so the prune drops both, or (`resealed`)
/// the same value carried on again, so the prune keeps the first `Put` that `Put` carries and
/// drops the reseal. C, silent in the month before the cut, wakes on the log it last took, which
/// held A's first two ops alone, and reseals the value, stating that it carries the reseal's
/// and replaces both.
/// Returns the whole log, its proposal, C's reseal and A's first `Put`.
pub(in crate::membership_log) fn carry_of_dropped(
    resealed: bool,
) -> (Log, checkpoint::Plan, RecordId, RecordId) {
    let mut log = found(&[A, B, C]);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "a.key", value),
        )
        .unwrap()
    };
    let first = held!(
        log,
        two(
            put(&log, A, t0 + HOUR, b"one"),
            A,
            Some(&Statement::default())
        )
    );
    let reseal = held!(
        log,
        two(
            put(&log, A, t0 + 2 * HOUR, b"one"),
            A,
            Some(&supersedes(&[], Some(&first)))
        )
    );
    let stale = log.clone();
    let (value, carries): (&[u8], _) = if resealed {
        (b"one", Some(&first))
    } else {
        (b"two", None)
    };
    held!(
        log,
        two(
            put(&log, A, t0 + 3 * HOUR, value),
            A,
            Some(&supersedes(&[&first, &reseal], carries))
        )
    );
    held!(log, one(draft(&log, B, t0 + 4 * HOUR, name("b1")), B));
    let carry = hold(
        &mut log,
        two(
            put(&stale, C, T + DAY_MS, b"one"),
            C,
            Some(&supersedes(&[&first, &reseal], Some(&reseal))),
        ),
    );
    let plan = propose(&log, NOW).expect("a proposal");
    let mut dropped = vec![*reseal.id()];
    if !resealed {
        dropped.push(*first.id());
    }
    dropped.sort_unstable();
    assert_eq!(plan.dropped, dropped);
    (log, plan, *carry.id(), *first.id())
}

/// What `mac`'s register reads each held carry among the siblings of `log` as standing for: the
/// op whose value it carries.
fn carried(log: &Log, mac: Mac) -> BTreeMap<RecordId, RecordId> {
    log.open_audience_entries(&mac.identity())
        .unwrap()
        .map(|opened| opened.carried)
        .unwrap_or_default()
        .into_iter()
        .filter(|(carry, _)| log.get(carry).is_some())
        .collect()
}

/// A carry beside the cut of an op the prune dropped reads as the whole log reads it: by its
/// statement, the frontier the held ops give it not being the one its writer read. Where A's
/// newer `Put` in the cut writes another value, C's reseal of the old one loses to it in both
/// logs (no conflict). Where A's newer `Put` carries the value on, C's reseal is its sibling and
/// stands, in both logs, for A's first `Put`, the semantic representative, though it names the
/// dropped reseal it read the value from: the rooted log reads that reseal's stub, whose
/// `carries` names the first `Put`.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::FrontierOnly` (the register reads C's reseal by the held ops of its frontier, as
///   slice 1 did: none where A's newer `Put` writes another value, so it writes a value of its
///   own), with ``assertion `left == right` failed: C's reseal reads as in the whole log
///   (resealed false)``, the left naming `a.key` a conflict (`{(Everyone, "a.key")}`) where the
///   right names none.
/// - `Mutant::StubCarriesUnread` (a statement naming a dropped carry stands for that carry), with
///   ``assertion `left == right` failed: the reseal stands for A's first `Put` (resealed
///   true)``: the left maps C's reseal to A's dropped reseal, the right to A's first `Put`.
#[test]
fn vault_checkpoint_a_carry_of_a_dropped_op_reads_its_statement() {
    for resealed in [false, true] {
        let (whole, plan, carry, first) = carry_of_dropped(resealed);
        let rooted = whole.rooted_at(&plan.body).unwrap();
        assert_eq!(
            evaluated(&rooted),
            evaluated(&whole),
            "C's reseal reads as in the whole log (resealed {resealed})"
        );
        for mac in [A, B, C] {
            if resealed {
                assert_eq!(carried(&whole, mac).get(&carry), Some(&first));
            }
            assert_eq!(
                carried(&rooted, mac),
                carried(&whole, mac),
                "the reseal stands for A's first `Put` (resealed {resealed})"
            );
        }
    }
}

/// A vault whose prune drops a record nothing follows. A and B write before day `D` and share
/// two final checkpoints, one at `D` (`old`) and one at `D` + 1 (`newer`, whose cut holds the
/// older's). B then falls silent, and A, the one seat heard in the month before day `P`, shares
/// `old` once more the day before it: its newest record, which no record follows. At `P` the
/// newest final checkpoint is `newer`; `old` is no predecessor of it and its cut nests in
/// `newer`'s, so the prune drops every share of `old`, A's last one with them.
fn dropped_head() -> (Log, Body, RecordId) {
    let mut log = found(&[A, B, C]);
    let a1 = held!(log, one(draft(&log, A, D - 3 * HOUR, name("a1")), A));
    let b1 = held!(
        log,
        one(
            draft_after(&log, B, D - 2 * HOUR, &[*a1.id()], name("b1")),
            B
        )
    );
    let a2 = held!(
        log,
        one(
            draft_after(&log, A, D + 2 * HOUR, &[*b1.id()], name("a2")),
            A
        )
    );
    let old = plan_at(&log, D, &[*b1.id()]).unwrap().body;
    let newer = plan_at(&log, D + DAY_MS, &[*a2.id()]).unwrap().body;
    let shared = D + 3 * DAY_MS;
    for (body, at) in [(&old, shared), (&newer, shared + HOUR)] {
        share_at(&mut log, A, body, at);
        share_at(&mut log, B, body, at);
    }
    let p = D + 40 * DAY_MS;
    share_at(&mut log, A, &old, p - DAY_MS);
    let last = *log.heads().iter().next().unwrap();
    assert_eq!(log.heads(), vec![last]);
    let plan = plan_at(&log, p, &[last]).unwrap();
    assert!(
        plan.dropped.contains(&last),
        "the prune drops A's last share"
    );
    (log, plan.body, last)
}

/// A dropped record nothing follows is a head of the rooted log as of the whole one, so the two
/// heads digests (what a Mac's frontier is compared by) are equal; a record that follows it ends
/// it as a head in both. A Mac drafting on the rooted log names it as a parent, as it would on
/// the whole log. The heads APIs read it as its record: it reaches each held record its record
/// does, a projection at the heads is the log with the same heads, and the next cut, which ends
/// at it, is proposed as the whole log proposes it.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::NoStubHeads` (a prune keeps no stub as a head, as slice 1 did: the dropped share's
///   held parents stand in its place): ``assertion `left == right` failed: the heads are the
///   whole log's``, `left` naming B's held share of `newer`, the parent of A's last share, where
///   `right` names that share.
/// - `Mutant::HeldIndexes` (the heads APIs resolve held heads alone, a dropped one to nothing):
///   ``assertion `left == right` failed: a dropped head reaches what its record does``, `left:
///   false`.
/// - `Mutant::HeldProposal` (a next checkpoint is proposed from the held records alone):
///   ``assertion `left == right` failed: the rooted log proposes the whole log's next
///   checkpoint``, `left: None`: no held record is A's newest.
#[test]
fn vault_checkpoint_a_dropped_record_nothing_follows_is_a_head() {
    let (mut whole, body, last) = dropped_head();
    let mut rooted = whole.rooted_at(&body).unwrap();
    assert!(rooted.is_dropped(&last));
    assert_eq!(
        rooted.heads(),
        whole.heads(),
        "the heads are the whole log's"
    );
    assert_eq!(rooted.heads_digest(), whole.heads_digest());
    // The heads APIs read a dropped head as the whole log reads its record: it reaches every
    // held record in its past, and a projection at the heads is the log, its heads the same.
    let heads = rooted.heads();
    for record in rooted.records() {
        assert_eq!(
            rooted.reaches(&heads, record.id()),
            whole.reaches(&heads, record.id()),
            "a dropped head reaches what its record does"
        );
    }
    #[cfg(unix)]
    {
        let projected = rooted.project_heads(&heads).unwrap();
        assert_eq!(projected.heads(), heads);
        assert_eq!(projected.held_count(), rooted.held_count());
    }
    // The next cut ends at the dropped share, A's newest record: the rooted log reads it off its
    // stub (its signer, previous record and clock) and proposes the whole log's checkpoint.
    let later = D + 50 * DAY_MS + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
    let next = propose(&whole, later).expect("a next proposal");
    assert_eq!(heads_of(&next.body), vec![last]);
    assert_eq!(
        propose(&rooted, later).map(|plan| checkpoint_id(&plan.body)),
        Some(checkpoint_id(&next.body)),
        "the rooted log proposes the whole log's next checkpoint"
    );
    let next = one(draft(&rooted, A, D + 41 * DAY_MS, name("a3")), A);
    assert_eq!(next.parents(), [last]);
    hold(&mut rooted, next.clone());
    hold(&mut whole, next.clone());
    assert_eq!(rooted.heads(), vec![*next.id()]);
    assert_eq!(rooted.heads(), whole.heads());
    assert_eq!(rooted.heads_digest(), whole.heads_digest());
}

/// A rooted log's stubs count against the byte bound with its records, so it never holds more
/// than the whole log was allowed: what it holds is its records' wire bytes and its stubs', less
/// than the whole log's, and a record that fits the bound beside the records and not beside the
/// stubs too is refused. No stub counts against a count budget: a prune frees count room.
///
/// NEGATIVE CONTROL: `Mutant::FreeStubs` (the stubs count against no budget, as slice 1 did), run
/// from the test's first line and seen to fail with ``assertion `left == right` failed: the
/// stubs count``, `left` the held records' bytes alone.
#[test]
fn vault_checkpoint_stubs_count_against_the_byte_bound() {
    let slept = sleeper();
    let whole = slept.l2.clone();
    let rooted = whole.rooted_at(&slept.body).unwrap();
    let stubs: usize = rooted
        .rooting()
        .unwrap()
        .stubs
        .values()
        .map(Stub::bytes)
        .sum();
    assert!(stubs > 0);
    assert_eq!(
        rooted.total_bytes(),
        rooted.held_bytes() + stubs,
        "the stubs count"
    );
    assert_eq!(rooted.fill(None).bytes, rooted.held_bytes() + stubs);
    assert!(
        rooted.total_bytes() < whole.total_bytes(),
        "a stub takes less than its record"
    );
    assert_eq!(rooted.counts().held() + 1, whole.counts().held());
    let next = one(draft(&rooted, A, T + 2 * DAY_MS, name("a3")), A);
    let bound = rooted.held_bytes() + stubs + next.wire_len();
    RecordBudgets::within_bytes(bound - 1, || {
        let error = rooted
            .clone()
            .insert(next.clone())
            .expect_err("the stubs fill the bound");
        assert_eq!(error.kind(), crate::ErrorKind::MembershipCapacity);
    });
    RecordBudgets::within_bytes(bound, || {
        assert_eq!(rooted.clone().insert(next.clone()).unwrap(), Inserted::Held);
    });
}

/// A vault whose prune drops A's first write of `z`, superseded by its second, while register
/// `k` loses nothing (no op of it is superseded by its own signer's): a fourth Mac puts a value,
/// C deletes it and A concurrently reseals it (a carry that
/// loses to C's `Delete`), and B deletes `k` again over both, and A and C then write after B, so
/// the cut holds every op. Returns the whole log, its proposal and B's second `Delete`, which
/// both logs read as a carry of C's `Delete`. With `sibling`, a new value of `k` is put
/// concurrently with B's second `Delete` (by the fourth Mac), which drops C's `Delete` and
/// makes `k` a conflict.
fn lost_nothing(sibling: bool) -> (Log, checkpoint::Plan, RecordId, RecordId) {
    let e4 = Mac(4);
    let mut log = found(&[A, B, C, e4]);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C, e4]);
    let put = |log: &Log, by: Mac, at: u64, name: &str, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, name, value),
        )
        .unwrap()
    };
    let delete =
        |log: &Log, by: Mac, at: u64| log.draft_delete(&by.id(), at, e, &key, "k").unwrap();
    let z1 = held!(
        log,
        two(
            put(&log, A, t0 + HOUR, "z", b"z1"),
            A,
            Some(&Statement::default())
        )
    );
    held!(
        log,
        two(
            put(&log, A, t0 + 2 * HOUR, "z", b"z2"),
            A,
            Some(&supersedes(&[&z1], None))
        )
    );
    let p = held!(
        log,
        two(
            put(&log, e4, t0 + 3 * HOUR, "k", b"p"),
            e4,
            Some(&Statement::default())
        )
    );
    let base = log.clone();
    let d1 = two(
        delete(&base, C, t0 + 4 * HOUR),
        C,
        Some(&supersedes(&[&p], None)),
    );
    let carry = two(
        put(&base, A, t0 + 4 * HOUR, "k", b"p"),
        A,
        Some(&supersedes(&[&p], Some(&p))),
    );
    let d1 = hold(&mut log, d1);
    let carry = hold(&mut log, carry);
    let before_x = log.clone();
    let x = held!(
        log,
        two(
            delete(&log, B, t0 + 5 * HOUR),
            B,
            Some(&supersedes(&[&p, &d1, &carry], None))
        )
    );
    if sibling {
        let other = two(
            put(&before_x, e4, t0 + 5 * HOUR, "k", b"q"),
            e4,
            Some(&supersedes(&[&p, &d1, &carry], None)),
        );
        hold(&mut log, other);
    }
    held!(log, one(draft(&log, A, t0 + 6 * HOUR, name("a")), A));
    held!(log, one(draft(&log, C, t0 + 7 * HOUR, name("c")), C));
    held!(log, one(draft(&log, B, t0 + 8 * HOUR, name("b")), B));
    held!(log, one(draft(&log, e4, t0 + 9 * HOUR, name("e")), e4));
    let plan = propose(&log, NOW).expect("a proposal");
    // The sibling's `Put` names the fourth Mac's first, which A's kept reseal carries.
    assert_eq!(
        plan.dropped,
        vec![*z1.id()],
        "the prune drops A's first `z` alone"
    );
    (log, plan, *x.id(), *d1.id())
}

/// A register that lost nothing to the prune reads as its frontier says, in both logs, though
/// both read B's second `Delete` of `k` by its statement, and a dropped op (A's first `z`) of
/// its audience lies in its past: of the newest ops it names, A's reseal is a carry C's `Delete`
/// replaced (its rep lies before C's `Delete`), so the statement reads C's `Delete` alone as
/// live, and both logs read B's `Delete` as a carry of it. Where A's concurrent `Put` drops
/// C's `Delete`, both read `k` as a conflict.
///
/// NEGATIVE CONTROL: `Mutant::StatedCarriesLive` (a `Delete`'s statement reads every op it names
/// among the newest as live, A's replaced reseal too, so B's `Delete` writes), run from the
/// test's first line and seen to fail with ``assertion `left == right` failed: B's second
/// `Delete` is a carry of C's (sibling false)``, `left: None`.
#[test]
fn vault_checkpoint_a_register_that_lost_nothing_reads_its_frontier() {
    for sibling in [false, true] {
        let (whole, plan, x, d1) = lost_nothing(sibling);
        let rooted = whole.rooted_at(&plan.body).unwrap();
        if !sibling {
            assert_eq!(
                carried(&whole, A).get(&x),
                Some(&d1),
                "B's second `Delete` is a carry of C's (sibling {sibling})"
            );
        }
        assert_eq!(
            evaluated_registers(&rooted, &BTreeSet::new()),
            evaluated_registers(&whole, &BTreeSet::new()),
            "B's second `Delete` reads as in the whole log (sibling {sibling})"
        );
        assert_eq!(
            evaluated(&rooted),
            evaluated(&whole),
            "`k` reads as in the whole log (sibling {sibling})"
        );
    }
}

/// A statement's names count only where the frames show they could be true
/// ([`super::checkpoint`]'s rule): a Mac that slept across the cut writes `a.key` over A's
/// dropped ops, naming them as replaced, and states that it carries a record that is no `Put` of
/// its audience (its own `Name`, or its own `Put` of its hosts audience). The rooted log reads
/// that write as stating no carry, which is what it is, as the whole log reads it: an edit of a
/// value of its own, which A's newer `Put` conflicts with. No carry stands for a record of another
/// kind or audience, which a removed Mac's exposure would read.
///
/// NEGATIVE CONTROL: `Mutant::AnyName` (a statement's names count whatever they name), run from
/// the test's first line and seen to fail with ``assertion `left == right` failed: the write reads
/// as in the whole log (hosts false)``, the left naming another winning op of `a.key` than the
/// right.
#[test]
fn vault_checkpoint_a_statement_names_only_an_op_it_could() {
    for hosts in [false, true] {
        let mut log = found(&[A, B, C]);
        let t0 = T - 5 * DAY_MS;
        let (e, key) = epoch(
            &mut log,
            A,
            T - 41 * DAY_MS,
            EpochAudience::Everyone,
            1,
            &[A, B, C],
        );
        // C is no recent seat: its records are older than the window before the cut.
        let old = T - 40 * DAY_MS;
        let mine = EpochAudience::Hosts(vec![label(C)]);
        let (h, host_key) = epoch(&mut log, C, old, mine.clone(), 1, &[C]);
        let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
            log.draft_put(
                &by.id(),
                at,
                e,
                &key,
                &entry(&EpochAudience::Everyone, "a.key", value),
            )
            .unwrap()
        };
        let named = if hosts {
            let draft = log
                .draft_put(
                    &C.id(),
                    old + HOUR,
                    h,
                    &host_key,
                    &entry(&mine, "a.key", b"h"),
                )
                .unwrap();
            held!(log, two(draft, C, Some(&Statement::default())))
        } else {
            held!(log, one(draft(&log, C, old + HOUR, name("c0")), C))
        };
        let first = held!(
            log,
            two(
                put(&log, A, t0 + 2 * HOUR, b"one"),
                A,
                Some(&Statement::default())
            )
        );
        let stale = log.clone();
        held!(
            log,
            two(
                put(&log, A, t0 + 3 * HOUR, b"two"),
                A,
                Some(&supersedes(&[&first], None))
            )
        );
        held!(log, one(draft(&log, B, t0 + 4 * HOUR, name("b1")), B));
        let forged = hold(
            &mut log,
            two(
                put(&stale, C, T + DAY_MS, b"evil"),
                C,
                Some(&supersedes(&[&first], Some(&named))),
            ),
        );
        let plan = propose(&log, NOW).expect("a proposal");
        assert_eq!(plan.dropped, vec![*first.id()]);
        let rooted = log.rooted_at(&plan.body).unwrap();
        for mac in [A, B, C] {
            assert_eq!(
                carried(&rooted, mac).get(forged.id()),
                None,
                "no carry stands for a record it could not name (hosts {hosts})"
            );
        }
        assert_eq!(
            conflicted(&log, B),
            BTreeSet::from(["a.key".to_owned()]),
            "the write is an edit, in conflict with A's newer `Put` (hosts {hosts})"
        );
        assert_eq!(
            evaluated_registers(&rooted, &BTreeSet::new()),
            evaluated_registers(&log, &BTreeSet::new()),
            "the write reads as in the whole log (hosts {hosts})"
        );
        assert_eq!(evaluated(&rooted), evaluated(&log));
    }
}

/// A dropped `Delete` that is a carry stands, in the rooted log, for the op it stands for in the
/// whole log, which its stub keeps ([`Stub::carries`], read off its statement when it was
/// dropped): B deletes `k` (an edit over A's `Put`), A deletes it again over B's `Delete` (a carry
/// of it) and once more over its own (a carry too), and the prune drops A's middle `Delete`. C,
/// waking on the log it last took, deletes `k` over A's dropped `Delete`: a carry of B's
/// `Delete` in both logs, beside A's last.
///
/// NEGATIVE CONTROL: `Mutant::DeleteStandsForItself` (a dropped `Delete`'s stub keeps no rep),
/// run from the test's first line and seen to fail at the stub with ``assertion `left == right`
/// failed``, `left: None` against B's `Delete`, and, that line removed, with ``assertion `left ==
/// right` failed: C's `Delete` stands for B's in both logs``.
#[test]
fn vault_checkpoint_a_dropped_delete_stands_for_its_rep() {
    let mut log = found(&[A, B, C]);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = log
        .draft_put(
            &A.id(),
            t0 + HOUR,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "k", b"v"),
        )
        .unwrap();
    let delete =
        |log: &Log, by: Mac, at: u64| log.draft_delete(&by.id(), at, e, &key, "k").unwrap();
    let p = held!(log, two(put, A, Some(&Statement::default())));
    let d0 = held!(
        log,
        two(
            delete(&log, B, t0 + 2 * HOUR),
            B,
            Some(&supersedes(&[&p], None))
        )
    );
    let d1 = held!(
        log,
        two(
            delete(&log, A, t0 + 3 * HOUR),
            A,
            Some(&supersedes(&[&p, &d0], None))
        )
    );
    let stale = log.clone();
    held!(
        log,
        two(
            delete(&log, A, t0 + 4 * HOUR),
            A,
            Some(&supersedes(&[&p, &d0, &d1], None))
        )
    );
    held!(log, one(draft(&log, B, t0 + 5 * HOUR, name("b1")), B));
    let woke = hold(
        &mut log,
        two(
            delete(&stale, C, T + DAY_MS),
            C,
            Some(&supersedes(&[&p, &d0, &d1], None)),
        ),
    );
    let plan = propose(&log, NOW).expect("a proposal");
    let mut dropped = vec![*p.id(), *d1.id()];
    dropped.sort_unstable();
    assert_eq!(plan.dropped, dropped, "A's `Put` and middle `Delete`");
    let rooted = log.rooted_at(&plan.body).unwrap();
    assert_eq!(rooted.stub(d1.id()).unwrap().carries, Some(*d0.id()));
    assert_eq!(carried(&log, A).get(woke.id()), Some(d0.id()));
    assert_eq!(
        evaluated_registers(&rooted, &BTreeSet::new()),
        evaluated_registers(&log, &BTreeSet::new()),
        "C's `Delete` stands for B's in both logs"
    );
}

/// What the whole log's next prune keeps of what an earlier prune dropped, the rooted log keeps
/// out of its next body's `dropped`, so it computes the whole log's next checkpoint and prunes at
/// it: C's reseal beside the first cut carries the value of A's reseal, which the first prune
/// dropped, and once the next cut holds C's reseal, a kept op, the whole log's prune keeps A's
/// reseal and the first `Put` it carries.
///
/// NEGATIVE CONTROL: `Mutant::DropEveryStub` (the next prune drops every stub in its cut), run
/// from the test's first line and seen to fail with ``assertion `left == right` failed: the rooted
/// log proposes the whole log's next checkpoint`` (another `dropped`, so another body id).
#[test]
fn vault_checkpoint_a_second_prune_keeps_what_the_whole_log_keeps() {
    let (mut whole, plan, carry, first) = carry_of_dropped(false);
    let mut rooted = whole.rooted_at(&plan.body).unwrap();
    for (at, mac) in [(2, A), (3, B), (4, C)] {
        let record = one(draft(&whole, mac, T + at * DAY_MS, name("after")), mac);
        hold(&mut whole, record.clone());
        assert_eq!(rooted.insert(record).unwrap(), Inserted::Held);
    }
    let now = T + 5 * DAY_MS + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
    let next = propose(&whole, now).expect("a next proposal");
    assert!(whole.reaches(&heads_of(&next.body), &carry));
    assert!(
        !next.dropped.contains(&first) && rooted.is_dropped(&first),
        "the whole log's next prune keeps what the first dropped"
    );
    assert_eq!(
        propose(&rooted, now).map(|plan| checkpoint_id(&plan.body)),
        Some(checkpoint_id(&next.body)),
        "the rooted log proposes the whole log's next checkpoint"
    );
    let again = rooted.rooted_at(&next.body).unwrap();
    assert!(again.is_dropped(&first));
    assert_eq!(evaluated(&again), evaluated(&whole));
}

/// A wide place costs a stub nothing: 24 Macs each write a `Name` at once, A writes 40 version-1
/// `Put`s after all of them, each following the one before, and a version-2 `Put` that names
/// them as replaced, and the prune drops the 40. The 24 `Name`s are the place of every dropped `Put` (the held records its past
/// ends at), whose 768 bytes outweigh each small record. A log at its byte bound prunes there,
/// and takes a new record in the room the prune frees.
/// Each stub keeps its record's parents alone, never the 24 newest records its past holds, so it
/// takes fewer bytes than its record, and the rooted log fewer than the whole.
///
/// NEGATIVE CONTROL: `Mutant::StoredPlace` (each stub also counts the place the reviewed branch
/// kept, 2 bytes and 32 per record), run from the test's first line and seen to fail with
/// ``called `Result::unwrap()` on an `Err` value: Error("membership log holds its limit of record
/// bytes", MembershipCapacity)``: the log at its bound could not take a record after its prune
/// (and, the bound lifted, with ``a prune frees bytes: 68112 against 42992``).
#[test]
fn vault_checkpoint_a_stub_takes_fewer_bytes_than_its_record() {
    let macs: Vec<Mac> = (1..=24).map(Mac).collect();
    let mut log = found(&macs);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &macs);
    let base = log.clone();
    for (k, &mac) in macs.iter().enumerate() {
        let record = one(draft(&base, mac, t0 + 1 + k as u64, name("wide")), mac);
        hold(&mut log, record);
    }
    let mut small: Vec<Record> = Vec::new();
    for k in 0..41u64 {
        let draft = log
            .draft_put(
                &A.id(),
                t0 + HOUR + k,
                e,
                &key,
                &entry(&EpochAudience::Everyone, "k", &k.to_be_bytes()),
            )
            .unwrap();
        if k < 40 {
            small.push(held!(log, one(draft, A)));
        } else {
            let named: Vec<&Record> = small.iter().collect();
            held!(log, two(draft, A, Some(&supersedes(&named, None))));
        }
    }
    let plan = plan_at(&log, T, &log.heads()).unwrap();
    assert_eq!(plan.dropped.len(), 40);
    // A log at its byte bound prunes, and the room it frees takes the next record.
    let full = log.total_bytes();
    let rooted = RecordBudgets::within_bytes(full, || {
        let rooted = log.rooted_at(&plan.body).unwrap();
        let next = one(draft(&rooted, B, T + DAY_MS, name("next")), B);
        assert_eq!(rooted.clone().insert(next).unwrap(), Inserted::Held);
        rooted
    });
    for (id, stub) in &rooted.rooting().unwrap().stubs {
        assert!(stub.bytes() < log.get(id).unwrap().wire_len());
    }
    assert!(
        rooted.total_bytes() < log.total_bytes(),
        "a prune frees bytes: {} against {}",
        rooted.total_bytes(),
        log.total_bytes()
    );
}

/// A vault whose final checkpoint's prune drops A's first write of `a.key`, which its second, in
/// the cut, supersedes. A and B, the recent seats, share it, so it is final. C, silent in the
/// month before the cut, last took the log when it held the first write alone. Returns the
/// whole log, the checkpoint, A's first `Put` and C's stale log.
fn final_cut_over_a_superseded_put() -> (Log, Body, RecordId, Log) {
    let mut log = found(&[A, B, C]);
    let t0 = T - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, at: u64, value: &[u8]| {
        log.draft_put(
            &A.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "a.key", value),
        )
        .unwrap()
    };
    let first = held!(
        log,
        two(put(&log, t0 + HOUR, b"one"), A, Some(&Statement::default()))
    );
    let stale = log.clone();
    held!(
        log,
        two(
            put(&log, t0 + 2 * HOUR, b"two"),
            A,
            Some(&supersedes(&[&first], None))
        )
    );
    held!(log, one(draft(&log, B, t0 + 3 * HOUR, name("b1")), B));
    let plan = propose(&log, NOW).expect("a proposal");
    assert_eq!(plan.dropped, vec![*first.id()]);
    share_at(&mut log, A, &plan.body, T + 2 * DAY_MS);
    share_at(&mut log, B, &plan.body, T + 2 * DAY_MS + HOUR);
    assert!(checkpoint::judge(&log, &plan.body).unwrap().is_final);
    (log, plan.body, *first.id(), stale)
}

/// The names of the entries `mac` reads in conflict in `log`.
fn conflicted(log: &Log, mac: Mac) -> BTreeSet<String> {
    log.open_audience_entries(&mac.identity())
        .unwrap()
        .map(|opened| {
            opened
                .concurrent
                .into_keys()
                .map(|(_, name)| name)
                .collect()
        })
        .unwrap_or_default()
}

/// What `mac` reads of each entry of `log`: its name and value.
fn values(log: &Log, mac: Mac) -> Option<Vec<(String, Vec<u8>)>> {
    log.open_entry_set(&mac.identity()).unwrap().map(|opened| {
        opened
            .entries
            .iter()
            .map(|e| (e.name.clone(), e.value.as_slice().to_vec()))
            .collect()
    })
}

/// A `Remove` whose cutoff lies behind a final cut revives nothing the prune dropped, in the whole
/// log as in the rooted one: after a Mac prunes at the final checkpoint, B removes A at A's first
/// `Put` of `a.key` (which the prune dropped), and, with `quorum`, C, waking on its stale log,
/// removes A there too, making a quorum that voids what A signed after it. A is out in both logs,
/// and its share of the checkpoint no longer counts, so [`checkpoint::judge`] no longer finds the
/// checkpoint final; but its recent seats signed it, so it stays settled, and what its prune
/// dropped counts in neither log ([`checkpoint::gone`]). The `Remove` stops counting A's second
/// `Put`, and the first stays dropped: both logs read `a.key` unset, which a seat sets again.
///
/// NEGATIVE CONTROL: `Mutant::Revives` (a record a settled prune dropped counts as its evaluation
/// alone says), run from the test's first line and seen to fail with ``assertion `left == right`
/// failed: the rooted log reads `a.key` as the whole log (quorum false)``, `left: Some([])` against
/// `right: Some([("a.key", [111, 110, 101])])`: the whole log reads A's first `Put` again.
#[test]
fn vault_checkpoint_a_remove_behind_a_final_cut_revives_nothing() {
    for quorum in [false, true] {
        let (mut whole, body, first, stale) = final_cut_over_a_superseded_put();
        let mut rooted = whole.adopt(&body).unwrap();
        assert!(rooted.is_dropped(&first));
        let remove = |log: &Log, by: Mac, at: u64| {
            one(
                draft(
                    log,
                    by,
                    at,
                    Body::Remove {
                        member: A.id(),
                        cutoff: first,
                    },
                ),
                by,
            )
        };
        let mut removers = vec![remove(&whole, B, T + 3 * DAY_MS)];
        if quorum {
            removers.push(remove(&stale, C, T + 3 * DAY_MS + HOUR));
        }
        for record in removers {
            hold(&mut whole, record.clone());
            assert_eq!(rooted.insert(record).unwrap(), Inserted::Held);
        }
        assert_eq!(
            whole.membership().removed().get(&A.id()).map(|r| r.quorum),
            Some(quorum),
            "A is out"
        );
        assert!(
            !checkpoint::judge(&whole, &body).unwrap().is_final,
            "A's share no longer counts (quorum {quorum})"
        );
        assert_eq!(
            values(&rooted, B),
            values(&whole, B),
            "the rooted log reads `a.key` as the whole log (quorum {quorum})"
        );
        assert_eq!(
            values(&whole, B),
            Some(Vec::new()),
            "the `Remove` voids A's second `Put`, and the first stays dropped (quorum {quorum})"
        );
        assert_eq!(evaluated(&rooted), evaluated(&whole));
        assert_eq!(
            evaluated_registers(&rooted, &BTreeSet::new()),
            evaluated_registers(&whole, &BTreeSet::new())
        );
    }
}

/// How a version-2 write's statement may misstate what its writer read ([`World::write`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lie {
    /// It states the truth: it carries the value of the op it names.
    Truth,
    /// It names nothing as replaced and carries nothing.
    Missing,
    /// It states that it carries the value of an op it may name, though it writes its own.
    FalseCarry,
    /// It states no carry, though it seals the value it read again.
    NoCarry,
}

/// A statement that is false reads alike in every log, whatever ops a prune dropped: A writes
/// `a.key` three times, each superseding the one before, around two cuts of one log (the first
/// drops A's first write, the second its first two), and C, waking on the log it took when it
/// held A's first write alone, writes `a.key` beside both cuts, naming that write and lying about
/// it: claiming to carry its value while writing another (`FalseCarry`), sealing its value again
/// while stating no carry (`NoCarry`), or doing so while naming nothing (`Missing`). The whole
/// log and the logs rooted at each cut read C's write by its statement, so all three read `a.key`,
/// its conflicts and its carries alike.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::FrontierOnly` (every log reads by the frontier it holds): ``assertion `left ==
///   right` failed: the log rooted at the first cut reads as the whole log (Truth)``: the whole
///   log reads C's true carry by the frontier that holds A's first write, the rooted log by one
///   that lost it.
/// - `Mutant::RootedStatementsOnly` (a whole log reads a version-2 op by its frontier, as before
///   this slice): ``assertion `left == right` failed: C's write is an edit in conflict with A's
///   newest unless its statement makes it a carry, which loses to it (FalseCarry)``, `left:
///   {"a.key"}` against `right: {}`: the whole log reads C's false carry as the edit its
///   frontier makes it.
#[test]
fn vault_checkpoint_a_false_statement_reads_alike_in_every_log() {
    for lie in [Lie::Truth, Lie::FalseCarry, Lie::NoCarry, Lie::Missing] {
        let mut log = found(&[A, B, C]);
        let t0 = T - 10 * DAY_MS;
        let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
        let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
            log.draft_put(
                &by.id(),
                at,
                e,
                &key,
                &entry(&EpochAudience::Everyone, "a.key", value),
            )
            .unwrap()
        };
        let p1 = held!(
            log,
            two(
                put(&log, A, t0 + HOUR, b"one"),
                A,
                Some(&Statement::default())
            )
        );
        let stale = log.clone();
        let p2 = held!(
            log,
            two(
                put(&log, A, t0 + 2 * HOUR, b"two"),
                A,
                Some(&supersedes(&[&p1], None))
            )
        );
        let b1 = held!(log, one(draft(&log, B, t0 + 3 * HOUR, name("b1")), B));
        let first_day = T - 9 * DAY_MS;
        held!(
            log,
            two(
                put(&log, A, first_day + HOUR, b"three"),
                A,
                Some(&supersedes(&[&p1, &p2], None))
            )
        );
        let b2 = held!(
            log,
            one(draft(&log, B, first_day + 2 * HOUR, name("b2")), B)
        );
        let (value, statement): (&[u8], Statement) = match lie {
            Lie::Truth => (b"one", supersedes(&[&p1], Some(&p1))),
            Lie::FalseCarry => (b"evil", supersedes(&[&p1], Some(&p1))),
            Lie::NoCarry => (b"one", supersedes(&[&p1], None)),
            Lie::Missing => (b"one", Statement::default()),
        };
        hold(
            &mut log,
            two(put(&stale, C, T + DAY_MS, value), C, Some(&statement)),
        );
        let at_first = plan_at(&log, first_day, &[*b1.id()]).unwrap();
        let at_second = plan_at(&log, T - 8 * DAY_MS, &[*b2.id()]).unwrap();
        assert_eq!(at_first.dropped, vec![*p1.id()]);
        let mut both = vec![*p1.id(), *p2.id()];
        both.sort_unstable();
        assert_eq!(at_second.dropped, both);
        let first = log.rooted_at(&at_first.body).unwrap();
        let second = log.rooted_at(&at_second.body).unwrap();
        let carry = matches!(lie, Lie::Truth | Lie::FalseCarry);
        let want: BTreeSet<String> = if carry {
            BTreeSet::new()
        } else {
            BTreeSet::from(["a.key".to_owned()])
        };
        assert_eq!(
            conflicted(&log, B),
            want,
            "C's write is an edit in conflict with A's newest unless its statement makes it a \
             carry, which loses to it ({lie:?})"
        );
        for (rooted, which) in [(&first, "first"), (&second, "second")] {
            assert_eq!(
                evaluated(rooted),
                evaluated(&log),
                "the log rooted at the {which} cut reads as the whole log ({lie:?})"
            );
            assert_eq!(
                evaluated_registers(rooted, &BTreeSet::new()),
                evaluated_registers(&log, &BTreeSet::new()),
                "the log rooted at the {which} cut reads its registers as the whole log ({lie:?})"
            );
        }
    }
}

/// A log rooted at a final checkpoint that dropped an earlier checkpoint's shares judges the
/// checkpoints that nest it as the whole log does. A and B, the recent seats, share four nested
/// checkpoints `K`, `N`, `P` and `Q`, each naming the one before as its predecessor; a prune at
/// `P`, whose newest final checkpoint is `N`, keeps one share of `K`, `N`'s predecessor, and drops
/// the other. A judgment of `N` finds `K` final within `N`'s cut by both of `K`'s shares: the
/// rooted log counts the dropped one at its stub ([`Stub::shares`]), whose signer and counting
/// the evaluators read there. A judgment of `Q` finds `P`, the root, final within `Q`'s cut, and
/// `P` recomputed as the log adopted it.
///
/// NEGATIVE CONTROL: `Mutant::SharesUnkept` (a dropped share's stub keeps no checkpoint), run
/// from the test's first line and seen to fail with ``assertion `left == right` failed: the
/// rooted log judges `N` as the whole log``, `left` recomputing nothing (`recomputed: false`)
/// where `right` does: without the dropped share `K` is not final within `N`'s cut, so the
/// rooted log computes `N` with no predecessor.
#[test]
fn vault_checkpoint_a_rooted_log_judges_a_nesting_checkpoint_as_the_whole_log() {
    let mut log = found(&[A, B, C]);
    let d0 = T - 20 * DAY_MS;
    let a1 = held!(log, one(draft(&log, A, d0 - 3 * HOUR, name("a1")), A));
    let b1 = held!(
        log,
        one(
            draft_after(&log, B, d0 - 2 * HOUR, &[*a1.id()], name("b1")),
            B
        )
    );
    let mut bodies: Vec<Body> = Vec::new();
    let mut heads = vec![*b1.id()];
    for k in 0..4u64 {
        let day = d0 + k * DAY_MS;
        let plan = plan_at(&log, day, &heads).unwrap();
        assert_eq!(
            plan.predecessor(),
            bodies.last().and_then(checkpoint_id),
            "each checkpoint follows the one before"
        );
        share_at(&mut log, A, &plan.body, day + HOUR);
        share_at(&mut log, B, &plan.body, day + 2 * HOUR);
        let by = if k % 2 == 0 { A } else { B };
        let next = held!(log, one(draft(&log, by, day + 3 * HOUR, name("n")), by));
        heads = vec![*next.id()];
        bodies.push(plan.body);
    }
    let [k, n, p, q] = &bodies[..] else {
        panic!("four checkpoints");
    };
    let rooted = log.adopt(p).unwrap();
    // `Q` drops one of `N`'s shares, settled, so that one counts nowhere ([`checkpoint::gone`]):
    // both logs recompute `N`, and neither finds it final.
    for (body, which, is_final) in [(n, "N", false), (q, "Q", true)] {
        let whole = checkpoint::judge(&log, body).unwrap();
        assert!(whole.recomputed && whole.is_final == is_final);
        assert_eq!(
            checkpoint::judge(&rooted, body).unwrap(),
            whole,
            "the rooted log judges `{which}` as the whole log"
        );
    }
    let k_id = checkpoint_id(k);
    let stubs = &rooted.rooting().unwrap().stubs;
    assert_eq!(
        stubs
            .values()
            .filter(|stub| stub.shares.is_some() && stub.shares == k_id)
            .count(),
        1,
        "the prune at `P` dropped one share of `K`"
    );
    let rooted_q = rooted.adopt(q).unwrap();
    assert_eq!(rooted_q.rooting().unwrap().roots.len(), 1);
    assert_eq!(evaluated(&rooted_q), evaluated(&log));
}

/// A and B, the recent seats, share a chain of `n` nested checkpoints, a day apart, each naming the
/// one before. Returns the log and the bodies.
fn settled_chain(n: u64) -> (Log, Vec<Body>) {
    let mut log = found(&[A, B, C]);
    let d0 = T - 20 * DAY_MS;
    let a1 = held!(log, one(draft(&log, A, d0 - 3 * HOUR, name("a1")), A));
    let b1 = held!(
        log,
        one(
            draft_after(&log, B, d0 - 2 * HOUR, &[*a1.id()], name("b1")),
            B
        )
    );
    let mut bodies: Vec<Body> = Vec::new();
    let mut heads = vec![*b1.id()];
    for k in 0..n {
        let day = d0 + k * DAY_MS;
        let plan = plan_at(&log, day, &heads).unwrap();
        share_at(&mut log, A, &plan.body, day + HOUR);
        share_at(&mut log, B, &plan.body, day + 2 * HOUR);
        let by = if k % 2 == 0 { A } else { B };
        let next = held!(log, one(draft(&log, by, day + 3 * HOUR, name("n")), by));
        heads = vec![*next.id()];
        bodies.push(plan.body);
    }
    (log, bodies)
}

/// The plans a fresh log of `log`'s records computes to read what its settled checkpoints drop
/// ([`checkpoint::gone`]), with the number of records that leaves counting nowhere.
fn plans_to_read_gone(log: &Log) -> (usize, usize) {
    let mut fresh = Log::new(log.genesis().clone()).unwrap();
    for record in log.records().skip(1) {
        fresh.insert(record.clone()).unwrap();
    }
    checkpoint::PLANS_COMPUTED.set(0);
    let gone = checkpoint::gone(&fresh).map_or(0, |gone| gone.len());
    (checkpoint::PLANS_COMPUTED.get(), gone)
}

/// A log reads what a chain of nested settled checkpoints drops computing each checkpoint's plan
/// once: ten checkpoints shared by A and B, each nesting the one before, cost a fresh log ten
/// plans, where each cut's own evaluation ([`checkpoint::gone`]) asks again of every checkpoint
/// its cut nests ([`checkpoint::Cuts`] keeps each answer for the log, its clones and its cuts).
///
/// NEGATIVE CONTROL: `checkpoint::Mutant::NoCutMemo` (no log keeps what a cut says), run from the
/// test's first line and seen to fail with ``assertion `left == right` failed: one plan per
/// checkpoint``, `left: 22749` against `right: 10`: about 2.7 times more for each checkpoint the
/// chain grows by.
#[test]
fn vault_checkpoint_a_chain_of_settled_checkpoints_costs_one_plan_each() {
    let n = 10;
    let (log, bodies) = settled_chain(n);
    let (plans, gone) = plans_to_read_gone(&log);
    assert!(gone > 0, "the chain's prunes drop its older shares");
    assert_eq!(plans, bodies.len(), "one plan per checkpoint");
}

/// A checkpoint settled by shares that follow a later checkpoint's cut reads alike in the whole log
/// and in a log rooted at that later checkpoint. `S`, at day `d1`, drops A's `x` (A's `x2`
/// superseded it) and keeps A's `z`; past `d1`, A's `z2` supersedes `z` and B's `y` carries `x`'s
/// value, so `R`, two days on, keeps `x` (for `y`'s carry) and drops `z`. A and B, the recent
/// seats, share `S` and then `R`, all after `R`'s day, so `S`'s shares lie outside `R`'s cut. A log
/// rooted at `R` holds `z` as a stub, reads it as `S`'s cut keeps it, and so computes `S` and
/// holds it settled as the whole log does: `x` counts in neither ([`checkpoint::gone`]), and both
/// propose one next checkpoint, which drops `x`.
///
/// NEGATIVE CONTROL: `Mutant::StubsDroppedForGood` (a plan keeps a dropped op only for a carry, a
/// cutoff or an owed epoch), run from the test's first line and seen to fail with
/// ``assertion `left == right` failed: the rooted log holds `S` settled``, `left` one id (`R`'s)
/// against `right` two: the rooted log drops `z`'s stub again at `S`'s cut, so it computes
/// another body.
#[test]
fn vault_checkpoint_a_rooted_log_settles_an_earlier_checkpoint_as_the_whole_log() {
    let mut log = found(&[A, B, C]);
    let d1 = T - 20 * DAY_MS;
    let t0 = d1 - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, by: Mac, at: u64, name: &str, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, name, value),
        )
        .unwrap()
    };
    let x = held!(
        log,
        two(
            put(&log, A, t0 + HOUR, "x.key", b"x"),
            A,
            Some(&Statement::default())
        )
    );
    held!(
        log,
        two(
            put(&log, A, t0 + 2 * HOUR, "x.key", b"x2"),
            A,
            Some(&supersedes(&[&x], None))
        )
    );
    let z = held!(
        log,
        two(
            put(&log, A, t0 + 3 * HOUR, "z.key", b"z"),
            A,
            Some(&Statement::default())
        )
    );
    held!(log, one(draft(&log, B, t0 + 4 * HOUR, name("b1")), B));
    let s = plan_at(&log, d1, &log.heads()).unwrap();
    assert_eq!(s.dropped, vec![*x.id()]);
    held!(
        log,
        two(
            put(&log, A, d1 + HOUR, "z.key", b"z2"),
            A,
            Some(&supersedes(&[&z], None))
        )
    );
    held!(
        log,
        two(
            put(&log, B, d1 + 2 * HOUR, "x.key", b"x"),
            B,
            Some(&supersedes(&[], Some(&x)))
        )
    );
    let r_day = d1 + 2 * DAY_MS;
    let r = plan_at(&log, r_day, &log.heads()).unwrap();
    assert!(r.keep.contains(x.id()) && r.dropped.contains(z.id()));
    for (body, at) in [(&s.body, r_day + HOUR), (&r.body, r_day + 2 * HOUR)] {
        share_at(&mut log, A, body, at);
        share_at(&mut log, B, body, at + HOUR);
    }
    let rooted = log.adopt(&r.body).unwrap();
    assert!(rooted.is_dropped(z.id()) && rooted.get(x.id()).is_some());
    let settled =
        |log: &Log| -> Vec<RecordId> { checkpoint::settled(log).iter().map(|s| s.id).collect() };
    assert_eq!(
        settled(&rooted),
        settled(&log),
        "the rooted log holds `S` settled"
    );
    assert!(settled(&log).contains(&checkpoint_id(&s.body).unwrap()));
    let x_gone = |log: &Log| {
        checkpoint::gone(log)
            .is_some_and(|gone| log.position(x.id()).is_some_and(|p| gone.contains(&p)))
    };
    assert!(x_gone(&log) && x_gone(&rooted), "`x` counts in neither");
    assert_eq!(evaluated(&rooted), evaluated(&log));
    let now = r_day + 3 * DAY_MS + MIN_AGE_MS + budgets::WINDOW_SKEW_MS;
    let next = propose(&log, now).expect("a next checkpoint");
    assert!(next.dropped.contains(x.id()), "the next prune drops `x`");
    assert_eq!(
        propose(&rooted, now).and_then(|plan| checkpoint_id(&plan.body)),
        checkpoint_id(&next.body),
        "the rooted log proposes the whole log's next checkpoint"
    );
}

/// A stolen key's statement drops no other signer's op. C writes `k0` and then `k1`; A's stolen
/// key writes `k2`, stating falsely that it replaced both of C's ops, though they stand in other
/// registers. A name supersedes only an op of the namer's own signer, so the checkpoint A, B and C
/// share drops neither of C's ops, and once B removes A at its last honest record, the whole log
/// and the log rooted at the checkpoint both read C's `k0` and `k1` and nothing of A's.
///
/// NEGATIVE CONTROL: `checkpoint::Mutant::CrossSigner` (FL-148b's rule: a name supersedes another
/// signer's op where a newer op of that signer is among the names), run from the test's first
/// line and seen to fail with ``assertion `left == right` failed: the whole log reads C's ops``,
/// `left: Some([("k1", ..)])` against `right: Some([("k0", ..), ("k1", ..)])`: its prune drops
/// `k0`, which the `Remove` of A then brings back in no log.
#[test]
fn vault_checkpoint_a_stolen_key_drops_no_other_signers_op() {
    let mut log = found(&[A, B, C]);
    let t0 = T - 10 * DAY_MS;
    let (e, key) = epoch(&mut log, B, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, by: Mac, at: u64, name: &str, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, name, value),
        )
        .unwrap()
    };
    let a0 = held!(log, one(draft(&log, A, t0 + HOUR, name("a0")), A));
    let k0 = held!(log, one(put(&log, C, t0 + 2 * HOUR, "k0", b"c0"), C));
    let k1 = held!(log, one(put(&log, C, t0 + 3 * HOUR, "k1", b"c1"), C));
    let k2 = held!(
        log,
        two(
            put(&log, A, t0 + 4 * HOUR, "k2", b"stolen"),
            A,
            Some(&supersedes(&[&k0, &k1], None))
        )
    );
    held!(log, one(draft(&log, B, t0 + 5 * HOUR, name("b1")), B));
    held!(log, one(draft(&log, C, t0 + 6 * HOUR, name("c2")), C));
    let plan = propose(&log, NOW).expect("a proposal");
    assert!(plan.keep.contains(k2.id()), "the cut holds A's `k2`");
    for mac in [A, B, C] {
        share_at(&mut log, mac, &plan.body, T + 2 * DAY_MS);
    }
    let mut rooted = log.adopt(&plan.body).unwrap();
    let remove = one(
        draft(
            &log,
            B,
            T + 3 * DAY_MS,
            Body::Remove {
                member: A.id(),
                cutoff: *a0.id(),
            },
        ),
        B,
    );
    hold(&mut log, remove.clone());
    assert_eq!(rooted.insert(remove).unwrap(), Inserted::Held);
    let want = Some(vec![
        ("k0".to_owned(), b"c0".to_vec()),
        ("k1".to_owned(), b"c1".to_vec()),
    ]);
    assert_eq!(values(&log, B), want, "the whole log reads C's ops");
    assert_eq!(values(&rooted, B), want, "the rooted log reads C's ops");
    assert_eq!(evaluated(&rooted), evaluated(&log));
    assert_eq!(
        plan.dropped,
        Vec::<RecordId>::new(),
        "the checkpoint drops none of C's ops"
    );
}

/// What a checkpoint a removed key forges alone can make final: nothing of another seat's. B
/// removes A at its last honest record; A's stolen key, on a fork of the log from before the
/// removal, writes `k2` stating falsely that it replaced C's `k0` and `k1`, signs a record with a
/// clock 400 days ahead, so that at a cut the next day A alone is a recent seat, and shares that
/// cut's checkpoint. The checkpoint is settled ([`checkpoint::settled`] counts a share past its
/// signer's cutoff, and a seat alone at its cut, as it must to never stop holding), but its prune
/// drops none of C's ops, since a name supersedes only its own signer's: B reads `k0` and `k1`,
/// and nothing of A's past its cutoff.
///
/// NEGATIVE CONTROL: `checkpoint::Mutant::CrossSigner` (a name supersedes another signer's op
/// where a newer op of that signer is among the names), run from the test's first line and seen
/// to fail with ``assertion `left == right` failed: B reads C's ops``, `left: Some([("k1", ..)])`
/// against `right: Some([("k0", ..), ("k1", ..)])`.
#[test]
fn vault_checkpoint_a_removed_keys_forged_checkpoint_drops_no_other_seats_op() {
    let mut log = found(&[A, B, C]);
    let t0 = T - 10 * DAY_MS;
    let (e, key) = epoch(&mut log, B, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, by: Mac, at: u64, name: &str, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, name, value),
        )
        .unwrap()
    };
    let a0 = held!(log, one(draft(&log, A, t0 + HOUR, name("a0")), A));
    let k0 = held!(log, one(put(&log, C, t0 + 2 * HOUR, "k0", b"c0"), C));
    let k1 = held!(log, one(put(&log, C, t0 + 3 * HOUR, "k1", b"c1"), C));
    held!(log, one(draft(&log, B, t0 + 4 * HOUR, name("b1")), B));
    let mut fork = log.clone();
    held!(
        log,
        one(
            draft(
                &log,
                B,
                t0 + 5 * HOUR,
                Body::Remove {
                    member: A.id(),
                    cutoff: *a0.id(),
                },
            ),
            B,
        )
    );
    held!(
        fork,
        two(
            put(&fork, A, t0 + 6 * HOUR, "k2", b"stolen"),
            A,
            Some(&supersedes(&[&k0, &k1], None))
        )
    );
    let ahead = T + 400 * DAY_MS;
    let head = held!(fork, one(draft(&fork, A, ahead, name("ahead")), A));
    let day = ahead / DAY_MS * DAY_MS + DAY_MS;
    let forged = plan_at(&fork, day, &[*head.id()]).unwrap();
    share_at(&mut fork, A, &forged.body, day + MIN_AGE_MS + HOUR);
    let judged = checkpoint::judge(&fork, &forged.body).unwrap();
    assert_eq!(judged.recent, BTreeSet::from([A.id()]), "A alone is recent");
    log.merge(&fork).unwrap();
    assert!(
        checkpoint::settled(&log)
            .iter()
            .any(|s| Some(s.id) == checkpoint_id(&forged.body)),
        "the forged checkpoint is settled"
    );
    let want = Some(vec![
        ("k0".to_owned(), b"c0".to_vec()),
        ("k1".to_owned(), b"c1".to_vec()),
    ]);
    assert_eq!(values(&log, B), want, "B reads C's ops");
}

/// A vault whose stub a later cut holds gone and an earlier cut keeps. `K`, at day `dk`, drops
/// A's `x` (A's `x2` superseded it); past `dk`, B's `y` carries `x`'s value, so `E`, two days on,
/// keeps `x` for `y`'s carry, its cut holding no share of `K`. A and B then share `K`, past
/// `E`'s cut and before `L`'s, two days later: `K` is settled in `L`'s cut, so `x` counts nowhere
/// there and `L` drops it. A and B share `L` past its cut, and `E` past it too or, with
/// `early_in_late`, before it: then `E` is final in `L`'s cut, `L` follows it, and `L`'s prune
/// keeps `E`'s shares and drops every share of `K`, which `E`'s cut nests. Returns the whole
/// log, `K`, `E`, `L` and `x`'s id.
fn gone_between_cuts(early_in_late: bool) -> (Log, Body, Body, Body, RecordId) {
    let mut log = found(&[A, B, C]);
    let dk = T - 30 * DAY_MS;
    let t0 = dk - 5 * DAY_MS;
    let (e, key) = epoch(&mut log, A, t0, EpochAudience::Everyone, 1, &[A, B, C]);
    let put = |log: &Log, by: Mac, at: u64, value: &[u8]| {
        log.draft_put(
            &by.id(),
            at,
            e,
            &key,
            &entry(&EpochAudience::Everyone, "x.key", value),
        )
        .unwrap()
    };
    let x = held!(
        log,
        two(
            put(&log, A, t0 + HOUR, b"x"),
            A,
            Some(&Statement::default())
        )
    );
    held!(
        log,
        two(
            put(&log, A, t0 + 2 * HOUR, b"x2"),
            A,
            Some(&supersedes(&[&x], None))
        )
    );
    held!(log, one(draft(&log, B, t0 + 3 * HOUR, name("b1")), B));
    let k = plan_at(&log, dk, &log.heads()).unwrap();
    assert_eq!(k.dropped, vec![*x.id()], "`K` drops `x`");
    held!(
        log,
        two(
            put(&log, B, dk + HOUR, b"x"),
            B,
            Some(&supersedes(&[], Some(&x)))
        )
    );
    held!(log, one(draft(&log, A, dk + 2 * HOUR, name("a1")), A));
    let d1 = dk + 2 * DAY_MS;
    let early = plan_at(&log, d1, &log.heads()).unwrap();
    assert!(early.keep.contains(x.id()), "`E` keeps `x` for `y`'s carry");
    share_at(&mut log, A, &k.body, d1 + HOUR);
    share_at(&mut log, B, &k.body, d1 + 2 * HOUR);
    if early_in_late {
        share_at(&mut log, A, &early.body, d1 + 3 * HOUR);
        share_at(&mut log, B, &early.body, d1 + 4 * HOUR);
    }
    held!(log, one(draft(&log, A, d1 + 5 * HOUR, name("a2")), A));
    let d2 = d1 + 2 * DAY_MS;
    let late = plan_at(&log, d2, &log.heads()).unwrap();
    assert!(
        late.dropped.contains(x.id()),
        "`L`'s cut holds `K` settled, so `L` drops `x`"
    );
    let shared_late: Vec<&Body> = if early_in_late {
        vec![&late.body]
    } else {
        vec![&early.body, &late.body]
    };
    for (n, body) in (0..).zip(shared_late) {
        share_at(&mut log, A, body, d2 + (2 * n + 1) * HOUR);
        share_at(&mut log, B, body, d2 + (2 * n + 2) * HOUR);
    }
    (log, k.body, early.body, late.body, *x.id())
}

/// A stub a later cut holds gone reads, in an earlier cut, as the whole log reads its record
/// there. In [`gone_between_cuts`] the log rooted at `L` holds `x` as a stub `L`'s cut held gone,
/// by `K`, whose settling shares lie past `E`'s cut: at `E`'s cut `x` counts, as in the whole
/// log, and `E` keeps it for `y`'s carry, so the rooted log computes `E`, judges it and holds it
/// settled as the whole log does, and proposes the whole log's next checkpoint. Where `L` follows
/// `E` its prune drops every share of `K`, which the rooted log still knows, its body read off a
/// dropped share's frame, and holds settled as the whole log does. In both, every stub of the
/// rooted log counts nowhere in it.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail:
/// - `Mutant::GoneFromTheLastCut` (a stub reads gone in every cut where a later prune's cut held
///   it so, as FL-148C's `gone` flag did): ``assertion `left == right` failed: the rooted log
///   judges `E` as the whole log (`E` shared past `L`'s cut)``, `left` `recomputed: false` against
///   `right` `recomputed: true`.
/// - `Mutant::ShareBodiesUnread` (no dropped share's body is read, as before this slice):
///   ``assertion `left == right` failed: the rooted log holds settled what the whole log does
///   (`E` shared within `L`'s cut)``, `left` one checkpoint against `right` three: without `K`'s
///   body `x` counts at `L`'s cut, `L` keeps it there, and the rooted log recomputes neither `K`
///   nor `L`.
#[test]
fn vault_checkpoint_a_stub_gone_at_an_earlier_cut_recomputes_as_the_whole_log() {
    for early_in_late in [false, true] {
        let which = if early_in_late {
            "`E` shared within `L`'s cut"
        } else {
            "`E` shared past `L`'s cut"
        };
        let (log, k, early, late, x) = gone_between_cuts(early_in_late);
        let rooted = log.adopt(&late).unwrap();
        assert!(rooted.is_dropped(&x));
        let k_shares_held = rooted
            .records()
            .any(|record| checkpoint_id(record.body()) == checkpoint_id(&k));
        assert_eq!(
            k_shares_held, !early_in_late,
            "`L`'s prune drops every share of `K` where it follows `E` ({which})"
        );
        let settled = |log: &Log| -> Vec<RecordId> {
            checkpoint::settled(log).iter().map(|s| s.id).collect()
        };
        assert_eq!(
            settled(&log).len(),
            3,
            "the whole log holds `K`, `E` and `L` settled ({which})"
        );
        assert_eq!(
            checkpoint::judge(&rooted, &early).unwrap(),
            checkpoint::judge(&log, &early).unwrap(),
            "the rooted log judges `E` as the whole log ({which})"
        );
        assert_eq!(
            settled(&rooted),
            settled(&log),
            "the rooted log holds settled what the whole log does ({which})"
        );
        let gone = checkpoint::gone(&rooted).unwrap_or_default();
        for id in rooted.rooting().unwrap().stubs.keys() {
            assert!(
                gone.contains(&rooted.position(id).unwrap()),
                "every stub counts nowhere in the rooted log ({which})"
            );
        }
        assert_eq!(evaluated(&rooted), evaluated(&log));
        let now = T + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
        let next = propose(&log, now).expect("a next checkpoint");
        assert_eq!(
            propose(&rooted, now).and_then(|plan| checkpoint_id(&plan.body)),
            checkpoint_id(&next.body),
            "the rooted log proposes the whole log's next checkpoint ({which})"
        );
    }
}

/// What [`vault_checkpoint_chains_of_cuts_read_each_stub_as_the_whole_log_over_random_dags`]
/// exercises, so the law is not met vacuously.
#[derive(Debug, Default)]
struct ChainCoverage {
    /// Chains of three settled cuts compared, and of them those whose last prune dropped every
    /// share of the first checkpoint, and those where A shared `K` inside `E`'s cut.
    chains: usize,
    first_shares_all_dropped: usize,
    minority: usize,
    /// Records the first checkpoint drops that the second keeps (no share of the first lies in
    /// its cut) and the third drops again (its cut holds the first settled).
    kept_between: usize,
    /// Version-1 ops in the second cut over an op the second keeps and the third drops.
    version_one_between: usize,
    /// Rooted logs whose next proposal was compared with the whole log's.
    next_proposals: usize,
    /// Logs pruned at several cuts of a chain in turn (on the whole log, and as each cut came),
    /// unions of the Macs' own prunes, and Macs at `K`'s cut caught up from a peer at `L`.
    successive: usize,
    unions: usize,
    caught_up: usize,
}

/// Over the seeded random vaults of
/// [`vault_checkpoint_pruning_preserves_the_evaluated_state_over_random_dags`], a chain of three
/// cuts: `K` at the first, `E` at a later one before any Mac but A shares `K` (A shares it inside
/// `E`'s cut in every other seed, a minority), and then, every Mac having shared `K` (and, in
/// every other run, `E`), `L` at a third, which every Mac shares (and `E` with it, where it did
/// not before; A shares `E` inside `L`'s cut in every other seed). So `L`'s cut holds `K`
/// settled, and drops again what `K` drops that `E`, whose cut holds `K` unsettled, keeps (B's
/// undo share of A's removal of C names a cutoff A superseded before `K`'s cut, which `E` keeps
/// and `K` drops); and, where `E` is settled in `L`'s cut, `L` follows `E` and drops every share
/// of `K`; and a version-1 op in `E`'s cut over an op `E` keeps and `L` drops reads, at `E`'s
/// cut, a frontier with that op in the whole log and without it in a log pruned at `L`, which no
/// plan reads (`rooted.rs`'s "A version-1 op beside a cut"). Each of these logs holds settled
/// the checkpoints the whole log does, judges `E` as it
/// does, holds every stub counting nowhere, evaluates as it does, and proposes its next
/// checkpoint: the whole log rooted at `L`; the whole log pruned at `K`, `E` and `L` in turn, at
/// `K` and `L`, and at `E` and `L`; and a Mac that pruned at each cut as it came, at `K` on the
/// log of `K`'s proposal, at `E` on that merged with the log of `E`'s, and at `L` on that merged
/// with the whole log. Each Mac pruning its own log at `L` (A and C at `K` first) and the three
/// merging hold what the whole log rooted at `L` holds, and a Mac that holds `K`'s cut catches
/// up from the log rooted at `L` and from the one pruned at `K`, `E` and `L`, holding what that
/// peer holds and evaluating as the whole log.
///
/// NEGATIVE CONTROLS, each run from the test's first line and seen to fail (record ids hold
/// random salts, so the failing seed may differ from run to run):
/// - `Mutant::GoneFromTheLastCut` (a stub reads gone in every cut where a later prune's cut
///   held it so, as FL-148C's `gone` flag did): ``assertion `left == right` failed: seed 1
///   (`E` settled in `L`'s cut, at `L`): the rooted log holds settled what the whole log
///   does``; with the check of the log rooted at `L` skipped, the same at ``seed 1 (`E`
///   settled in `L`'s cut, `K`, `E`, `L` in turn)``.
/// - `Mutant::ShareBodiesUnread` (no dropped share's body is read): the same two failures.
/// - `Mutant::HeldNesting` (a plan reads a checkpoint as nested in the newest final one's cut
///   among the held records alone, as before this slice, so it keeps every share of one whose
///   head a prune dropped): ``assertion `left == right` failed: seed 1 (`E` settled past `L`'s
///   cut, at `L`): the rooted log proposes the whole log's next checkpoint``; with that check
///   skipped, the same at ``seed 1 (`E` settled past `L`'s cut, `K`, `E`, `L` in turn)``.
/// - `Mutant::StubsDroppedForGood` (a stub is superseded for good), with the log rooted at `L`
///   unchecked: ``seed 1 (`E` settled past `L`'s cut, `K`, `E`, `L`): a prune in turn: this log
///   computes another checkpoint at those heads``.
/// - `Mutant::HeldProposal` (a checkpoint is computed from the held records alone): ``seed 1
///   (`E` settled past `L`'s cut): a prune at `E` as it comes: this log computes another
///   checkpoint at those heads``.
/// - `Mutant::NoEarlierRoots` (a prune keeps no root it nests): ``assertion `left == right`
///   failed: seed 2 (`E` settled past `L`'s cut): a Mac at `K` holds what its peer `K`, `E`,
///   `L` in turn holds``, the records each holds differing.
/// - `Mutant::KeepNestedRoots` (a prune keeps every root it nests among its roots): ``seed 1
///   (`E` settled past `L`'s cut): the union of the Macs' prunes is the prune of the union``.
#[test]
fn vault_checkpoint_chains_of_cuts_read_each_stub_as_the_whole_log_over_random_dags() {
    use crate::membership_log::served::tests::{catch_up, holding};
    let mut coverage = ChainCoverage::default();
    let settled =
        |log: &Log| -> Vec<RecordId> { checkpoint::settled(log).iter().map(|s| s.id).collect() };
    let cut_ms = |body: &Body| match body {
        Body::Checkpoint { cut_ms, .. } => *cut_ms,
        _ => panic!("a checkpoint"),
    };
    let later = |world: &World| world.clock + MIN_AGE_MS + budgets::WINDOW_SKEW_MS + HOUR;
    for seed in 1..=SEEDS {
        let minority = seed % 2 == 1;
        for early_in_late in [false, true] {
            let phase = if early_in_late {
                "`E` settled in `L`'s cut"
            } else {
                "`E` settled past `L`'s cut"
            };
            let mut world = random_vault(seed, 100, 30);
            let Some(k) = propose(&world.whole(), NOW) else {
                continue;
            };
            let at_k = world.whole();
            let k_then = at_k.rooted_at(&k.body).unwrap_or_else(|e| {
                panic!("seed {seed} ({phase}): a prune at `K` as it comes: {e}")
            });
            world.run(seed, 120);
            if minority {
                world.share(0, &k.body);
            }
            let Some(early) = propose(&world.whole(), later(&world)) else {
                continue;
            };
            let mut e_then = k_then;
            e_then.merge(&world.whole()).unwrap();
            let e_then = e_then.rooted_at(&early.body).unwrap_or_else(|e| {
                panic!("seed {seed} ({phase}): a prune at `E` as it comes: {e}")
            });
            // `K`'s shares, A's apart, lie past `E`'s cut.
            world.clock = world.clock.max(cut_ms(&early.body) + HOUR);
            world.settle(&k.body);
            if early_in_late {
                world.settle(&early.body);
            } else if minority {
                world.share(0, &early.body);
            }
            world.run(seed + 1000, 120);
            let Some(late) = propose(&world.whole(), later(&world)) else {
                continue;
            };
            world.clock = world.clock.max(cut_ms(&late.body) + HOUR);
            if !early_in_late {
                world.settle(&early.body);
            }
            world.settle(&late.body);
            let whole = world.whole();
            let ids = [&k, &early, &late].map(|plan| checkpoint_id(&plan.body).unwrap());
            if !ids.iter().all(|id| settled(&whole).contains(id)) {
                // No seat was heard in the month before one of the cuts: no Mac prunes there.
                continue;
            }
            let rooted = whole
                .rooted_at(&late.body)
                .unwrap_or_else(|e| panic!("seed {seed} ({phase}): a prune at `L`: {e}"));
            coverage.chains += 1;
            coverage.minority += usize::from(minority);
            coverage.first_shares_all_dropped += usize::from(
                !rooted
                    .records()
                    .any(|record| checkpoint_id(record.body()) == Some(ids[0])),
            );
            coverage.kept_between += late
                .dropped
                .iter()
                .filter(|id| k.dropped.contains(id) && early.keep.contains(*id))
                .count();
            // A version-1 op in `E`'s cut over an op `E` keeps and `L` drops: `E`'s cut does not
            // hold `L` settled, so the whole log reads its frontier there with that op and the
            // rooted log without it, and `E` computes alike only as no plan reads that frontier.
            let early_heads = heads_of(&early.body);
            let kept_by_early: Vec<&RecordId> = late
                .dropped
                .iter()
                .filter(|id| early.keep.contains(*id))
                .collect();
            coverage.version_one_between += rooted
                .records()
                .filter(|r| r.version() == 1 && matches!(r.kind(), Kind::Put | Kind::Delete))
                .filter(|r| whole.in_cut_of(&early_heads, r.id()))
                .filter(|r| kept_by_early.iter().any(|id| whole.reaches(&[*r.id()], id)))
                .count();
            let now = later(&world) + DAY_MS;
            let next = propose(&whole, now).map(|plan| checkpoint_id(&plan.body));
            coverage.next_proposals += usize::from(next.is_some());
            let as_whole = |log: &Log, which: &str| {
                assert_eq!(
                    settled(log),
                    settled(&whole),
                    "seed {seed} ({phase}, {which}): the rooted log holds settled what the whole \
                     log does"
                );
                assert_eq!(
                    checkpoint::judge(log, &early.body).unwrap(),
                    checkpoint::judge(&whole, &early.body).unwrap(),
                    "seed {seed} ({phase}, {which}): the rooted log judges `E` as the whole log"
                );
                let gone = checkpoint::gone(log).unwrap_or_default();
                assert!(
                    log.rooting()
                        .unwrap()
                        .stubs
                        .keys()
                        .all(|id| gone.contains(&log.position(id).unwrap())),
                    "seed {seed} ({phase}, {which}): every stub counts nowhere in the rooted log"
                );
                assert_evaluates_as(seed, &format!("{phase}, {which}"), log, &whole);
                assert_eq!(
                    propose(log, now).map(|plan| checkpoint_id(&plan.body)),
                    next,
                    "seed {seed} ({phase}, {which}): the rooted log proposes the whole log's next \
                     checkpoint"
                );
            };
            as_whole(&rooted, "at `L`");

            let in_turn = |bodies: &[&Body], which: &str| {
                bodies.iter().fold(whole.clone(), |log, body| {
                    log.rooted_at(body).unwrap_or_else(|e| {
                        panic!("seed {seed} ({phase}, {which}): a prune in turn: {e}")
                    })
                })
            };
            let k_e_l = in_turn(&[&k.body, &early.body, &late.body], "`K`, `E`, `L`");
            as_whole(&k_e_l, "`K`, `E`, `L` in turn");
            let k_l = in_turn(&[&k.body, &late.body], "`K`, `L`");
            as_whole(&k_l, "`K`, `L` in turn");
            let e_l = in_turn(&[&early.body, &late.body], "`E`, `L`");
            as_whole(&e_l, "`E`, `L` in turn");
            let mut l_then = e_then;
            l_then.merge(&whole).unwrap();
            let l_then = l_then.rooted_at(&late.body).unwrap_or_else(|e| {
                panic!("seed {seed} ({phase}): a prune at `L` as it comes: {e}")
            });
            as_whole(&l_then, "each cut as it came");
            coverage.successive += 4;

            let mut union: Option<Log> = None;
            for (m, log) in world.logs.iter().enumerate() {
                let bodies: &[&Body] = if m == 1 {
                    &[&late.body]
                } else {
                    &[&k.body, &late.body]
                };
                let own = bodies.iter().fold(log.clone(), |log, body| {
                    log.rooted_at(body).unwrap_or_else(|e| {
                        panic!("seed {seed} ({phase}): Mac {m} prunes its own log: {e}")
                    })
                });
                match union.as_mut() {
                    None => union = Some(own),
                    Some(union) => union.merge(&own).unwrap(),
                }
            }
            let union = union.unwrap().rooted_at(&late.body).unwrap();
            assert!(
                same_log(&union, &rooted),
                "seed {seed} ({phase}): the union of the Macs' prunes is the prune of the union"
            );
            coverage.unions += 1;

            for (which, peer) in [("at `L`", &rooted), ("`K`, `E`, `L` in turn", &k_e_l)] {
                let receiver = catch_up(peer, &at_k).unwrap_or_else(|e| {
                    panic!("seed {seed} ({phase}): a Mac at `K` catches up from {which}: {e}")
                });
                let log = receiver.log().unwrap();
                assert_eq!(
                    holding(log),
                    holding(peer),
                    "seed {seed} ({phase}): a Mac at `K` holds what its peer {which} holds"
                );
                assert_evaluates_as(seed, &format!("{phase}, caught up {which}"), log, &whole);
                coverage.caught_up += 1;
            }
        }
    }
    eprintln!("{coverage:?}");
    assert!(
        coverage.chains * 2 >= usize::try_from(SEEDS).unwrap(),
        "{coverage:?}"
    );
    assert!(
        coverage.first_shares_all_dropped > 0
            && coverage.kept_between > 0
            && coverage.version_one_between > 0
            && coverage.minority > 0,
        "{coverage:?}"
    );
    assert!(coverage.next_proposals > 0, "{coverage:?}");
    assert_eq!(
        (coverage.successive, coverage.unions, coverage.caught_up),
        (4 * coverage.chains, coverage.chains, 2 * coverage.chains),
        "{coverage:?}"
    );
}

/// What C, waking on the log it took when it held A's first `Put` of `a.key` alone, writes
/// beside the cut of [`final_cut_over_a_superseded_put`], whose prune dropped that `Put`: a
/// version-1 `Put` of the value it read (a reseal, a carry where that `Put` counts), of another
/// value, or a version-1 `Delete`.
#[derive(Clone, Copy, Debug)]
enum Beside {
    Reseal,
    Edit,
    Delete,
}

/// A version-1 op beside a final cut over an op its prune dropped reads as the whole log reads
/// it, whether the rooted log took it before its prune or after. It states nothing, and reads by
/// the counting ops of its frontier; the cut is settled in every log that holds its shares, so
/// the dropped op counts nowhere there, the whole log's frontier lacks it as the rooted log's
/// does ([`checkpoint::gone`]), and C's op is an edit beside A's second `Put` in both: `a.key`
/// reads in conflict, where C's reseal would have carried a value that counts. That is the cost
/// of the rule, alike in every log; a rooted log refuses no such record, which an honest Mac
/// that slept across the cut may write.
///
/// NEGATIVE CONTROL: `Mutant::Revives` (a record a settled prune dropped counts as its
/// evaluation alone says), run from the test's first line and seen to fail with
/// ``assertion `left == right` failed: the rooted log reads `a.key`'s conflicts as the whole log
/// (Reseal, taken after the prune)``, `left: {"a.key"}` against `right: {}`: the whole log reads
/// C's reseal by a frontier that holds A's first `Put`, as a carry its second replaced, and the
/// rooted log by one that lost it, as an edit.
#[test]
fn vault_checkpoint_a_version_one_op_beside_a_cut_reads_as_the_whole_log() {
    for beside in [Beside::Reseal, Beside::Edit, Beside::Delete] {
        for after in [true, false] {
            let when = if after {
                "taken after the prune"
            } else {
                "held before the prune"
            };
            let (mut whole, body, first, stale) = final_cut_over_a_superseded_put();
            let keys = stale.unwrap_epochs(&C.identity()).unwrap();
            let epoch = stale.newest_epochs(&EpochAudience::Everyone)[0];
            let key = &keys[&epoch];
            let at = T + 3 * DAY_MS;
            let drafted = match beside {
                Beside::Reseal | Beside::Edit => {
                    let value: &[u8] = if matches!(beside, Beside::Reseal) {
                        b"one"
                    } else {
                        b"c's"
                    };
                    stale.draft_put(
                        &C.id(),
                        at,
                        epoch,
                        key,
                        &entry(&EpochAudience::Everyone, "a.key", value),
                    )
                }
                Beside::Delete => stale.draft_delete(&C.id(), at, epoch, key, "a.key"),
            };
            let record = one(drafted.unwrap(), C);
            assert_eq!(record.version(), 1);
            assert!(
                record.parents().contains(&first),
                "C's op follows A's first `Put`"
            );
            let rooted = if after {
                let mut rooted = whole.adopt(&body).unwrap();
                hold(&mut whole, record.clone());
                assert_eq!(rooted.insert(record).unwrap(), Inserted::Held);
                rooted
            } else {
                hold(&mut whole, record);
                whole.adopt(&body).unwrap()
            };
            assert!(rooted.is_dropped(&first));
            assert_eq!(
                conflicted(&rooted, B),
                conflicted(&whole, B),
                "the rooted log reads `a.key`'s conflicts as the whole log ({beside:?}, {when})"
            );
            assert_eq!(
                conflicted(&whole, B),
                BTreeSet::from(["a.key".to_owned()]),
                "C's op is an edit beside A's second `Put` ({beside:?}, {when})"
            );
            assert_eq!(values(&rooted, B), values(&whole, B));
            assert_eq!(evaluated(&rooted), evaluated(&whole));
            assert_eq!(
                evaluated_registers(&rooted, &BTreeSet::new()),
                evaluated_registers(&whole, &BTreeSet::new()),
                "the rooted log reads its registers as the whole log ({beside:?}, {when})"
            );
        }
    }
}
