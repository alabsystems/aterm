// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fallible leases over one immutable, currently in-memory membership history.
//! This boundary changes neither storage nor evaluation and claims no capacity improvement.

use super::{Body, EpochAudience, Log, Record, RecordId, epoch_commitment};
use crate::commons::binding::Binding;
use crate::{
    Error, Result,
    age::{Identity, Recipient},
};
use aterm_digest::Sha256;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

#[cfg(test)]
thread_local! {
    /// The epoch keys [`HistoryLease::unwrap_commons_epoch`] unwrapped on this thread, counted
    /// (never timed).
    pub(crate) static UNWRAPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The epochs of one log's held records whose keys a device opened, by the device's recipient
/// ([`HistoryLease::opened`]). It holds no key, only which epochs opened: whether a device's
/// `Wrap` of an epoch opens and the log names the epoch's writers follows from the held records
/// and the device alone, so the log keeps it until it holds another record, and its clones share
/// it (`Log::hold`).
#[derive(Debug, Default)]
pub(crate) struct Opened(std::sync::Mutex<BTreeMap<Recipient, BTreeSet<RecordId>>>);

impl Opened {
    fn holds(&self, recipient: Recipient, epoch: &RecordId) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&recipient)
            .is_some_and(|epochs| epochs.contains(epoch))
    }

    fn note(&self, recipient: Recipient, epoch: RecordId) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(recipient)
            .or_default()
            .insert(epoch);
    }
}

/// Identity of the exact complete record bytes in their retained causal order.
/// A peer-supplied label or a heads-only digest cannot substitute for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Generation([u8; 32]);

/// Read exact original records. Missing bytes for an indexed complete record are an error.
pub(crate) trait RecordReader {
    fn read(&self, id: &RecordId) -> Result<Option<&Record>>;
}

/// The [`Generation`] of `log`'s held records, derived once for them: the log keeps it until it
/// holds another record, and its clones share it.
fn held_generation(log: &Log) -> Generation {
    *log.generation.get_or_init(|| {
        let mut digest = Sha256::new();
        digest.update(b"aterm/membership/history-generation/1\0");
        digest.update(log.network());
        digest.update((log.held_count() as u64).to_le_bytes());
        for record in log.records() {
            digest.update(record.id());
        }
        Generation(digest.finalize())
    })
}

/// One immutable verified Log and where a lease takes its records from.
pub(crate) struct HistoryView<'a> {
    log: &'a Log,
    /// A fallible provider of the log's records, or `None` for the log's own held records.
    reader: Option<&'a dyn RecordReader>,
    generation: Generation,
}

impl<'a> HistoryView<'a> {
    /// A view of `log`'s held records, which its leases take from the log itself: they are the
    /// records its generation names, so a lease reads none of them back. Each lease of a quiet
    /// repair round looked every record up in the log again, 3.6% of such a round over a log of
    /// 9,605 records (2026-11-08).
    pub(crate) fn new(log: &'a Log) -> Self {
        Self {
            log,
            reader: None,
            generation: held_generation(log),
        }
    }

    /// A view of `log` whose leases read each of its records from `reader`.
    #[cfg(test)]
    pub(crate) fn with_reader(log: &'a Log, reader: &'a dyn RecordReader) -> Self {
        Self {
            log,
            reader: Some(reader),
            generation: held_generation(log),
        }
    }

    /// Pin every complete record before any query or cache lookup. The returned immutable
    /// references keep those exact bytes available for the lease without further provider I/O; a
    /// lease of the log's own records borrows them from the log.
    pub(crate) fn lease_complete(&self) -> Result<HistoryLease<'_>> {
        let Some(reader) = self.reader else {
            return Ok(HistoryLease {
                view: self,
                writers: OnceCell::new(),
                records: None,
            });
        };
        let mut records = Vec::with_capacity(self.log.held_count());
        for expected in self.log.records() {
            let Some(record) = reader.read(expected.id())? else {
                return Err(Error::new("complete membership history is unavailable"));
            };
            if !std::ptr::eq(record, expected)
                && (record.id() != expected.id() || record.bytes() != expected.bytes())
            {
                return Err(Error::new("membership history record changed"));
            }
            records.push(record);
        }
        Ok(HistoryLease {
            view: self,
            writers: OnceCell::new(),
            records: Some(records),
        })
    }
}

pub(crate) enum RecordState<'a> {
    Complete(RecordLease<'a>),
    Pending,
    Absent,
}

/// Original record bytes, borrowed for no longer than their immutable history lease.
pub(crate) struct RecordLease<'a> {
    record: &'a Record,
    generation: Generation,
}

pub(crate) struct HistoryLease<'a> {
    view: &'a HistoryView<'a>,
    /// The log's graph, built once for the lease's epochs, with what every epoch's historical
    /// writers read of it, which the log keeps ([`super::eval::HistoricalWriters`]).
    writers: OnceCell<super::eval::HistoricalWriters<'a>>,
    /// The records a provider gave, pinned for the lease, or `None` where they are the log's own.
    records: Option<Vec<&'a Record>>,
}

