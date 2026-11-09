// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weighing over the weighed records alone (`eval.rs`): the reader pass authorizes and voids
//! what the full pass it replaced did, what weighing costs does not move when records of other
//! kinds lie among the weighed ones, and a log carries its weighing across every record of
//! another kind, keeping only what is read after it, and weighs again at each weighed one
//! (`Log::hold`).

use super::super::eval::{self, MUTANT, Mutant, WEIGHINGS};
use super::reach::live_shape;
use super::weigh::{corpus, generated_logs};
use super::*;

/// Every record `log` holds, the genesis first.
fn held(log: &Log) -> Vec<Record> {
    log.records().cloned().collect()
}

/// Run `run` with `mutant` set, and clear it however `run` ends.
fn with_mutant<T>(mutant: Mutant, run: impl FnOnce() -> T) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            MUTANT.set(None);
        }
    }
    let _clear = Clear;
    MUTANT.set(Some(mutant));
    run()
}

/// Signs records chain by chain: each Mac's at its next sequence, after its previous record.
struct Chains {
    network: RecordId,
    previous: BTreeMap<u8, (RecordId, u64)>,
}

impl Chains {
    fn new(network: RecordId) -> Self {
        Self {
            network,
            previous: BTreeMap::new(),
        }
    }

    fn sign(&mut self, by: Mac, mut parents: Vec<RecordId>, body: Body) -> Record {
        let previous = self.previous.get(&by.0).copied();
        parents.extend(previous.map(|(id, _)| id));
        let sequence = previous.map_or(1, |(_, s)| s + 1);
        let record = Draft {
            network: self.network,
            sequence,
            previous: previous.map(|(id, _)| id),
            parents,
            authored_ms: 1_000,
            body,
        }
        .sign(&by.seed(), by.incarnation())
        .unwrap();
        self.previous.insert(by.0, (*record.id(), sequence));
        record
    }
}

/// How many records of other kinds [`padded`] lays among a log's records.
const PADDING: usize = 12_000;

/// The Macs that sign [`padded`]'s records: founders of most hand-made logs, and Macs no record
/// seats, so no record names.
const PADDERS: [Mac; 5] = [A, X, Mac(200), Mac(201), Mac(202)];

/// `records` (a log's held records, the genesis first) with an `Epoch` A signs on the genesis
/// next, and [`PADDING`] Wraps and Puts under it laid evenly among the rest: each follows the
/// record laid just before it, so it lies after the weighed records before it and in no
/// weighed record's past.
fn padded(records: &[Record]) -> Vec<Record> {
    let genesis = records[0].clone();
    let mut chains = Chains::new(*genesis.id());
    let epoch = chains.sign(
        A,
        vec![*genesis.id()],
        Body::Epoch {
            audience: EpochAudience::Everyone,
            generation: 1,
            commitment: [7; 32],
        },
    );
    let epoch_id = *epoch.id();
    let recipient = Mac(200).identity().recipient();
    let rest = &records[1..];
    let each = PADDING.div_ceil(rest.len());
    let mut out = Vec::with_capacity(records.len() + 1 + PADDING);
    out.extend([genesis, epoch]);
    let mut laid = 0;
    for record in rest {
        out.push(record.clone());
        for _ in 0..each.min(PADDING - laid) {
            let by = PADDERS[laid % PADDERS.len()];
            let body = if laid % 4 == 3 {
                Body::Put {
                    audience: EpochAudience::Everyone,
                    epoch: epoch_id,
                    salt: [3; 32],
                    ciphertext: vec![0xc3; 17].into(),
                }
            } else {
                Body::Wrap {
                    epoch: epoch_id,
                    recipient,
                    capsule: vec![0x5a; 16].into(),
                }
            };
            let last = *out.last().expect("the genesis leads").id();
            out.push(chains.sign(by, vec![last, epoch_id], body));
            laid += 1;
        }
    }
    assert_eq!(laid, PADDING);
    out
}

/// A hand-made log's held records beside them padded ([`padded`]).
struct Padded {
    name: String,
    records: Vec<Record>,
    padded: Vec<Record>,
}

