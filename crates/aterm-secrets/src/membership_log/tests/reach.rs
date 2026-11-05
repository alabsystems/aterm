// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The log's reachability and seat indexes (`membership_log/reach.rs`) against the walks they
//! replaced: a log shaped like ALab's at 4,071 records, every hand-made and generated log of the
//! weighing tests, and probe records signed against each, with reload work, clone sharing and
//! byte counts pinned by count (never timed).

use super::super::reach::tests::{Rng, disagreements, walked};
use super::super::reach::{MUTANT, Mutant, Reach, reserved_for};
use super::weigh::{corpus, generated_logs};
use super::*;

/// Every held record in or reachable from `roots`, by walking parents: the walk the index
/// replaced in `Log::ancestors_of`.
fn walked_ancestors(log: &Log, roots: &[RecordId]) -> BTreeSet<RecordId> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<RecordId> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if log.held.contains_key(&id)
            && seen.insert(id)
            && let Some(parent) = log.get(&id)
        {
            stack.extend(parent.parents().iter().copied());
        }
    }
    seen
}

/// Whether the held record `ancestor` is in `record`'s causal past, by the walk the index
/// replaced in `Log::precedes`.
fn walked_precedes(log: &Log, ancestor: &RecordId, record: &Record) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&RecordId> = record.parents().iter().collect();
    while let Some(id) = stack.pop() {
        if id == ancestor {
            return true;
        }
        if seen.insert(*id)
            && let Some(parent) = log.get(id)
        {
            stack.extend(parent.parents().iter());
        }
    }
    false
}

/// Whether `actor` holds a seat anywhere in `log`, by the scan the seat index replaced in
/// `Log::holds_seat`.
fn scanned_holds_seat(log: &Log, actor: &MemberId) -> bool {
    log.order
        .iter()
        .chain(log.waiting.values())
        .any(|r| match r.body() {
            Body::Genesis { founders, .. } => founders.iter().any(|f| f.member() == Some(*actor)),
            Body::Admit { entrant, .. } => {
                r.actor() != &entrant.member() && entrant.member() == *actor
            }
            _ => false,
        })
}

/// Whether `record`'s signer is a bound founder or admitted in its causal past, by the walk the
/// seat index replaced in `Log::has_seat_in_past`.
fn walked_seat_in_past(log: &Log, record: &Record) -> bool {
    let actor = record.actor();
    if let Body::Genesis { founders, .. } = log.genesis().body()
        && founders.iter().any(|f| f.member() == Some(*actor))
    {
        return true;
    }
    walked_ancestors(log, record.parents()).iter().any(|id| {
        log.get(id).is_some_and(|parent| {
            matches!(
                parent.body(),
                Body::Admit { entrant, .. }
                    if parent.actor() != &entrant.member() && entrant.member() == *actor
            )
        })
    })
}

