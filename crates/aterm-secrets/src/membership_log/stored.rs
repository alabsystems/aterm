// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a rewritten records file holds (`docs/DESIGN-vault-log-2026-11-14.md` step 4, tracker
//! FL-148D): a store's records, and ahead of them, for a log rooted at a final checkpoint
//! ([`super::rooted`]), its roots and the stubs of the records they dropped. A store read back
//! from the file is the store that was written: the same records in the same order, the same
//! stubs and roots, and so the same evaluation. A Mac that reads format 2 writes such a file
//! where it takes a served root (`client::persist_stored`, through
//! `crate::records_storage::rewrite`) and reads it back (`client::records_cache`); no running
//! build reads format 2, so none does either.
//!
//! **The file** ([`Store::write_stored`]): for a rooted log one frame ([`Kept`]), then the
//! records' stream as every records file holds it ([`Store::write_encoded`]). A whole log's
//! file has no frame and is its records' stream alone, as it always was. The frame:
//!
//! | bytes | field |
//! |---|---|
//! | 4 | `ALK1`: no record's magic, and no served frame's |
//! | 8 | the length of the rest of the frame |
//! | 32 | the network |
//! | 2, then each | the roots: each its body's clear fields, then a `u32` count of the ids its prune dropped and those ids, ascending |
//! | 2, then each | the earlier roots kept beside them ([`super::Rooting::earlier`]), as the roots |
//! | 4, then each | the stubs, each in its one encoding ([`Stub::encode`]), ascending by id |
//!
//! Decided here, and why:
//!
//! - **The frame comes first.** Records are appended to a records file between rewrites, and
//!   a rooting changes only by a prune, which is a rewrite: so the frame is written once, at
//!   the file's start, and every later write is an append, as before. A frame after the
//!   records would have to be moved by every append.
//! - **A root is kept whole**, its body and its dropped ids, not as an id to find a share by
//!   ([`super::served`] sends ids, since its receiver checks the shares): a log may be rooted
//!   at a body of which it holds no share ([`Log::rooted_at`]), and a file must read back
//!   without the records' help.
//! - **What a read checks.** Each root's `dropped` digest is the digest of the stubs kept for
//!   the ids it dropped ([`checkpoint::dropped_digest`]), and every stub is one a kept root
//!   dropped, so no field of a stub and no stub is in the file that a root's body does not
//!   bind. A file that fails either is refused whole, as is one whose frame does not read
//!   exactly: without its stubs a rooted log's records do not place, so there is no part of it
//!   to keep. The records after the frame are read as a records file's are
//!   ([`Store::recovering`]): each that still verifies is kept, so an append cut short costs
//!   its own record and nothing before it.
//! - **What a read does not check.** It judges no root final and verifies no stub's signature.
//!   Finality lapses (a later `Remove` un-counts a share), and a file that had to be final to
//!   read would stop reading; the Mac judged the root before it pruned there
//!   ([`Log::adopt`], [`Log::take_served`]). The file is this Mac's own, private to it, and
//!   every byte of the frame was held to the digest its rewrite declared before the pin moved
//!   to it. A stub's frame is still read as the record it is, under the reader's format, so a
//!   reader of format 1 refuses a version-2 record's stub as it refuses the record.
//! - **The vault's head of such a file keeps the frame too** ([`super::head`]): after its
//!   records and ops, behind the frame's length, and only for a rooted log, so a whole log's
//!   head is what it was. An open through the head places each record by the stubs as a full
//!   read does, and each record's place in the file starts after the frame. A head is still
//!   written only by a resolve of a read that began once the file had settled, never by the
//!   rewrite.
//! - **A store that cannot be read back is not written.** [`Kept::of`] holds a log to the same
//!   two laws before it encodes it, so a rewrite never replaces a readable file with one its
//!   own read refuses.
//! - **The read is of what its store holds.** [`Store::recovering_stored`] takes no more of a
//!   file than a store of its reader's format holds ([`Store::capacity`], the storage class of
//!   byte bound, [`crate::crossing::capacity`]), and everything it reads through reads the same
//!   capacity: the strict decode of the stream ([`super::decode_records_in`]), its log's byte
//!   bound ([`Log::capacity`], which [`Log::insert`] and `make_room` hold), and the head's.
//!   None is held to one reply's bytes ([`super::REPLY_CAP`], the wire's class), so a capacity
//!   past a reply (tracker FL-249) is read back whole. The capacity is of the log's records,
//!   waiting records and stubs; the frame's roots are the file's alone, which a writer of this
//!   file holds to the same bound (`crate::records_storage::rewrite`).