/// Hand-made logs with quorums, restores and every kind of record, each padded.
fn padded_logs() -> &'static [Padded] {
    static PADDED: OnceLock<Vec<Padded>> = OnceLock::new();
    PADDED.get_or_init(|| {
        let picked = [
            "§4.2 N=32",
            "distinct K=8",
            "restored letting in m=10 K=8",
            "every kind",
        ];
        let logs: Vec<_> = corpus()
            .into_iter()
            .filter(|(name, _)| picked.contains(&name.as_str()))
            .map(|(name, log)| {
                let records = held(&log);
                let padded = padded(&records);
                Padded {
                    name,
                    records,
                    padded,
                }
            })
            .collect();
        assert_eq!(logs.len(), picked.len());
        logs
    })
}

/// Every log the differential reads: the hand-made ones, 200 generated ones, the live shape and
/// the padded logs, each as its held records.
fn every_log() -> Vec<(String, Vec<Record>)> {
    let mut logs: Vec<(String, Vec<Record>)> = corpus()
        .iter()
        .chain(generated_logs())
        .map(|(name, log)| (name.clone(), held(log)))
        .collect();
    logs.push(("the live shape".into(), held(live_shape())));
    for Padded { name, padded, .. } in padded_logs() {
        logs.push((format!("{name}, padded"), padded.clone()));
    }
    logs
}

/// The reader pass, extended to every record in one sweep, authorizes every record as the full
/// pass did, and the weighing's voids void what the full pass's did, with nothing void, under
/// each candidate's effects and under the quorums that take effect: on every hand-made log, 200
/// generated ones, the live shape, and logs with 12,000 Wraps and Puts laid among their records.
/// NEGATIVE CONTROLS: a pass that skips one reader (the first `Remove`), or that reads each
/// record's authorization and void at its record index rather than its slot, disagrees with the
/// full pass.
#[test]
fn the_reader_pass_authorizes_and_voids_what_the_full_pass_did() {
    let mut checked = 0;
    for (name, records) in every_log() {
        assert_eq!(eval::reader_pass_disagreements(&records), 0, "{name}");
        checked += records.len();
    }
    assert!(checked > 4 * PADDING, "the padded logs were read");
    let logs: Vec<Vec<Record>> = corpus().iter().map(|(_, log)| held(log)).collect();
    for mutant in [Mutant::SkipReader, Mutant::ReadByRecord] {
        let wrong: usize = with_mutant(mutant, || {
            logs.iter()
                .map(|records| eval::reader_pass_disagreements(records))
                .sum()
        });
        assert!(wrong > 0, "{mutant:?} goes unnoticed");
    }
}

/// Weighing costs exactly what it cost before 12,000 Wraps and Puts by five signers (A, X and
/// three Macs no record seats) were laid among a log's records: the same `is_member_upto` calls
/// and scans, authorization passes and the reads each pass makes (one `is_member_before` per
/// weighed record but the genesis), class bytes and walk bookkeeping; and the log's members,
/// removals and quorums stay the same.
/// NEGATIVE CONTROLS: interning every signer as an actor, not only the founders and the signers,
/// entrants and targets of weighed records, costs a padded log more `is_member_upto` calls; a
/// pass that also reads every record of another kind, as the full pass did, costs every padded
/// log more reads.
#[test]
fn weighing_costs_the_same_with_twelve_thousand_records_of_other_kinds_among_the_weighed() {
    let mut weighed = false;
    for Padded {
        name,
        records,
        padded,
    } in padded_logs()
    {
        assert_eq!(padded.len(), records.len() + 1 + PADDING, "{name}");
        let cost = eval::weigh_cost(records);
        assert_eq!(eval::weigh_cost(padded), cost, "{name}");
        let readers = records.iter().filter(|r| r.kind().weighed()).count();
        assert_eq!(cost.before_calls, cost.passes * (readers - 1), "{name}");
        weighed |= cost.evaluations > 0 && cost.class_bytes > 0 && cost.passes > 1;
        let (alone, among) = (eval::evaluate(records), eval::evaluate(padded));
        assert_eq!(among.members(), alone.members(), "{name}");
        assert_eq!(among.removed(), alone.removed(), "{name}");
        assert_eq!(among.quorums(), alone.quorums(), "{name}");
    }
    assert!(weighed, "some padded log weighs candidates in classes");
    let logs = padded_logs();
    let grown = |mutant, by: fn(&eval::WeighCost) -> usize| {
        with_mutant(mutant, || {
            logs.iter()
                .filter(
                    |Padded {
                         records, padded, ..
                     }| {
                        by(&eval::weigh_cost(padded)) > by(&eval::weigh_cost(records))
                    },
                )
                .count()
        })
    };
    assert!(
        grown(Mutant::InternEverySigner, |cost| cost.evaluations) > 0,
        "interning every signer goes unnoticed"
    );
    assert_eq!(
        grown(Mutant::PassOverEveryRecord, |cost| cost.before_calls),
        logs.len(),
        "a pass over every record goes unnoticed"
    );
}