/// `Log::check_against_parents` as it stood before the index, walks and all: the verdict every
/// check now reads from the index must equal.
fn walked_check(log: &Log, record: &Record) -> Result<()> {
    if log.enforce_seats && !walked_seat_in_past(log, record) {
        return Err(Error::new(
            "a membership record's signer has no seat in its causal past",
        ));
    }
    if let Some(previous) = record.previous() {
        let before = log.get(previous).filter(|p| p.kind() != Kind::Genesis);
        if !before
            .is_some_and(|p| p.actor() == record.actor() && p.sequence() + 1 == record.sequence())
        {
            return Err(Error::new(
                "a record's previous is its signer's record at the sequence before",
            ));
        }
    }
    if let Body::Admit { entrant, evidence } = record.body() {
        let Body::Genesis { root, founders, .. } = log.genesis().body() else {
            unreachable!("a log starts at a genesis");
        };
        let binding = entrant.request.binding();
        if !root.names(binding) {
            return Err(Error::new(
                "an entrant's binding names its network's Commons root",
            ));
        }
        if *evidence == Evidence::Listed
            && !founders.iter().any(|f| {
                f.binding.is_none()
                    && f.recipient == binding.recipient()
                    && f.label == entrant.label
            })
        {
            return Err(Error::new(
                "a listed admission names a founder the genesis lists with no binding, under its label",
            ));
        }
    }
    if let Body::Remove { member, cutoff } = record.body() {
        if !walked_precedes(log, cutoff, record) {
            return Err(Error::new(
                "a removal's cutoff is a record in its own causal past",
            ));
        }
        let names_it = log.get(cutoff).is_some_and(|at| match at.body() {
            Body::Genesis { founders, .. } => founders.iter().any(|f| f.member() == Some(*member)),
            Body::Admit { entrant, .. } => at.actor() == member || entrant.member() == *member,
            _ => at.actor() == member,
        });
        if !names_it {
            return Err(Error::new(
                "a removal's cutoff is the removed Mac's record or the one that let it in",
            ));
        }
    }
    match record.body() {
        Body::Wrap { epoch, .. }
            if !walked_precedes(log, epoch, record)
                || !matches!(log.get(epoch).map(Record::body), Some(Body::Epoch { .. })) =>
        {
            return Err(Error::new("a Wrap names an Epoch in its causal past"));
        }
        Body::Put {
            audience, epoch, ..
        }
        | Body::Delete {
            audience, epoch, ..
        } if !walked_precedes(log, epoch, record)
            || !matches!(
                log.get(epoch).map(Record::body),
                Some(Body::Epoch { audience: ep_aud, .. }) if ep_aud == audience
            ) =>
        {
            return Err(Error::new(
                "a Put or Delete names an Epoch for its audience in its causal past",
            ));
        }
        _ => {}
    }
    Ok(())
}

/// `Store::missing` as it stood before the index: every held record outside the walked closure
/// of `have`, in order.
fn walked_missing(log: &Log, have: &[RecordId]) -> Vec<RecordId> {
    let known = walked_ancestors(log, have);
    log.order
        .iter()
        .map(|r| r.id)
        .filter(|id| !known.contains(id))
        .collect()
}

/// A Mac a probe signs as, its id and recipient kept, since each derives a request.
struct Signer {
    mac: Mac,
    id: MemberId,
    recipient: crate::age::Recipient,
}

/// The Macs a probe signs as: every founder, entrant and Sybil tag the fixtures use, seated or
/// not.
fn signers() -> &'static [Signer] {
    static SIGNERS: OnceLock<Vec<Signer>> = OnceLock::new();
    SIGNERS.get_or_init(|| {
        (1..=60)
            .chain(100..=140)
            .map(|t| Signer {
                mac: Mac(t),
                id: Mac(t).id(),
                recipient: Mac(t).identity().recipient(),
            })
            .collect()
    })
}

/// The verdicts of every probe kind: how many checks held and how many were refused.
#[derive(Debug, Default)]
struct Verdicts {
    held: BTreeMap<&'static str, usize>,
    refused: BTreeMap<&'static str, usize>,
    disagreements: usize,
}

impl Verdicts {
    /// Check `record` against `log` both ways, recording a disagreement and the verdict.
    fn check(&mut self, log: &Log, record: &Record, what: &'static str) {
        let now = log.check_against_parents(record).map_err(|e| e.to_string());
        let then = walked_check(log, record).map_err(|e| e.to_string());
        if now != then {
            self.disagreements += 1;
        }
        let tally = if then.is_ok() {
            &mut self.held
        } else {
            &mut self.refused
        };
        *tally.entry(what).or_default() += 1;
    }
}

/// A record by `by` at sequence one that follows the held records at `parents`, whatever its
/// body needs: a probe of the checks, never inserted.
fn probe(log: &Log, by: Mac, parents: Vec<RecordId>, body: Body) -> Record {
    Draft {
        network: *log.network(),
        sequence: 1,
        previous: None,
        parents,
        authored_ms: 7,
        body,
    }
    .sign(&by.seed(), by.incarnation())
    .unwrap()
}

