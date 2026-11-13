// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::Request;
use crate::membership_log::{Body, Entrant, Inserted};

fn fixture() -> (Log, [u8; 32], [u8; 16], super::super::MemberId) {
    let seed = [17; 32];
    let incarnation = [18; 16];
    let identity = Identity::from_secret_bytes([19; 32]);
    let request =
        Request::create([20; 16], [21; 32], [22; 16], incarnation, &identity, &seed).unwrap();
    let actor = request.binding().actor();
    let genesis = Record::found(
        &[23; 32],
        vec![Entrant {
            request,
            node_key: [24; 32],
            name: "member".to_owned(),
            label: "m1".to_owned(),
        }],
        1,
    )
    .unwrap();
    (Log::new(genesis).unwrap(), seed, incarnation, actor)
}

#[test]
fn record_status_and_generation_follow_complete_history_only() {
    let (mut log, seed, incarnation, actor) = fixture();
    let mut future = log.clone();
    let parent = future
        .draft(
            &actor,
            2,
            Body::Name {
                name: "parent".into(),
            },
        )
        .sign(&seed, incarnation)
        .unwrap();
    future.insert(parent.clone()).unwrap();
    let child = future
        .draft(
            &actor,
            3,
            Body::Name {
                name: "child".into(),
            },
        )
        .sign(&seed, incarnation)
        .unwrap();
    let before = HistoryView::new(&log)
        .lease_complete()
        .unwrap()
        .generation();
    assert_eq!(log.insert(child.clone()).unwrap(), Inserted::Waiting);
    let waiting = HistoryView::new(&log);
    let lease = waiting.lease_complete().unwrap();
    assert_eq!(lease.generation(), before);
    assert!(matches!(
        lease.record(log.network()).unwrap(),
        RecordState::Complete(_)
    ));
    assert!(matches!(
        lease.record(child.id()).unwrap(),
        RecordState::Pending
    ));
    assert!(matches!(
        lease.record(&[0xff; 32]).unwrap(),
        RecordState::Absent
    ));
    assert_eq!(log.insert(parent).unwrap(), Inserted::Held);
    let complete = HistoryView::new(&log);
    let lease = complete.lease_complete().unwrap();
    assert_ne!(lease.generation(), before);
    assert!(matches!(
        lease.record(child.id()).unwrap(),
        RecordState::Complete(_)
    ));
}

/// A log's history generation names its held records in their order, derived once for them
/// and shared by its clones: after each record a log holds, asked first through a clone, a
/// lease's generation is the one a log given the same records afresh gives, and the clone
/// keeps its own; a clone first asked after the log held another record and was asked gives
/// its own records' too.
/// NEGATIVE CONTROL: `Log::hold` keeping the generation gives the log's lease the generation
/// before the record it held.
#[test]
fn a_leases_generation_follows_each_record_the_log_holds() {
    let (mut log, seed, incarnation, actor) = fixture();
    let generation = |log: &Log| HistoryView::new(log).lease_complete().unwrap().generation();
    let afresh = |log: &Log| {
        let mut records = log.records().cloned();
        let mut fresh = Log::new(records.next().unwrap()).unwrap();
        for record in records {
            assert_eq!(fresh.insert(record).unwrap(), Inserted::Held);
        }
        generation(&fresh)
    };
    let hold = |log: &mut Log, n: u64| {
        let next = log
            .draft(
                &actor,
                2 + n,
                Body::Name {
                    name: format!("name-{n}"),
                },
            )
            .sign(&seed, incarnation)
            .unwrap();
        assert_eq!(log.insert(next).unwrap(), Inserted::Held);
    };
    for n in 0..4 {
        let asked = log.clone();
        let before = generation(&asked);
        assert_eq!(before, afresh(&log));
        hold(&mut log, n);
        assert_eq!(generation(&log), afresh(&log));
        assert_ne!(generation(&log), before);
        assert_eq!(generation(&asked), before);
    }
    hold(&mut log, 4);
    let unasked = log.clone();
    hold(&mut log, 5);
    assert_eq!(generation(&log), afresh(&log));
    assert_eq!(generation(&unasked), afresh(&unasked));
}

/// A log keeps which epochs each device opened for its held records: a lease of it, or of a
/// clone, answers an epoch noted as opened for that device and for no other, and once the log
/// holds another record its leases answer afresh, while the clone taken before keeps its own.
/// NEGATIVE CONTROL: `Log::hold` keeping `opened` gives the log's lease the epoch opened after
/// it holds another record.
#[test]
fn a_logs_opened_epochs_follow_its_held_records() {
    let (mut log, seed, incarnation, actor) = fixture();
    let identity = Identity::from_secret_bytes([19; 32]);
    let other = Identity::from_secret_bytes([29; 32]);
    let epoch = *log.network();
    let opened = |log: &Log, identity: &Identity| {
        let view = HistoryView::new(log);
        let lease = view.lease_complete().unwrap();
        let RecordState::Complete(epoch) = lease.record(&epoch).unwrap() else {
            panic!("the genesis is held");
        };
        lease.opened(&epoch, identity).unwrap()
    };
    assert!(!opened(&log, &identity));
    {
        let view = HistoryView::new(&log);
        let lease = view.lease_complete().unwrap();
        let RecordState::Complete(held) = lease.record(&epoch).unwrap() else {
            panic!("the genesis is held");
        };
        lease.note_opened(&held, &identity).unwrap();
    }
    let before = log.clone();
    assert!(opened(&log, &identity));
    assert!(opened(&before, &identity), "a clone shares what opened");
    assert!(!opened(&log, &other), "another device opened nothing");
    let next = log
        .draft(
            &actor,
            2,
            Body::Name {
                name: "next".into(),
            },
        )
        .sign(&seed, incarnation)
        .unwrap();
    assert_eq!(log.insert(next).unwrap(), Inserted::Held);
    assert!(
        !opened(&log, &identity),
        "a log that holds another record answers afresh"
    );
    assert!(opened(&before, &identity));
}