/// What replaying a log record by record found ([`replay`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct Replayed {
    /// The weighings the log ran.
    weighings: usize,
    /// The weighed records it held, the genesis among them.
    weighed: usize,
    /// Checked steps whose carried weighing differs from one weighed afresh.
    stale: usize,
    /// Appends that kept the weighing a weighed record changes, or let go of one no record
    /// changed.
    kept_wrong: usize,
    /// Checked steps whose membership differs from evaluating the held records afresh.
    membership: usize,
}

/// Replay `log`'s records one by one into a log of its genesis (seats enforced as `log`
/// enforces them), reading the weighing at every step: one weighed afresh from what the log
/// holds is compared every `every`th step, at each weighed record and the step after it, and at
/// the last step, and so is the membership where `membership` says.
fn replay(log: &Log, every: usize, membership: bool) -> (Replayed, Log) {
    let mut again = Log::new(log.order[0].clone()).unwrap();
    again.enforce_seats = log.enforce_seats;
    let mut found = Replayed {
        weighed: 1,
        ..Replayed::default()
    };
    let mut recent = true;
    for (step, record) in log.order.iter().enumerate() {
        let weighed = record.kind().weighed();
        if step > 0 {
            let kept = Arc::clone(&again.weighing);
            again.keep(record.clone());
            found.weighed += usize::from(weighed);
            found.kept_wrong += usize::from(Arc::ptr_eq(&kept, &again.weighing) == weighed);
        }
        let before = WEIGHINGS.get();
        let carried = again.weighing();
        found.weighings += WEIGHINGS.get() - before;
        if step % every == 0 || weighed || recent || step + 1 == log.order.len() {
            found.stale += usize::from(*carried != eval::weigh(again.history()));
            if membership {
                found.membership += usize::from(again.membership() != eval::evaluate(&again.order));
            }
        }
        recent = weighed;
    }
    (found, again)
}

/// A log weighs once when first read and again only after it holds a weighed record (the
/// genesis, an `Admit` or a `Remove`), and its carried weighing equals one weighed afresh from
/// what it holds, as its membership equals the held records evaluated afresh: replayed record by
/// record, on every hand-made log, 200 generated ones and the live shape, which weighs 36 times
/// for its 4,071 records. A clone shares the weighing across a record of another kind, and the
/// queries that read the weighing (membership, effective records, undo, seats and known actors)
/// weigh nothing more.
/// NEGATIVE CONTROLS: a log that carries its weighing across an `Admit`, or across a `Remove`,
/// holds a weighing that differs from a fresh one and shares it where it should weigh again.
#[test]
fn a_log_weighs_again_only_after_it_holds_a_weighed_record() {
    for (name, log) in corpus().iter().chain(generated_logs()) {
        let (found, _) = replay(log, 1, true);
        assert_eq!(found.weighings, found.weighed, "{name}");
        assert_eq!(
            (found.stale, found.kept_wrong, found.membership),
            (0, 0, 0),
            "{name}"
        );
    }
    let (found, again) = replay(live_shape(), 64, false);
    assert_eq!((found.weighings, found.weighed), (36, 36), "the live shape");
    assert_eq!((found.stale, found.kept_wrong), (0, 0), "the live shape");
    assert_eq!(again.membership(), live_shape().membership());

    let before = WEIGHINGS.get();
    let membership = again.membership();
    let _ = again.effective();
    for (id, member) in membership.members() {
        let _ = (
            again.undo_cutoff(id),
            again.undo(id),
            again.has_known_actor(id),
        );
        assert!(again.historical_seat_entry(&member.binding).is_some());
        assert!(again.has_historical_seat(&member.binding));
    }
    assert_eq!(WEIGHINGS.get(), before, "a query weighed again");

    let logs = corpus();
    for kind in [Kind::Admit, Kind::Remove] {
        let (wrong, stale) = with_mutant(Mutant::CarryAcross(kind), || {
            logs.iter().fold((0, 0), |(wrong, stale), (_, log)| {
                let (found, _) = replay(log, 1, false);
                (wrong + found.kept_wrong, stale + found.stale)
            })
        });
        assert!(
            wrong > 0 && stale > 0,
            "carrying across {kind:?} goes unnoticed"
        );
    }
}