/// Check `count` probes against `log`, and with `held` every record it holds, with seats
/// enforced and not, and compare every Mac's seat and `count` answers of `Log::reaches` with the
/// scan and walk they replaced.
fn check_both_ways(log: &Log, held: bool, count: usize, rng: &mut Rng, verdicts: &mut Verdicts) {
    let mut enforcing = log.clone();
    enforcing.require_seats();
    let epochs: Vec<usize> = (0..log.order.len())
        .filter(|&i| log.order[i].kind() == Kind::Epoch)
        .collect();
    let signers = signers();
    for view in [log, &enforcing] {
        if held {
            for record in view.order.iter().skip(1) {
                verdicts.check(view, record, "held");
            }
        }
        for _ in 0..count {
            let by = &signers[rng.below(signers.len())];
            let n = view.order.len();
            let parents: Vec<RecordId> = (0..1 + rng.below(3))
                .map(|_| view.order[rng.below(n)].id)
                .collect();
            let target = &view.order[rng.below(n)];
            let (what, body) = match rng.below(4) {
                0 => (
                    "name",
                    Body::Name {
                        name: "probe".into(),
                    },
                ),
                1 => {
                    let member = match target.body() {
                        Body::Genesis { founders, .. } => {
                            founders.iter().find_map(Founder::member).unwrap_or(by.id)
                        }
                        Body::Admit { entrant, .. } if rng.below(2) == 0 => entrant.member(),
                        _ => *target.actor(),
                    };
                    let cutoff = target.id;
                    ("remove", Body::Remove { member, cutoff })
                }
                kind => {
                    let epoch = if epochs.is_empty() || rng.below(10) == 0 {
                        target.id
                    } else {
                        view.order[epochs[rng.below(epochs.len())]].id
                    };
                    if kind == 2 {
                        let recipient = by.recipient;
                        let capsule = vec![7; 64].into();
                        let body = Body::Wrap {
                            epoch,
                            recipient,
                            capsule,
                        };
                        ("wrap", body)
                    } else {
                        let body = Body::Put {
                            audience: EpochAudience::Everyone,
                            epoch,
                            salt: [1; 32],
                            ciphertext: vec![9; 32].into(),
                        };
                        ("put", body)
                    }
                }
            };
            verdicts.check(view, &probe(view, by.mac, parents, body), what);
        }
    }
    for by in signers {
        if log.holds_seat(&by.id) != scanned_holds_seat(log, &by.id) {
            verdicts.disagreements += 1;
        }
    }
    for _ in 0..count {
        let n = log.order.len();
        let from: Vec<RecordId> = (0..1 + rng.below(2))
            .map(|_| log.order[rng.below(n)].id)
            .collect();
        let target = log.order[rng.below(n)].id;
        if log.reaches(&from, &target) != walked_ancestors(log, &from).contains(&target) {
            verdicts.disagreements += 1;
        }
    }
}

/// Feed `log`'s records newest first to a seat-enforcing log of its genesis, so records whose
/// signer holds a seat wait for their parents, then let the waiting go one by one; after each
/// step every Mac's seat matches the scan, waiting `Admit`s and dropped ones included. Returns
/// the disagreements and how many records waited at most.
fn seats_while_waiting(log: &Log) -> (usize, usize) {
    let signers = signers();
    let mut wrong = 0;
    let mut most = 0;
    let mut late = Log::new(log.genesis().clone()).unwrap();
    late.require_seats();
    let compare = |late: &Log| {
        signers
            .iter()
            .filter(|by| late.holds_seat(&by.id) != scanned_holds_seat(late, &by.id))
            .count()
    };
    for record in log.order.iter().skip(1).rev() {
        let _ = late.insert(record.clone());
        most = most.max(late.waiting());
        wrong += compare(&late);
    }
    while late.drop_last_waiting() {
        wrong += compare(&late);
    }
    (wrong, most)
}

/// Mac `t` of the live shape, its id and recipient kept, since each derives a request.
struct LiveMac {
    mac: Mac,
    id: MemberId,
    recipient: crate::age::Recipient,
}

/// Builds the live-shape log batch by batch ([`live_shape`]).
struct Live {
    log: Log,
    rng: Rng,
    macs: Vec<LiveMac>,
    /// Each Mac's latest record and its sequence.
    previous: BTreeMap<u8, (RecordId, u64)>,
    /// The `Admit` that seats each admitted Mac.
    seat: BTreeMap<u8, RecordId>,
    /// The heads as the previous batch started, which a batch signed against an older view
    /// follows instead of the current ones.
    older: Vec<RecordId>,
    batch: bool,
}

