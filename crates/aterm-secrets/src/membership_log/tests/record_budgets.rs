// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The record budgets (`budgets.rs`) on a [`Log`]: each budget refuses its own kinds alone, records
//! that wait on a late parent land alike in any arrival order with the budgets full, a waiting
//! `Admit` that cannot land leaves the waiting room and the seat it named, and each record kind's
//! longest record takes the bytes [`Kind::most_bytes`] counts for it.

use super::*;
use crate::membership_log::budgets::{LANDING, Landing};
use std::sync::mpsc;
use std::time::Duration;
use zeroize::Zeroizing;

fn budgets_of(weighed: usize, keyed: usize, held: usize) -> RecordBudgets {
    RecordBudgets {
        weighed,
        keyed,
        held,
    }
}

/// A fresh `[everyone]` epoch by `by`, held: its id and key.
fn mint(log: &mut Log, by: Mac) -> (RecordId, Zeroizing<[u8; 32]>) {
    let (draft, key) = log
        .draft_epoch(&by.id(), 7, EpochAudience::Everyone, 1)
        .unwrap();
    let epoch = log.keep(draft.sign(&by.seed(), by.incarnation()).unwrap());
    (*epoch.id(), key)
}

/// A `Wrap` by `by` of `epoch` to `to`, not inserted.
fn wrap(log: &Log, by: Mac, epoch: &(RecordId, Zeroizing<[u8; 32]>), to: Mac) -> Record {
    log.draft_wrap(&by.id(), 7, epoch.0, &epoch.1, to.identity().recipient())
        .unwrap()
        .sign(&by.seed(), by.incarnation())
        .unwrap()
}

/// `log` refuses the record `draft` signs from it as past `budget`, and holds what it held.
fn refused_for(log: &mut Log, draft: impl FnOnce(&Log) -> Record, budget: Budget) {
    let before = log.counts();
    let record = draft(log);
    let error = log.insert(record).expect_err("a record past its budget");
    assert_eq!(
        error.kind(),
        crate::ErrorKind::MembershipCapacity,
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains(&format!("limit of {}", budget.counts())),
        "{error}"
    );
    assert_eq!(log.counts(), before);
}

/// Each budget refuses records of its own kinds alone: a full weighed budget refuses the next
/// `Admit` while an `Epoch` and a `Wrap` still enter, a full keyed budget refuses the next `Wrap`
/// while an `Admit` still enters, and the held total refuses both.
///
/// NEGATIVE CONTROL (in the test): the same records under one bound over every kind, the shape
/// before the budgets split, refuse the record each budget let in.
#[test]
fn each_budget_refuses_its_own_kinds_alone() {
    // The weighed budget: the genesis and two `Admit`s fill 3.
    budgets_of(3, 8, 16).within(|| {
        let mut log = found(&[(A, "m1")]);
        log.keep(admit(&log, A, B, "m2"));
        log.keep(admit(&log, A, C, "m3"));
        refused_for(&mut log, |log| admit(log, A, D, "m4"), Budget::Weighed);
        let epoch = mint(&mut log, A);
        log.keep(wrap(&log, A, &epoch, B));
        assert_eq!(
            log.counts(),
            RecordCounts {
                weighed: 3,
                keyed: 2
            }
        );
    });
    budgets_of(3, 3, 3).within(|| {
        let mut log = found(&[(A, "m1")]);
        log.keep(admit(&log, A, B, "m2"));
        log.keep(admit(&log, A, C, "m3"));
        let (draft, _) = log
            .draft_epoch(&A.id(), 7, EpochAudience::Everyone, 1)
            .unwrap();
        let epoch = draft.sign(&A.seed(), A.incarnation()).unwrap();
        refused_for(&mut log, |_| epoch, Budget::Held);
    });

    // The keyed budget: an `Epoch` and two `Wrap`s fill 3.
    budgets_of(8, 3, 16).within(|| {
        let mut log = found(&[(A, "m1"), (B, "m2")]);
        let epoch = mint(&mut log, A);
        log.keep(wrap(&log, A, &epoch, A));
        log.keep(wrap(&log, A, &epoch, B));
        refused_for(&mut log, |log| wrap(log, A, &epoch, C), Budget::Keyed);
        log.keep(admit(&log, A, C, "m3"));
        assert_eq!(
            log.counts(),
            RecordCounts {
                weighed: 2,
                keyed: 3
            }
        );
    });
    budgets_of(4, 4, 4).within(|| {
        let mut log = found(&[(A, "m1"), (B, "m2")]);
        let epoch = mint(&mut log, A);
        log.keep(wrap(&log, A, &epoch, A));
        log.keep(wrap(&log, A, &epoch, B));
        refused_for(&mut log, |log| admit(log, A, C, "m3"), Budget::Held);
    });

    // The held total refuses either kind.
    budgets_of(8, 8, 4).within(|| {
        let mut log = found(&[(A, "m1"), (B, "m2")]);
        log.keep(admit(&log, A, C, "m3"));
        let epoch = mint(&mut log, A);
        log.keep(wrap(&log, A, &epoch, A));
        assert_eq!(log.held_count(), 4);
        refused_for(&mut log, |log| admit(log, A, D, "m4"), Budget::Held);
        refused_for(&mut log, |log| wrap(log, A, &epoch, B), Budget::Held);
    });
}