/// A log's seats and known actors read what it keeps of its history (`eval::Historical`), and
/// it keeps each seat it found: replayed record by record, on every hand-made log and 200
/// generated ones, each binding a founder or an `Admit` of the log names finds at every step the
/// seat a derivation afresh from the records held finds, and each actor is known as one afresh
/// knows it, asked twice; a step's asks derive what the log keeps once, and the second ask
/// searches the log's graph again only for a binding with no seat.
/// NEGATIVE CONTROLS: a log that keeps what it derived across a hold misses a seat a later
/// `Admit` granted (`what a derivation afresh finds`); a seat search, or a known actor, read from
/// a derivation of its own derives again (`the asks derive once`); a log that keeps no seat it
/// found searches for each again (`a seat found is searched for once`).
#[test]
fn a_log_finds_each_seat_once_until_it_holds_a_record() {
    use super::super::eval::{HISTORICALS, SEAT_SEARCHES};
    for (name, log) in corpus().iter().chain(generated_logs()) {
        let mut bindings: Vec<crate::commons::binding::Binding> = Vec::new();
        for record in log.order.iter() {
            match record.body() {
                Body::Genesis { founders, .. } => {
                    bindings.extend(founders.iter().filter_map(|f| f.binding.clone()));
                }
                Body::Admit { entrant, .. } => bindings.push(entrant.request.binding().clone()),
                _ => {}
            }
        }
        bindings.sort();
        bindings.dedup();
        let actors: BTreeSet<MemberId> = bindings.iter().map(|b| b.actor()).collect();
        let mut again = Log::new(log.order[0].clone()).unwrap();
        again.enforce_seats = log.enforce_seats;
        for (step, record) in log.order.iter().enumerate() {
            if step > 0 {
                again.keep(record.clone());
            }
            let weighing = eval::weigh(again.history());
            let afresh = |binding| {
                eval::historical_seat_entry(again.history(), &OnceLock::new(), &weighing, binding)
            };
            let seats: Vec<_> = bindings.iter().map(afresh).collect();
            let known: Vec<bool> = actors
                .iter()
                .map(|actor| {
                    eval::has_known_actor(again.history(), &OnceLock::new(), &weighing, actor)
                })
                .collect();
            let derived = HISTORICALS.get();
            for ask in 0..2 {
                let before = SEAT_SEARCHES.get();
                let found: Vec<_> = bindings
                    .iter()
                    .map(|binding| again.historical_seat_entry(binding))
                    .collect();
                let searched = SEAT_SEARCHES.get() - before;
                assert_eq!(
                    found, seats,
                    "{name}, step {step}, ask {ask}: what a derivation afresh finds"
                );
                let knows: Vec<bool> = actors.iter().map(|a| again.has_known_actor(a)).collect();
                assert_eq!(
                    knows, known,
                    "{name}, step {step}, ask {ask}: the known actors"
                );
                if ask == 1 {
                    assert_eq!(
                        searched,
                        seats.iter().filter(|seat| seat.is_none()).count(),
                        "{name}, step {step}: a seat found is searched for once"
                    );
                }
            }
            assert_eq!(
                HISTORICALS.get() - derived,
                1,
                "{name}, step {step}: the asks derive once"
            );
        }
    }
}