impl Live {
    fn mac(&self, t: u8) -> &LiveMac {
        &self.macs[usize::from(t) - 1]
    }

    /// Start a batch: its first record follows the heads (one batch in five an older view of
    /// them, so a head forks off and a later batch merges it back).
    fn start(&mut self) {
        self.batch = false;
    }

    /// Sign and hold `body` by Mac `t`, after its previous record; the first of a batch also
    /// follows the heads, and every record also follows its seat and the record its body names
    /// when what it follows does not reach them.
    fn sign(&mut self, t: u8, body: Body) -> RecordId {
        let previous = self.previous.get(&t).copied();
        let mut parents: Vec<RecordId> = previous.map(|(id, _)| id).into_iter().collect();
        if !self.batch {
            let heads = self.log.heads();
            if self.rng.below(5) == 0 && !self.older.is_empty() {
                parents.extend(self.older.iter().copied());
            } else {
                parents.extend(heads.iter().copied());
            }
            self.older = heads;
            self.batch = true;
        }
        let named = match &body {
            Body::Wrap { epoch, .. } | Body::Put { epoch, .. } => Some(*epoch),
            Body::Remove { cutoff, .. } => Some(*cutoff),
            _ => None,
        };
        for needed in [self.seat.get(&t).copied(), named].into_iter().flatten() {
            if !self.log.reaches(&parents, &needed) {
                parents.push(needed);
            }
        }
        let mac = self.mac(t).mac;
        let record = Draft {
            network: *self.log.network(),
            sequence: previous.map_or(1, |(_, s)| s + 1),
            previous: previous.map(|(id, _)| id),
            parents,
            authored_ms: 1_000,
            body,
        }
        .sign(&mac.seed(), mac.incarnation())
        .unwrap();
        let id = *self.log.keep(record).id();
        let sequence = previous.map_or(1, |(_, s)| s + 1);
        self.previous.insert(t, (id, sequence));
        id
    }

    fn node(&mut self, t: u8) {
        self.start();
        let id = self.mac(t).id;
        let proof = node_pair(t).sign(&node_proof_message(self.log.network(), &id));
        let body = Body::Node {
            node_key: node_key(t),
            proof: proof.as_ref().try_into().unwrap(),
        };
        self.sign(t, body);
    }
}

/// How many Macs ALab's genesis binds in the live shape, and how many it holds in all.
const LIVE_FOUNDERS: u8 = 26;
const LIVE_MACS: u8 = 53;
/// The Macs the live shape's one remover takes out near its end, as m22 took out eight.
const LIVE_REMOVED: std::ops::RangeInclusive<u8> = 46..=53;