/// A landing of `waiting` into `log`, read without [`Log::insert`]: pass by pass, each pass's
/// ready records in id order, each landing while the held total and its own budget have room. The
/// held ids in order, and the ids left waiting, sorted.
fn landing(log: &Log, waiting: &[Record]) -> (Vec<RecordId>, Vec<RecordId>) {
    let budgets = log.budgets();
    let mut held: Vec<RecordId> = log.records().map(|r| *r.id()).collect();
    let (mut weighed, mut keyed) = (log.counts().weighed, log.counts().keyed);
    let mut left: Vec<&Record> = waiting.iter().collect();
    loop {
        let mut ready: Vec<&Record> = left
            .iter()
            .copied()
            .filter(|r| r.parents().iter().all(|p| held.contains(p)))
            .collect();
        ready.sort_by_key(|r| *r.id());
        left.retain(|r| !ready.iter().any(|x| x.id() == r.id()));
        let mut landed = false;
        for record in ready {
            let is_weighed = matches!(record.kind(), Kind::Genesis | Kind::Admit | Kind::Remove);
            let room = if is_weighed {
                weighed < budgets.weighed
            } else {
                keyed < budgets.keyed
            };
            if held.len() < budgets.held && room {
                if is_weighed {
                    weighed += 1;
                } else {
                    keyed += 1;
                }
                held.push(*record.id());
                landed = true;
            }
        }
        if !landed {
            break;
        }
    }
    let mut left: Vec<RecordId> = left.iter().map(|r| *r.id()).collect();
    left.sort_unstable();
    (held, left)
}

/// Run `run` with this thread's logs landing waiting records by `landing`.
fn landing_by<T>(landing: Landing, run: impl FnOnce() -> T) -> T {
    let before = LANDING.with(|cell| cell.replace(landing));
    let out = run();
    LANDING.with(|cell| cell.set(before));
    out
}

/// The order of `0..n` an xorshift from `seed` deals.
fn dealt(n: usize, seed: u64) -> Vec<usize> {
    let mut state = seed | 1;
    let mut order: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        order.swap(i, (state % (i as u64 + 1)) as usize);
    }
    order
}