/// A carried weighing keeps the members at the cutoffs of the quorums that take effect alone, at
/// most five ids for each of their shares, not a set of members for every cutoff a candidate
/// names: A admits 64 Macs in a chain, and A and B each remove every one of them at the `Admit`
/// that let it in, so 64 candidates name 64 cutoffs, each with one member more than the one
/// before, and only the first two hold. Every query reads what is kept, and the carried weighing
/// equals one weighed afresh.
/// NEGATIVE CONTROL: the whole apart weighing, which a log carried before it was cut down, holds
/// the members at every cutoff and passes the bound.
#[test]
fn a_carried_weighing_keeps_only_what_its_quorums_read() {
    const ADMITTED: u8 = 64;
    let mut lab = found(&[(A, "m1"), (B, "m2")]);
    let entries: Vec<(Mac, RecordId)> = (1..=ADMITTED)
        .map(|k| {
            let mac = Mac(100 + k);
            let entry = lab.keep(admit(&lab, A, mac, &format!("m{}", mac.0)));
            (mac, *entry.id())
        })
        .collect();
    for &(mac, entry) in &entries {
        for by in [A, B] {
            lab.keep(remove(&lab, by, mac, &entry));
        }
    }
    let removes = 2 * usize::from(ADMITTED);
    assert_eq!(lab.membership().quorums().len(), 2);
    let (kept, whole) = eval::apart_ids(lab.history(), lab.weighing());
    assert!(
        kept <= 5 * removes,
        "{kept} ids kept for {removes} removals"
    );
    assert!(
        whole > 5 * removes,
        "the whole apart weighing holds only {whole} ids"
    );
    assert_eq!((kept, whole), (16, 2_217));
    assert_eq!(*lab.weighing(), eval::weigh(lab.history()));
}

/// The most candidates a log within its weighed budget can hold apart, weighed in full: the
/// genesis binds A, B, C and D, D signs 2,047 `Name`s, and A and B each remove D at every one of
/// them, so 4,095 records are weighed (the genesis and 4,094 `Remove`s) and 2,047 candidates
/// hold apart, every one active in the relations. Their 2,047-wide relations fit the payload
/// budget, so the weighing never meets the relations error: every quorum takes effect and D is
/// out. Each candidate voids only D's `Name`s, which no weighed record reads, so all weigh in
/// one authorization pass, at four members' re-check per cutoff.
///
/// The fixture holds 6,142 records, signed into a record list and evaluated through the
/// evaluator's seam. Its weighed records stay within `MAX_WEIGHED_RECORDS`, the bound that sizes
/// the relations.
#[test]
fn the_widest_candidate_set_a_held_log_can_weigh_fits_the_relations_budget() {
    const CUTOFFS: usize = (MAX_WEIGHED_RECORDS - 1) / 2;
    let lab = found(&[(A, "m1"), (B, "m2"), (C, "m3"), (D, "m4")]);
    let mut chains = Chains::new(*lab.network());
    let mut records = vec![lab.genesis().clone()];
    let mut names = Vec::with_capacity(CUTOFFS);
    for k in 0..CUTOFFS {
        let after = names.last().copied().unwrap_or(*lab.network());
        let name = chains.sign(
            D,
            vec![after],
            Body::Name {
                name: format!("d{k}"),
            },
        );
        names.push(*name.id());
        records.push(name);
    }
    let member = D.id();
    for by in [A, B] {
        for &cutoff in &names {
            records.push(chains.sign(by, vec![cutoff], Body::Remove { member, cutoff }));
        }
    }
    let weighed = records.iter().filter(|r| r.kind().weighed()).count();
    assert_eq!((records.len(), weighed), (6_142, MAX_WEIGHED_RECORDS - 1));

    let membership = eval::evaluate(&records);
    assert_eq!(membership.quorums().len(), CUTOFFS);
    let members: BTreeSet<MemberId> = membership.members().keys().copied().collect();
    assert_eq!(members, ids(&[A, B, C]));
    let cost = eval::weigh_cost(&records);
    assert_eq!((cost.passes, cost.evaluations), (1, 4 * CUTOFFS));
}