/// A log shaped like ALab's on 2026-11-03 (`map-evaluator`'s census of the live file): 4,071
/// records, Genesis 1, Admit 27, Remove 8, Node 53, Epoch 81, Wrap 3,352 and Put 549, by 53
/// signers, seats enforced. It grows in batches by one Mac at a time, each a chain on its
/// signer's previous record whose first record follows the heads (sometimes an older view of
/// them, which forks a head that a later batch merges), as rotations, admissions and writes
/// do. Wrap capsules and Put ciphertexts are filler: nothing here opens them.
pub(super) fn live_shape() -> &'static Log {
    static LIVE: OnceLock<Log> = OnceLock::new();
    LIVE.get_or_init(|| {
        let macs: Vec<LiveMac> = (1..=LIVE_MACS)
            .map(|t| {
                let mac = Mac(t);
                LiveMac {
                    mac,
                    id: mac.id(),
                    recipient: mac.identity().recipient(),
                }
            })
            .collect();
        let labels: Vec<(Mac, String)> = (1..=LIVE_FOUNDERS)
            .map(|t| (Mac(t), format!("m{t}")))
            .collect();
        let founders: Vec<(Mac, &str)> = labels.iter().map(|(m, l)| (*m, l.as_str())).collect();
        let mut log = found(&founders);
        log.require_seats();
        let mut live = Live {
            log,
            rng: Rng(4_071),
            macs,
            previous: BTreeMap::new(),
            seat: BTreeMap::new(),
            older: Vec::new(),
            batch: false,
        };
        for t in 1..=LIVE_FOUNDERS {
            live.node(t);
        }
        let mut seated: Vec<u8> = (1..=LIVE_FOUNDERS).collect();
        let mut generation = 0;
        for round in 0..81usize {
            if let Some(t) = u8::try_from(round)
                .ok()
                .map(|r| r + LIVE_FOUNDERS + 1)
                .filter(|&t| t <= LIVE_MACS)
            {
                let by = seated[live.rng.below(seated.len())];
                live.start();
                let entrant = Mac(t).entrant(&format!("m{t}"));
                let admit = live.sign(
                    by,
                    Body::Admit {
                        entrant,
                        evidence: Evidence::Phrase,
                    },
                );
                live.seat.insert(t, admit);
                live.node(t);
                seated.push(t);
            }
            if round == 70 {
                live.start();
                for t in LIVE_REMOVED {
                    let cutoff = live.previous[&t].0;
                    let member = live.mac(t).id;
                    live.sign(1, Body::Remove { member, cutoff });
                }
                seated.retain(|t| !LIVE_REMOVED.contains(t));
            }
            let by = seated[live.rng.below(seated.len())];
            generation += 1;
            live.start();
            let epoch = live.sign(
                by,
                Body::Epoch {
                    audience: EpochAudience::Everyone,
                    generation,
                    commitment: [u8::try_from(generation % 255).unwrap() + 1; 32],
                },
            );
            let wraps = if round < 31 { 42 } else { 41 };
            for w in 0..wraps {
                let recipient = live.macs[w % usize::from(LIVE_MACS)].recipient;
                let capsule = vec![0x5a; 560].into();
                live.sign(
                    by,
                    Body::Wrap {
                        epoch,
                        recipient,
                        capsule,
                    },
                );
            }
            let mut puts = if round < 63 { 7 } else { 6 };
            while puts > 0 {
                let writer = seated[live.rng.below(seated.len())];
                live.start();
                for _ in 0..(1 + live.rng.below(3)).min(puts) {
                    live.sign(
                        writer,
                        Body::Put {
                            audience: EpochAudience::Everyone,
                            epoch,
                            salt: [3; 32],
                            ciphertext: vec![0xc3; 2_400].into(),
                        },
                    );
                    puts -= 1;
                }
            }
        }
        live.log
    })
}

/// The live shape holds the census's kind mix, signers and fan-in: 4,071 records, at most 9
/// parents on one record, and parent edges within 11% of the live file's 4,241.
#[test]
fn the_live_shape_holds_the_census_mix() {
    let live = live_shape();
    let mut kinds: BTreeMap<Kind, usize> = BTreeMap::new();
    for record in live.records() {
        *kinds.entry(record.kind()).or_default() += 1;
    }
    assert_eq!(
        kinds,
        BTreeMap::from([
            (Kind::Genesis, 1),
            (Kind::Admit, 27),
            (Kind::Remove, 8),
            (Kind::Epoch, 81),
            (Kind::Wrap, 3_352),
            (Kind::Put, 549),
            (Kind::Node, 53),
        ])
    );
    let signers: BTreeSet<_> = live.records().skip(1).map(Record::actor).collect();
    assert_eq!(signers.len(), usize::from(LIVE_MACS));
    let edges: usize = live.records().map(|r| r.parents().len()).sum();
    let widest = live.records().map(|r| r.parents().len()).max().unwrap();
    eprintln!(
        "live shape: {} records, {edges} parent edges, widest {widest}",
        live.held_count()
    );
    assert!(
        edges.abs_diff(4_241) * 100 <= 11 * 4_241,
        "{edges} parent edges"
    );
    assert!(widest <= 9, "{widest} parents on one record");
    assert_eq!(
        live.membership().members().len(),
        usize::from(LIVE_MACS) - 8
    );
}

/// Each held record's parents in `log`, by index.
fn parents_of(log: &Log) -> Vec<Vec<usize>> {
    log.order
        .iter()
        .map(|r| r.parents().iter().map(|p| log.held[p]).collect())
        .collect()
}