/// Records that wait on one parent land alike when it comes, whatever order they came in, and the
/// held total ends full: 50 orders give one log, the one the landing read in id order gives.
/// Ten siblings follow the late parent (five `Admit`s, five `Name`s) and one more follows a
/// sibling `Admit`; the budgets have room for five of the eleven. Each ready record that does not
/// fit leaves the waiting room; one whose parent did not land stays waiting. The waiting room is
/// keyed by id, so arrival order never reaches the landing: the 50 orders hold that of whatever
/// keeps the waiting records, and the comparison with the landing read holds the id order.
///
/// NEGATIVE CONTROLS (in the test): under `Landing::Reversed` (each pass in reverse id order) the
/// log is not the one the landing read gives, which the comparison refuses; and under
/// `Landing::KeepUnfit` (a landing that skips what does not fit and leaves it waiting) the log
/// keeps the dropped siblings waiting, which the comparison refuses too.
#[test]
fn waiting_records_land_alike_in_any_arrival_order_with_the_budgets_full() {
    let labels: Vec<(Mac, String)> = (1..=11).map(|n| (Mac(n), format!("m{n}"))).collect();
    let founders: Vec<(Mac, &str)> = labels.iter().map(|(m, l)| (*m, l.as_str())).collect();
    budgets_of(4, 4, 7).within(|| {
        let base = found(&founders);
        let mut late = base.clone();
        let parent = late.keep(sign(
            &late,
            A,
            Body::Name {
                name: "late".into(),
            },
        ));
        let mut children = Vec::new();
        for n in 2..=6 {
            children.push(admit(&late, Mac(n), Mac(20 + n), &format!("m{}", 20 + n)));
        }
        for n in 7..=11 {
            children.push(sign(
                &late,
                Mac(n),
                Body::Name {
                    name: format!("n{n}"),
                },
            ));
        }
        let mut deeper = late.clone();
        deeper.keep(children[0].clone());
        children.push(sign(
            &deeper,
            Mac(2),
            Body::Name {
                name: "after".into(),
            },
        ));
        assert!(
            children[..10].iter().all(|r| r.parents() == [*parent.id()]),
            "the siblings follow the late parent alone"
        );

        let deal = |seed: u64| -> Vec<Record> {
            dealt(children.len(), seed)
                .into_iter()
                .map(|i| children[i].clone())
                .collect()
        };
        let land = |arrived: &[Record]| -> Log {
            let mut log = base.clone();
            for record in arrived {
                assert_eq!(log.insert(record.clone()).unwrap(), Inserted::Waiting);
            }
            assert_eq!(log.insert(parent.clone()).unwrap(), Inserted::Held);
            log
        };
        let read = |log: &Log| -> (Vec<RecordId>, Vec<RecordId>) {
            (
                log.records().map(|r| *r.id()).collect(),
                log.waiting.keys().copied().collect(),
            )
        };
        let expected = landing(&late, &children);
        assert_eq!(expected.0.len(), 7, "the held total ends full");
        for seed in 0..50 {
            let log = land(&deal(0x9e37_79b9_7f4a_7c15 ^ seed));
            assert_eq!(read(&log), expected, "order {seed}");
            assert_eq!(log.held_count(), log.budgets().held);
            assert!(
                log.waiting() <= 1,
                "only a record whose parent did not land waits"
            );
        }

        let reversed = landing_by(Landing::Reversed, || land(&deal(0x9e37_79b9_7f4a_7c15)));
        assert_ne!(
            read(&reversed),
            expected,
            "a landing in reverse id order is not the landing read"
        );
        let mutant = landing_by(Landing::KeepUnfit, || land(&deal(0x9e37_79b9_7f4a_7c15)));
        assert_ne!(read(&mutant), expected, "the mutant keeps what did not fit");
        assert!(mutant.waiting() > 1);
    });
}