use super::checkpoint::{self, checkpoint_id};
use super::rooted::{Cursor, Root, Rooting, Stub};
use super::served::MIN_STUB_WIRE;
use super::{Body, Kind, Log, MAX_CHECKPOINT_HEADS, Reading, RecordId, Store};
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// What a rooted log's frame begins with ([`Kept::encode`]).
pub(crate) const KEPT_MAGIC: &[u8; 4] = b"ALK1";

/// A broken reading of a kept frame, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::membership_log) enum Mutant {
    /// A root's `dropped` digest is not held to the stubs kept for it.
    Unbound,
    /// A stub no kept root dropped is read too.
    Uncovered,
    /// The vault's head keeps no frame: a rooted log's head holds its records alone.
    HeadWithoutFrame,
    /// A read passes over the frame and takes the records after it as a whole log's.
    FrameSkipped,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's kept frames are read as, if any.
    pub(in crate::membership_log) static MUTANT: std::cell::Cell<Option<Mutant>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(in crate::membership_log) fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

/// A rooted log's roots and stubs, as its records file keeps them ahead of its records.
#[derive(Clone, Debug)]
pub(crate) struct Kept {
    /// The network the log is of: its genesis's id.
    pub(super) network: RecordId,
    /// What the log keeps of the checkpoints it was pruned at.
    rooting: Rooting,
}

impl Kept {
    /// What `log`'s records file keeps of its rooting; `None` for a whole log.
    ///
    /// # Errors
    /// The rooting is not one a read takes ([`Kept::check`]): then no file is written from it.
    pub(crate) fn of(log: &Log) -> Result<Option<Self>> {
        let Some(rooting) = log.rooting() else {
            return Ok(None);
        };
        let kept = Self {
            network: *log.network(),
            rooting: rooting.clone(),
        };
        kept.check()?;
        Ok(Some(kept))
    }

    /// The rooting it keeps.
    pub(super) fn into_rooting(self) -> Rooting {
        self.rooting
    }

    /// The module's two laws: each root's digest names the stubs kept for what it dropped, and
    /// every stub is one a kept root dropped.
    fn check(&self) -> Result<()> {
        let rooting = &self.rooting;
        if rooting.roots.is_empty() {
            return Err(Error::new("a rooted log keeps a root"));
        }
        let mut bound: BTreeSet<&RecordId> = BTreeSet::new();
        for root in rooting.roots.iter().chain(&rooting.earlier) {
            let Body::Checkpoint { dropped, .. } = &root.body else {
                return Err(Error::new("a kept root is a checkpoint"));
            };
            let mut stubs = BTreeMap::new();
            for id in &root.dropped {
                let stub = rooting
                    .stubs
                    .get(id)
                    .ok_or_else(|| Error::new("a kept root dropped a record no stub stands for"))?;
                stubs.insert(*id, stub.clone());
                bound.insert(id);
            }
            #[cfg(test)]
            if mutant(Mutant::Unbound) {
                continue;
            }
            if checkpoint::dropped_digest(&stubs) != *dropped {
                return Err(Error::new(
                    "a kept root's digest does not name the stubs kept for it",
                ));
            }
        }
        #[cfg(test)]
        if mutant(Mutant::Uncovered) {
            return Ok(());
        }
        if rooting.stubs.keys().any(|id| !bound.contains(id)) {
            return Err(Error::new("a kept stub no kept root dropped"));
        }
        Ok(())
    }