/// A lease answers each held record as the log holds it, whether it takes the log's own
/// records or reads them through a provider, and a waiting or unknown one as such.
/// NEGATIVE CONTROL: the log's own lease answering the record before the one asked
/// (`order.get(at.saturating_sub(1))`) answers the genesis for the record after it.
#[test]
fn a_lease_answers_each_held_record_as_the_log_holds_it() {
    struct Reads<'a>(&'a Log);
    impl RecordReader for Reads<'_> {
        fn read(&self, id: &RecordId) -> Result<Option<&Record>> {
            Ok(self.0.get(id))
        }
    }
    let (mut log, seed, incarnation, actor) = fixture();
    for n in 0..3 {
        let next = log
            .draft(
                &actor,
                2 + n,
                Body::Name {
                    name: format!("name-{n}"),
                },
            )
            .sign(&seed, incarnation)
            .unwrap();
        assert_eq!(log.insert(next).unwrap(), Inserted::Held);
    }
    let mut future = log.clone();
    let parent = future
        .draft(
            &actor,
            5,
            Body::Name {
                name: "parent".into(),
            },
        )
        .sign(&seed, incarnation)
        .unwrap();
    future.insert(parent).unwrap();
    let child = future
        .draft(
            &actor,
            6,
            Body::Name {
                name: "child".into(),
            },
        )
        .sign(&seed, incarnation)
        .unwrap();
    assert_eq!(log.insert(child.clone()).unwrap(), Inserted::Waiting);
    let reads = Reads(&log);
    for view in [
        HistoryView::new(&log),
        HistoryView::with_reader(&log, &reads),
    ] {
        let lease = view.lease_complete().unwrap();
        for held in log.records() {
            let RecordState::Complete(record) = lease.record(held.id()).unwrap() else {
                panic!("a held record is complete")
            };
            assert!(std::ptr::eq(record.record, held));
            assert_eq!(record.generation, lease.generation());
        }
        assert!(matches!(
            lease.record(child.id()).unwrap(),
            RecordState::Pending
        ));
        assert!(matches!(
            lease.record(&[0xff; 32]).unwrap(),
            RecordState::Absent
        ));
    }
}

#[test]
fn a_record_lease_cannot_supply_another_generation() {
    let (log, seed, incarnation, actor) = fixture();
    let mut later = log.clone();
    let record = later
        .draft(
            &actor,
            2,
            Body::Name {
                name: "later".into(),
            },
        )
        .sign(&seed, incarnation)
        .unwrap();
    later.insert(record).unwrap();
    let before = HistoryView::new(&log);
    let after = HistoryView::new(&later);
    let first = before.lease_complete().unwrap();
    let second = after.lease_complete().unwrap();
    let RecordState::Complete(genesis) = first.record(log.network()).unwrap() else {
        panic!("genesis is complete")
    };
    assert!(
        first
            .historical_commons_writers(&genesis)
            .unwrap()
            .is_none()
    );
    assert!(second.historical_commons_writers(&genesis).is_err());
}

#[test]
fn an_acquired_lease_keeps_its_exact_records_without_rereading() {
    use std::cell::Cell;

    struct Reads<'a> {
        log: &'a Log,
        available: Cell<bool>,
        calls: Cell<usize>,
    }
    impl RecordReader for Reads<'_> {
        fn read(&self, id: &RecordId) -> Result<Option<&Record>> {
            self.calls.set(self.calls.get() + 1);
            if !self.available.get() {
                return Err(Error::new("history is no longer readable"));
            }
            Ok(self.log.get(id))
        }
    }

    let (log, _, _, _) = fixture();
    let reads = Reads {
        log: &log,
        available: Cell::new(true),
        calls: Cell::new(0),
    };
    let history = HistoryView::with_reader(&log, &reads);
    let lease = history.lease_complete().unwrap();
    let acquired = reads.calls.get();
    reads.available.set(false);
    let RecordState::Complete(genesis) = lease.record(log.network()).unwrap() else {
        panic!("genesis is complete")
    };
    assert!(
        lease
            .historical_commons_writers(&genesis)
            .unwrap()
            .is_none()
    );
    assert_eq!(reads.calls.get(), acquired);
    assert!(history.lease_complete().is_err());
}