/// An `Admit` waiting on a parent that comes once the weighed budget is full lands nothing: the
/// parent's insert returns (a one-minute hang detector), the `Admit` leaves the waiting room, and
/// the Mac it names holds no seat any more, so in a store's log (seats required) its next record
/// missing a parent is refused rather than kept waiting.
///
/// NEGATIVE CONTROL (in the test): under `Landing::KeepUnfit` (a landing that skips the `Admit` and
/// leaves it waiting) the insert still returns, but the `Admit` stays waiting and the Mac's record
/// is kept waiting on its seat: both of the checks below fail there, as the run shows.
#[test]
fn a_late_parent_into_a_full_weighed_budget_lands_nothing_and_returns() {
    // Whether the `Admit` still waits, and whether its Mac's next record waits.
    let run = |keep_unfit: bool| -> (bool, bool) {
        budgets_of(2, 8, 16).within(|| {
            let mut log = found(&[(A, "m1"), (B, "m2")]);
            log.require_seats();
            log.keep(admit(&log, A, C, "m3"));
            assert_eq!(log.counts().weighed, 2, "the weighed budget is full");

            let mut ahead = log.clone();
            let parent = sign(
                &ahead,
                A,
                Body::Name {
                    name: "late".into(),
                },
            );
            assert_eq!(ahead.insert(parent.clone()).unwrap(), Inserted::Held);
            let entry = admit(&ahead, A, D, "m4");
            assert_eq!(entry.parents(), [*parent.id()]);
            let mut further = ahead.clone();
            further.keep(sign(
                &further,
                B,
                Body::Name {
                    name: "unsent".into(),
                },
            ));
            let first = sign(&further, D, Body::Name { name: "one".into() });
            let next = sign(&further, D, Body::Name { name: "two".into() });

            assert_eq!(log.insert(entry.clone()).unwrap(), Inserted::Waiting);
            assert_eq!(
                log.insert(first).unwrap(),
                Inserted::Waiting,
                "the waiting Admit seats its Mac"
            );
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let landing = if keep_unfit {
                    Landing::KeepUnfit
                } else {
                    Landing::Sound
                };
                let inserted = landing_by(landing, || log.insert(parent).unwrap());
                tx.send((log, inserted)).unwrap();
            });
            let (mut log, inserted) = rx
                .recv_timeout(Duration::from_secs(60))
                .expect("the late parent's insert returns");
            assert_eq!(inserted, Inserted::Held);
            assert!(log.get(entry.id()).is_none(), "the Admit did not land");
            assert_eq!(log.counts().weighed, 2);
            (
                log.waiting.contains_key(entry.id()),
                log.insert(next).is_ok(),
            )
        })
    };
    assert_eq!(
        run(false),
        (false, false),
        "the Admit left the waiting room, and with it its Mac's seat"
    );
    assert_eq!(run(true), (true, true), "the mutant kept both");
}

/// Each record kind whose fields bound it takes at most [`Kind::most_bytes`], which an admission's
/// batch is weighed by before it is signed (`client::readmit::decide`): its longest record (the
/// most parents, the longest name and label, the largest capsule) takes exactly that.
///
/// NEGATIVE CONTROL (in the test): each of those fields one past its bound is refused, so these
/// records are the longest of their kinds: a count short of any of them fails the equality.
#[test]
fn each_bounded_kinds_longest_record_takes_its_most_bytes() {
    let log = found(&[(A, "m1")]);
    let parents: Vec<RecordId> = (1..=MAX_PARENTS)
        .map(|n| [u8::try_from(n).unwrap(); 32])
        .collect();
    let draft = |parents: &[RecordId], body: Body| Draft {
        network: *log.network(),
        sequence: 2,
        previous: Some(parents[0]),
        parents: parents.to_vec(),
        authored_ms: 7,
        body,
    };
    let longest = |body: Body| draft(&parents, body).sign(&A.seed(), A.incarnation());
    let node_pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[0x5a; 32]).unwrap();
    let proof: [u8; 64] = node_pair
        .sign(&node_proof_message(log.network(), &A.id()))
        .as_ref()
        .try_into()
        .unwrap();
    let admit = |name: &str, label: &str| Body::Admit {
        entrant: Entrant {
            name: name.to_owned(),
            ..B.entrant(label)
        },
        evidence: Evidence::Phrase,
    };
    let wrap = |capsule: usize| Body::Wrap {
        epoch: [9; 32],
        recipient: B.identity().recipient(),
        capsule: Payload::from(vec![1; capsule]),
    };
    let name = "n".repeat(MAX_NAME);
    let bodies = [
        admit(&name, "m99999"),
        Body::Remove {
            member: B.id(),
            cutoff: [9; 32],
        },
        Body::Name { name: name.clone() },
        wrap(entries::MAX_WRAP_CAPSULE),
        Body::Node {
            node_key: node_pair.public_key().as_ref().try_into().unwrap(),
            proof,
        },
    ];
    for body in bodies {
        let kind = body.kind();
        let record = longest(body).unwrap();
        assert_eq!(record.bytes().len(), kind.most_bytes(), "{kind:?}");
    }

    let more_parents: Vec<RecordId> = (1..=MAX_PARENTS + 1)
        .map(|n| [u8::try_from(n).unwrap(); 32])
        .collect();
    let past = [
        draft(&more_parents, Body::Name { name: name.clone() }),
        draft(&parents, admit(&format!("{name}n"), "m99999")),
        draft(&parents, admit(&name, "m100000")),
        draft(
            &parents,
            Body::Name {
                name: format!("{name}n"),
            },
        ),
        draft(&parents, wrap(entries::MAX_WRAP_CAPSULE + 1)),
    ];
    for draft in past {
        let kind = draft.body.kind();
        assert!(
            draft.sign(&A.seed(), A.incarnation()).is_err(),
            "{kind:?} past a bound"
        );
    }
}

