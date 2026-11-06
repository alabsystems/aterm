// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fallible leases over one immutable, currently in-memory membership history.
//! This boundary changes neither storage nor evaluation and claims no capacity improvement.

use super::{Body, EpochAudience, Log, Record, RecordId, epoch_commitment};
use crate::commons::binding::Binding;
use crate::{Error, Result, age::Identity};
use aterm_digest::Sha256;
use zeroize::Zeroizing;

/// Identity of the exact complete record bytes in their retained causal order.
/// A peer-supplied label or a heads-only digest cannot substitute for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Generation([u8; 32]);

/// Read exact original records. Missing bytes for an indexed complete record are an error.
pub(crate) trait RecordReader {
    fn read(&self, id: &RecordId) -> Result<Option<&Record>>;
}

impl RecordReader for Log {
    fn read(&self, id: &RecordId) -> Result<Option<&Record>> {
        Ok(self.get(id))
    }
}

/// One immutable verified Log and its fallible record provider.
pub(crate) struct HistoryView<'a> {
    log: &'a Log,
    reader: &'a dyn RecordReader,
    generation: Generation,
}

impl<'a> HistoryView<'a> {
    pub(crate) fn new(log: &'a Log) -> Self {
        Self::with_reader(log, log)
    }

    pub(crate) fn with_reader(log: &'a Log, reader: &'a dyn RecordReader) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"aterm/membership/history-generation/1\0");
        digest.update(log.network());
        digest.update((log.held_count() as u64).to_le_bytes());
        for record in log.records() {
            digest.update(record.id());
        }
        Self {
            log,
            reader,
            generation: Generation(digest.finalize()),
        }
    }

    /// Pin every complete record before any query or cache lookup. The returned immutable
    /// references keep those exact bytes available for the lease without further provider I/O.
    pub(crate) fn lease_complete(&self) -> Result<HistoryLease<'_>> {
        let mut records = Vec::with_capacity(self.log.held_count());
        for expected in self.log.records() {
            let Some(record) = self.reader.read(expected.id())? else {
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
            records,
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
    records: Vec<&'a Record>,
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
        let Some(&record) = self.records.get(at) else {
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
            .historical_commons_writers(id)
            .or_else(|| log.historical_fold_writers(id)))
    }

    pub(crate) fn unwrap_commons_epoch(
        &self,
        epoch: &RecordLease<'_>,
        identity: &Identity,
    ) -> Result<Option<Zeroizing<[u8; 32]>>> {
        self.check_record(epoch)?;
        let log = self.view.log;
        if log.is_fold_epoch(epoch.record.id()) {
            return log.unwrap_fold_epoch(epoch.record.id(), identity);
        }
        log.unwrap_commons_epoch(epoch.record.id(), identity)
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