/// On the live shape the log's own index answers what a depth-first walk of its records'
/// parents answers, and its rows take the bytes the bound reserves for that many records.
#[test]
fn the_live_shape_index_matches_a_depth_first_walk() {
    let live = live_shape();
    let past = walked(&parents_of(live));
    assert_eq!(disagreements(&past, &live.reach, &mut Rng(4_241)), 0);
    assert_eq!(live.reach.bytes(), reserved_for(live.held_count()));
}

/// Reloading the live shape from its stream (which orders records afresh) ORs exactly each
/// parent's row into each record's, the sum over records of the sum over parents `j` of
/// `ceil(j / 64)` words, copies no chunk, and leaves the index exact; the store's footprint is
/// its stream's bytes and the bytes the bound reserves for that many records' index.
#[test]
fn a_live_shape_reload_costs_exactly_its_parents_row_words() {
    let live = live_shape();
    let mut store = Store::with_compiled(Some(*live.network()));
    let stream = encode_records(live.records());
    assert_eq!(store.ingest(&stream).unwrap(), live.held_count());
    assert_eq!(
        store.footprint(),
        stream.len() + reserved_for(live.held_count())
    );
    let log = store.log().unwrap();
    assert_eq!(log.held_count(), live.held_count());
    let words: usize = log
        .order
        .iter()
        .flat_map(|r| r.parents().iter().map(|p| log.held[p].div_ceil(64)))
        .sum();
    let edges: usize = log.order.iter().map(|r| r.parents().len()).sum();
    eprintln!("live reload: {edges} parent edges, {words} parent-row words");
    assert_eq!(log.reach.cost.word_ors, words);
    assert_eq!(log.reach.cost.chunk_copies, 0);
    assert_eq!(
        disagreements(&walked(&parents_of(log)), &log.reach, &mut Rng(9)),
        0
    );
    let built: usize = live
        .order
        .iter()
        .flat_map(|r| r.parents().iter().map(|p| live.held[p].div_ceil(64)))
        .sum();
    assert_eq!(
        live.reach.cost.word_ors, built,
        "and so does the build, in its own order"
    );
}

/// Every insert check (seats, a removal's cutoff, a Wrap's, Put's or Delete's epoch) gives the
/// verdict the walks it replaced gave: on every held record and on probe records with random
/// parents, bodies and signers, with seats enforced and not, over every hand-made log, 200
/// generated ones and the live shape; and so do every seat lookup, with records waiting and let
/// go, and every `Log::reaches` the drafts read. Both verdicts occur for every probe kind.
/// NEGATIVE CONTROLS: the same checks over a live-shape index rebuilt without each record's
/// first parent's row, or without its first parent's own bit, disagree with the walks.
#[test]
fn insert_checks_and_seats_give_the_verdicts_of_the_walks_they_replaced() {
    let mut verdicts = Verdicts::default();
    let mut rng = Rng(20_261_103);
    let mut waited = 0;
    let hand_made = corpus();
    let every_kind = [("every kind".to_owned(), every_kind().0)];
    let logs = hand_made.iter().chain(generated_logs()).chain(&every_kind);
    for (name, log) in logs {
        check_both_ways(log, true, 40, &mut rng, &mut verdicts);
        let (wrong, most) = seats_while_waiting(log);
        assert_eq!(wrong, 0, "{name}: seats while waiting");
        waited = waited.max(most);
        assert_eq!(verdicts.disagreements, 0, "{name}");
    }
    let live = live_shape();
    check_both_ways(live, true, 2_000, &mut rng, &mut verdicts);
    assert_eq!(verdicts.disagreements, 0, "the live shape");
    assert!(waited > 0, "some records waited");
    for what in ["held", "name", "remove", "wrap", "put"] {
        assert!(verdicts.held.get(what) > Some(&0), "no {what} probe held");
        if what != "held" {
            assert!(
                verdicts.refused.get(what) > Some(&0),
                "no {what} probe refused"
            );
        }
    }
    eprintln!("verdicts: {verdicts:?}");

    for mutant in [Mutant::OmitParentRow, Mutant::OmitParentBit] {
        let mut broken = live.clone();
        broken.reach = Reach::default();
        MUTANT.set(Some(mutant));
        for record in &live.order {
            broken
                .reach
                .push(record.parents().iter().map(|p| live.held[p]));
        }
        MUTANT.set(None);
        let mut wrong = Verdicts::default();
        check_both_ways(&broken, false, 500, &mut Rng(5), &mut wrong);
        assert!(wrong.disagreements > 0, "{mutant:?} goes unnoticed");
    }
}