/// The kinds whose body depends on their audience and value take at most what
/// [`entries::most_bytes`] counts for them (a re-key's room in bytes, one-mesh proofs P6 F6):
/// an `Epoch` of a host list, a `Put` with an anchor and of the Commons descriptor, and a
/// `Delete`, each signed with the parents the log gives it, take exactly that less 32 bytes for
/// each parent short of [`MAX_PARENTS`].
///
/// NEGATIVE CONTROL (in the test): the same `Put` with a value one byte longer takes one byte
/// more, and its bound is one byte more too, so a bound that read no value (or capped it) fails.
/// The bound is new with P6, so no rule before it is run.
#[test]
fn a_rekeys_records_take_their_most_bytes() {
    let mut log = found(&[(A, "m1")]);
    let short = |record: &Record| 32 * (MAX_PARENTS - record.parents().len());
    let hosts = EpochAudience::Hosts(vec!["m1".to_owned(), "m2".to_owned()]);
    let minted = |log: &mut Log, audience: &EpochAudience| {
        let (draft, key) = log.draft_epoch(&A.id(), 7, audience.clone(), 1).unwrap();
        let epoch = log.keep(draft.sign(&A.seed(), A.incarnation()).unwrap());
        assert_eq!(
            epoch.bytes().len() + short(&epoch),
            entries::most_bytes::epoch(audience),
            "{audience:?}"
        );
        (*epoch.id(), key)
    };
    let (ring, ring_key) = minted(&mut log, &hosts);
    let (commons, commons_key) = minted(&mut log, &EpochAudience::Commons);
    let put = |log: &Log, value: usize| {
        let entry = crate::text::Entry {
            name: "ring-cred".to_owned(),
            value: crate::SecretBytes::from(vec![b'v'; value]),
            audience: Audience::Hosts(vec!["m1".to_owned(), "m2".to_owned()]),
            if_absent: true,
            anchor: Some("ring-anchor".to_owned()),
        };
        let record = log
            .draft_put(&A.id(), 7, ring, &ring_key, &entry)
            .unwrap()
            .sign(&A.seed(), A.incarnation())
            .unwrap();
        let most = entries::most_bytes::put(&hosts, 9, Some(11), value);
        (record.bytes().len() + short(&record), most)
    };
    let (taken, most) = put(&log, 300);
    assert_eq!(taken, most, "a Put with an anchor");
    assert_eq!(
        put(&log, 301),
        (taken + 1, most + 1),
        "CONTROL: a byte more of value takes a byte more, and the bound reads it"
    );
    let descriptor = b"ACD1-descriptor".as_slice();
    let record = log
        .draft_commons_put(&A.id(), 7, commons, &commons_key, descriptor)
        .unwrap()
        .sign(&A.seed(), A.incarnation())
        .unwrap();
    assert_eq!(
        record.bytes().len() + short(&record),
        entries::most_bytes::put(
            &EpochAudience::Commons,
            crate::commons::ENTRY.len(),
            None,
            descriptor.len()
        ),
        "the Commons descriptor"
    );
    let record = log
        .draft_delete(&A.id(), 7, ring, &ring_key, "ring-cred")
        .unwrap()
        .sign(&A.seed(), A.incarnation())
        .unwrap();
    assert_eq!(
        record.bytes().len() + short(&record),
        entries::most_bytes::delete(&hosts, 9),
        "a Delete"
    );
}