    /// The frame, in its one encoding (the module's table).
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&self.network);
        for roots in [&self.rooting.roots, &self.rooting.earlier] {
            body.extend_from_slice(&u16::try_from(roots.len()).unwrap_or(u16::MAX).to_be_bytes());
            for root in roots {
                body.extend_from_slice(&super::encode_body(&root.body));
                body.extend_from_slice(
                    &u32::try_from(root.dropped.len())
                        .unwrap_or(u32::MAX)
                        .to_be_bytes(),
                );
                for id in &root.dropped {
                    body.extend_from_slice(id);
                }
            }
        }
        body.extend_from_slice(
            &u32::try_from(self.rooting.stubs.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for (id, stub) in &self.rooting.stubs {
            stub.encode(id, &mut body);
        }
        let mut out = Vec::with_capacity(KEPT_MAGIC.len() + 8 + body.len());
        out.extend_from_slice(KEPT_MAGIC);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// The frame [`Kept::encode`] wrote, at the start of `bytes`, and how many bytes it takes:
    /// each stub read as `reading` reads its record ([`Stub::decode`]), and the whole held to
    /// the module's laws ([`Kept::check`]).
    ///
    /// # Errors
    /// `bytes` does not begin with [`KEPT_MAGIC`], ends inside the frame, holds a root that is
    /// no checkpoint, roots or ids or stubs out of ascending order or twice, a stub [`Stub`]
    /// cannot read, or bytes the frame's length counts and its fields do not; or the frame
    /// fails [`Kept::check`].
    pub(crate) fn decode(bytes: &[u8], reading: Reading) -> Result<(Self, usize)> {
        let mut at = 0usize;
        let mut read = Cursor { bytes, at: &mut at };
        if read.take(KEPT_MAGIC.len())? != KEPT_MAGIC {
            return Err(Error::new("a kept frame begins with ALK1"));
        }
        let len = usize::try_from(read.u64()?).unwrap_or(usize::MAX);
        let end = at
            .checked_add(len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| Error::new("a kept frame is truncated"))?;
        let bytes = &bytes[..end];
        let mut read = Cursor { bytes, at: &mut at };
        let network = read.id()?;
        let roots = Self::roots(&mut read)?;
        let earlier = Self::roots(&mut read)?;
        let mut count = [0u8; 8];
        count[4..].copy_from_slice(read.take(4)?);
        let count = usize::try_from(u64::from_be_bytes(count)).unwrap_or(usize::MAX);
        if count.saturating_mul(MIN_STUB_WIRE) > bytes.len().saturating_sub(at) {
            return Err(Error::new("a kept frame counts more stubs than it holds"));
        }
        let mut stubs = BTreeMap::new();
        let mut last: Option<RecordId> = None;
        for _ in 0..count {
            let (id, stub) = Stub::decode(bytes, &mut at, &network, reading)?;
            if last.is_some_and(|last| last >= id) {
                return Err(Error::new(
                    "a kept frame holds its stubs once each, ascending",
                ));
            }
            last = Some(id);
            stubs.insert(id, stub);
        }
        if at != end {
            return Err(Error::new("a kept frame has bytes its fields do not count"));
        }
        let kept = Self {
            network,
            rooting: Rooting {
                roots,
                stubs,
                earlier,
            },
        };
        kept.check()?;
        Ok((kept, end))
    }

    /// A counted list of roots, ascending by id, each once.
    fn roots(read: &mut Cursor<'_, '_>) -> Result<Vec<Root>> {
        let mut count = [0u8; 8];
        count[6..].copy_from_slice(read.take(2)?);
        let count = usize::try_from(u64::from_be_bytes(count)).unwrap_or(usize::MAX);
        let mut roots: Vec<Root> = Vec::new();
        for _ in 0..count {
            let predecessor = Some(read.id()?).filter(|id| *id != [0; 32]);
            let cut_ms = read.u64()?;
            let heads = read.ids(2, MAX_CHECKPOINT_HEADS)?;
            let (state, dropped) = (read.id()?, read.id()?);
            if heads.is_empty()
                || !super::strictly_sorted_nonzero(&heads)
                || state == [0; 32]
                || dropped == [0; 32]
            {
                return Err(Error::new("a kept root is no checkpoint"));
            }
            let body = Body::Checkpoint {
                predecessor,
                cut_ms,
                heads,
                state,
                dropped,
            };
            let id =
                checkpoint_id(&body).ok_or_else(|| Error::new("a kept root is no checkpoint"))?;
            let most = read.bytes.len().saturating_sub(*read.at) / size_of::<RecordId>();
            let dropped = read.ids(4, most)?;
            if dropped.windows(2).any(|pair| pair[0] >= pair[1])
                || roots.last().is_some_and(|last| last.id >= id)
            {
                return Err(Error::new(
                    "a kept frame holds its roots and their dropped ids once each, ascending",
                ));
            }
            roots.push(Root { id, body, dropped });
        }
        Ok(roots)
    }
}

impl Store {
    /// The bytes before the first record of the records file this store was recovered from
    /// ([`Store::recovering_stored`]): the length of a rewritten file's frame, 0 for a file
    /// with none and for a store recovered from no file.
    #[cfg(any(unix, windows))]
    pub(crate) const fn file_lead(&self) -> u64 {
        self.file_lead
    }

    /// The frame this store's rewritten records file begins with ([`Kept`]); `None` for a store
    /// whose log is whole, or that holds none.
    ///
    /// # Errors
    /// Its log's rooting is not one a read takes ([`Kept::of`]).
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "only a Unix member writes a rewritten records file (`client::persist_stored`)"
        )
    )]
    pub(crate) fn kept_frame(&self) -> Result<Option<Vec<u8>>> {
        let Some(log) = &self.log else {
            return Ok(None);
        };
        Ok(Kept::of(log)?.map(|kept| kept.encode()))
    }

    /// Write this store's rewritten records file (the module's file): its frame where its log
    /// is rooted, then every record as [`Store::write_encoded`] writes them.
    ///
    /// # Errors
    /// As [`Store::kept_frame`] and [`Store::write_encoded`]: then the file is not whole.
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "only a Unix member writes a rewritten records file (`client::persist_stored`)"
        )
    )]
    pub(crate) fn write_stored<W: std::io::Write + ?Sized>(
        &self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        let frame = self.kept_frame().map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        })?;
        if let Some(frame) = frame {
            writer.write_all(&frame)?;
        }
        self.write_encoded(writer)
    }

    /// How long the file [`Store::write_stored`] writes is: its frame's bytes and those of
    /// every held and waiting record. A rooted log's stubs are in the frame and are counted
    /// there alone: the log's own byte bound ([`Store::encoded_len`]) counts them beside the
    /// records, so it is not the file's length.
    ///
    /// # Errors
    /// As [`Store::kept_frame`].
    #[cfg_attr(
        not(any(unix, test)),
        expect(
            dead_code,
            reason = "only a Unix member writes a rewritten records file (`client::persist_stored`)"
        )
    )]
    pub(crate) fn stored_len(&self) -> Result<usize> {
        let frame = self.kept_frame()?.map_or(0, |frame| frame.len());
        let records = self.log.as_ref().map_or_else(
            || self.encoded_len(),
            |log| log.held_bytes().saturating_add(log.waiting_bytes()),
        );
        Ok(frame.saturating_add(records))
    }

    /// [`Store::recovering`] for a rewritten records file ([`Store::write_stored`]), into this
    /// empty store: a file that begins with no frame is read as `recovering` reads it, and one
    /// that does is read into a log rooted as its frame says, its records each after the one
    /// before, so the store is the one that was written.
    ///
    /// # Errors
    /// The file begins with a frame that [`Kept::decode`] refuses, that is not followed by the
    /// genesis of its network, or that this store, which is not empty, cannot take: nothing of
    /// the file is kept.
    pub(crate) fn recovering_stored(mut self, bytes: &[u8]) -> Result<Self> {
        let bytes = &bytes[..bytes.len().min(self.capacity())];
        if !bytes.starts_with(KEPT_MAGIC) {
            return Ok(self.recovering(bytes));
        }
        let (kept, lead) = Kept::decode(bytes, self.reading())?;
        #[cfg(test)]
        if mutant(Mutant::FrameSkipped) {
            return Ok(self.recovering(&bytes[lead..]));
        }
        let (records, strict) = self.decoded(&bytes[lead..]);
        #[cfg(any(unix, windows))]
        let order: Option<std::sync::Arc<[(RecordId, usize)]>> = strict.then(|| {
            records
                .iter()
                .map(|record| (*record.id(), record.wire_len()))
                .collect()
        });
        #[cfg(not(any(unix, windows)))]
        let _ = strict;
        let mut records = records.into_iter();
        let genesis = records
            .next()
            .filter(|record| record.kind() == Kind::Genesis && *record.id() == kept.network)
            .ok_or_else(|| Error::new("a rooted records file holds its genesis after its frame"))?;
        if !self.is_empty() {
            return Err(Error::new("a records file is read into an empty store"));
        }
        self.insert(genesis)?;
        let log = self
            .log
            .as_ref()
            .ok_or_else(|| Error::new("a rooted records file holds its genesis after its frame"))?;
        let mut rooted = log.rebuilt_with(kept.into_rooting(), &BTreeSet::new(), records)?;
        super::store::make_room(&mut rooted);
        self.log = Some(rooted);
        #[cfg(any(unix, windows))]
        {
            self.file_order = order;
            self.file_lead = lead as u64;
        }
        Ok(self)
    }
}

#[cfg(test)]
#[path = "stored_tests.rs"]
pub(crate) mod tests;