/// `Store::missing` answers what its walk answered, and `Log::ancestor_closure` and
/// `Log::ancestor_chain` the walked closure, for random `have` lists (held ids and ids the log
/// never held) and targets on every hand-made log and the live shape.
#[test]
fn missing_records_and_closures_match_their_walks() {
    let mut rng = Rng(4_096);
    let mut logs: Vec<Log> = corpus().into_iter().map(|(_, log)| log).collect();
    logs.push(live_shape().clone());
    for log in &logs {
        let mut store = Store::with_compiled(Some(*log.network()));
        store.ingest(&encode_records(log.records())).unwrap();
        let held = store.log().unwrap();
        let n = held.order.len();
        for round in 0..24 {
            let mut have: Vec<RecordId> = (0..rng.below(4))
                .map(|_| held.order[rng.below(n)].id)
                .collect();
            if round % 3 == 0 {
                have.push([0xee; 32]);
            }
            let missing: Vec<RecordId> = store.missing(&have).iter().map(|r| r.id).collect();
            assert_eq!(missing, walked_missing(held, &have));
            let mut keep = walked_ancestors(held, &have);
            keep.extend(have.iter().filter(|id| held.held.contains_key(*id)));
            let want: Vec<RecordId> = held
                .order
                .iter()
                .map(|r| r.id)
                .filter(|id| keep.contains(id))
                .collect();
            let closure: Vec<RecordId> =
                held.ancestor_closure(&have).iter().map(|r| r.id).collect();
            assert_eq!(closure, want);
            let target = held.order[rng.below(n)].id;
            let chain: Vec<RecordId> = held
                .ancestor_chain(&target)
                .unwrap()
                .iter()
                .map(|r| r.id)
                .collect();
            let mut keep = walked_ancestors(held, &[target]);
            keep.insert(target);
            let want: Vec<RecordId> = held
                .order
                .iter()
                .map(|r| r.id)
                .filter(|id| keep.contains(id))
                .collect();
            assert_eq!(chain, want);
        }
        assert!(held.ancestor_chain(&[0xee; 32]).is_none());
    }
}

/// A log's clones share every chunk of its index, and the first record one holds copies at most
/// the one partial chunk it writes into; a lone log whose evaluation has run copies nothing when
/// it holds the next record, since `hold` lets go of that evaluation (which shares the chunks)
/// first.
#[test]
fn log_clones_share_index_chunks_and_an_append_copies_at_most_one() {
    let live = live_shape();
    let mut log = live.clone();
    let chunks = log.reach.chunks();
    assert!((0..chunks).all(|c| log.reach.shares_chunk(&live.reach, c)));
    let before = log.reach.cost;
    let name = sign(
        &log,
        A,
        Body::Name {
            name: "after".into(),
        },
    );
    log.keep(name);
    assert_eq!(log.reach.cost.chunk_copies, before.chunk_copies + 1);
    assert!(log.reach.cost.copied_words - before.copied_words <= 64 * chunks);
    assert!((0..chunks - 1).all(|c| log.reach.shares_chunk(&live.reach, c)));
    assert!(!log.reach.shares_chunk(&live.reach, chunks - 1));
    assert_eq!(live.held_count() + 1, log.held_count());

    let _ = log.effective();
    let _ = log.membership();
    let copies = log.reach.cost.chunk_copies;
    let name = sign(
        &log,
        B,
        Body::Name {
            name: "again".into(),
        },
    );
    log.keep(name);
    assert_eq!(
        log.reach.cost.chunk_copies, copies,
        "a lone log copies nothing"
    );
}