impl HistoryLease<'_> {
    pub(crate) fn generation(&self) -> Generation {
        self.view.generation
    }

    pub(crate) fn record(&self, id: &RecordId) -> Result<RecordState<'_>> {
        let Some(at) = self.view.log.record_index(id) else {
            return Ok(if self.view.log.waiting.contains_key(id) {
                RecordState::Pending
            } else {
                RecordState::Absent
            });
        };
        let record = match &self.records {
            None => self.view.log.order.get(at),
            Some(records) => records.get(at).copied(),
        };
        let Some(record) = record else {
            return Err(Error::new("complete membership history is unavailable"));
        };
        Ok(RecordState::Complete(RecordLease {
            record,
            generation: self.generation(),
        }))
    }

    fn check_record(&self, record: &RecordLease<'_>) -> Result<()> {
        if record.generation != self.generation() {
            return Err(Error::new("membership record belongs to another history"));
        }
        if self.view.log.record_index(record.record.id()).is_none() {
            return Err(Error::new("membership record is outside this history"));
        }
        Ok(())
    }

    pub(crate) fn historical_commons_writers(
        &self,
        epoch: &RecordLease<'_>,
    ) -> Result<Option<(u64, Vec<Binding>)>> {
        self.check_record(epoch)?;
        let log = self.view.log;
        let id = epoch.record.id();
        Ok(log
            .historical_commons_writers_in(&self.writers, id)
            .or_else(|| log.historical_fold_writers_in(&self.writers, id)))
    }

    pub(crate) fn unwrap_commons_epoch(
        &self,
        epoch: &RecordLease<'_>,
        identity: &Identity,
    ) -> Result<Option<Zeroizing<[u8; 32]>>> {
        self.check_record(epoch)?;
        #[cfg(test)]
        UNWRAPS.with(|unwraps| unwraps.set(unwraps.get() + 1));
        let log = self.view.log;
        if log.is_fold_epoch(epoch.record.id()) {
            return log.unwrap_fold_epoch(epoch.record.id(), identity);
        }
        log.unwrap_commons_epoch(epoch.record.id(), identity)
    }

    /// Whether `identity` opened `epoch`'s key under this history before: its `Wrap` of the
    /// epoch opened and the log names the epoch's writers ([`HistoryLease::note_opened`]). A log
    /// that holds another record answers afresh.
    pub(crate) fn opened(&self, epoch: &RecordLease<'_>, identity: &Identity) -> Result<bool> {
        self.check_record(epoch)?;
        Ok(self
            .view
            .log
            .opened
            .holds(identity.recipient(), epoch.record.id()))
    }

    /// Note that `identity` opened `epoch`'s key under this history, its writers named
    /// ([`HistoryLease::historical_commons_writers`]) and its key unwrapped
    /// ([`HistoryLease::unwrap_commons_epoch`]), for each later lease of the same records
    /// ([`HistoryLease::opened`]).
    pub(crate) fn note_opened(&self, epoch: &RecordLease<'_>, identity: &Identity) -> Result<()> {
        self.check_record(epoch)?;
        self.view
            .log
            .opened
            .note(identity.recipient(), *epoch.record.id());
        Ok(())
    }

    /// Whether `key` is the key of the counting Commons epoch `epoch` (it matches the
    /// commitment its signed `Epoch` names) and `epoch` is `under` or in its causal past: what a
    /// history handoff of a log-era Commons epoch carries, checked against the original record.
    ///
    /// For a counting `[everyone]` epoch (the fold, B11) the handoff carries the derived network
    /// key, never the `[everyone]` key, which would also open the vault entries that epoch seals
    /// to a Mac it was never wrapped to; the commitment is to the `[everyone]` key, so only the
    /// epoch and the issuance's past are checked here. A wrong key opens no operation (each is
    /// AEAD-sealed) and grants no authorship (each is signed by its writer), so an issuer that
    /// lies can deny the recipient a read, as withholding the handoff would, and nothing more.
    pub(crate) fn commons_epoch_key_under(
        &self,
        epoch: &RecordLease<'_>,
        under: &RecordId,
        key: &[u8; 32],
    ) -> Result<bool> {
        self.check_record(epoch)?;
        let log = self.view.log;
        let (Some(at), Some(top)) = (log.record_index(epoch.record.id()), log.record_index(under))
        else {
            return Ok(false);
        };
        let effective = log.effective();
        if log.is_fold_epoch(epoch.record.id()) {
            return Ok(effective.counts(at) && (at == top || effective.before(at, top)));
        }
        let Body::Epoch {
            audience: EpochAudience::Commons,
            generation,
            commitment,
        } = epoch.record.body()
        else {
            return Ok(false);
        };
        Ok(effective.counts(at)
            && (at == top || effective.before(at, top))
            && aterm_digest::ct_eq(
                &epoch_commitment(log.network(), &EpochAudience::Commons, *generation, key),
                commitment,
            ))
    }
}

#[cfg(test)]
mod tests {
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
}
