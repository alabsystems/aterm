// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a log rooted at a final checkpoint ([`super::rooted`]) serves a peer that lacks its cut,
//! and how that peer takes it (`docs/DESIGN-vault-log-2026-11-14.md` step 4, trackers FL-148c
//! and FL-148C2):
//! so a Mac behind the cut catches up from a pruned peer, and a record that follows only a record
//! the other side of a split dropped is placed once that side's root is taken. Nothing in this
//! build prunes, so no store is rooted and none serves this: it lands dark.
//!
//! - **What is served** ([`Served`]): beside the records a peer lacks
//!   ([`super::Store::missing_served`]), to a peer that says it takes a frame and whose `have`
//!   does not show every root's cut, the roots' ids and the stub of every record a root dropped
//!   ([`Log::served`]). A root's body is a share's ([`Body::Checkpoint`]),
//!   and its settling shares are records past its cut, which a rooted log keeps and serves as it
//!   serves any record. A log with no root serves byte for byte what it served before. A log
//!   pruned again keeps as a stub a record an earlier root dropped that the later root's whole
//!   log keeps (a rep a kept op carries, a kept `Remove`'s cutoff), which no current root's
//!   digest binds: it keeps that earlier root beside its roots ([`super::Rooting::earlier`]) and
//!   serves its id with theirs, so a Mac behind both cuts takes both, whichever first (a prune at
//!   a root a taken root nests nests it, [`Log::pruned`]), and reads that stub by the earlier
//!   root's digest.
//! - **How it is authenticated.** A checkpoint's `dropped` digest hashes each dropped record's
//!   stub ([`checkpoint::dropped_digest`], [`super::Stub::encode`]), so its shares sign the stubs.
//!   A version-2 record's stub is more: its frame ([`Stub::frame`]) is the record's shed frame
//!   past the network, whose SHA-256 is the record's id and which its signer signed, so a
//!   receiver checks every field of it against the id it is served under ([`Stub::checked`]),
//!   and its one field no frame holds, a `Delete`'s `carries`, by reading the `Delete`'s
//!   statement again at its stub ([`Log::stub_delete_rep`]): a reading of the frames and stubs
//!   in its past alone, each of which it checks so too. A receiver that holds the root's cut itself (its own records, with what the
//!   reply brought) takes no stub from the peer: it adopts the root as [`Log::adopt`] does, by
//!   recomputing it. One that does not reads the peer's stubs only where it neither holds nor
//!   dropped the record, and takes the root only where, with them, it computes that very body at
//!   the root's heads ([`checkpoint::plan_at`]: the stubs' digest, the state digest and the
//!   predecessor), the shares it holds make it final there by [`checkpoint::judge`]'s rule (more
//!   than half of the recent seats at the cut, each share a record that counts; settled, for a
//!   root another served root nests, below: "Earlier roots are settled") and, where a stub
//!   it read in the cut is a version-1 record's, are more than half of its seats too (below), and
//!   every stub it read is one a root it took dropped. Then it
//!   prunes there itself ([`Log::rooted_at`]'s
//!   prune), as a Mac that holds the cut does: its own records the root drops become stubs made
//!   from those records, never the peer's. A forged stub field, a frame that is not its stub's
//!   record, a stub set whose digest is not the body's, a root that is not final by its shares,
//!   and a stub no root drops each refuse the whole reply, and nothing of it is taken.
//! - **What that trusts.** A receiver that does not hold the cut cannot check a version-1
//!   record's stub against its record (its id hashes its ciphertext, which no stub keeps): its
//!   fields are bound only by the digest the shares sign. And the recent seats
//!   whose shares make the root final are read off the same stubs (a dropped record's signer and
//!   clock), so the signers would name who counts: one seat that relabels another's dropped write
//!   as its own, and places it behind its own record, is the only recent seat of a cut it chose,
//!   and its one share makes final a root that drops the other seat's write, which no whole log
//!   holding that write could adopt ([`Log::insert`] refuses the record that follows it as the
//!   signer's). So a root read through a version-1 record's stub is taken only where its shares
//!   are also more than half of the receiver's seats: the seats are the weighed records', which a prune keeps whole
//!   and the receiver holds signed, so no stub shrinks them (a `Remove` whose cutoff is read off a
//!   stub takes out only a Mac its signer could take out at a real cutoff), and a forged stub
//!   then needs a majority of every seat behind it. That is the cost
//!   [`crate::commons::finality`] states for a newcomer that trusts the shares, with the seats it
//!   counts out of the signers' hands; a Mac
//!   that holds the cut recomputes and is not exposed. Where every stub it reads in the cut is a
//!   version-2 record's, the stubs say nothing their signers did not sign: the recent seats are
//!   the records' own, as a Mac that holds the cut reads them, so the receiver takes the root on
//!   [`checkpoint::judge`]'s rule alone, as that Mac does, and a relabelled stub is refused as
//!   not its frame's. The price, recorded as the cost of a version-1 drop: a Mac behind a cut
//!   whose prune dropped a version-1 record, in a vault whose live seats are half or fewer of its
//!   seats (dead seats outnumbering live ones, or the minority side of a split), takes no root
//!   read through stubs, and waits on a peer that holds the cut; FL-148f's writers sign version
//!   2, so a cut of their records drops none. The relabelling cases are `served_tests.rs`'s
//!   `vault_checkpoint_one_seat_cannot_certify_the_stubs_it_serves` (version 1) and
//!   `vault_checkpoint_a_forged_version_two_stub_is_refused`; the minority cases its
//!   `vault_checkpoint_a_mac_behind_a_minority_cut_catches_up_from_self_checking_stubs`.
//! - **The wire.** A reply is the records stream ([`super::read_records`]) and then, where a root
//!   is served, one frame ([`Served::encode`]): `ALS1`, the network (32), a `u16` count of roots
//!   and their ids, a `u32` count of stubs and each stub's encoding ([`Stub::encode`]: a
//!   version-1 record's tagged `1`, a version-2 record's tagged `2` with its frame), ascending
//!   by id. [`super::decode_reply`] reads
//!   both; [`super::decode_records`] refuses the frame as a record of no layout. A frame goes
//!   only to a pull that says it takes one ([`crate::transport::RECORDS_SERVED_HEADER`], which no
//!   client of this build sends), never on [`Log::named_cuts`]' judgement alone: a `have` names a
//!   head and ancestors 1, 2, 4, 8 … back, so a whole Mac past the server that names records the
//!   server lacks can read as lacking a cut it holds, and a reader of records alone would refuse
//!   the whole reply.
//! - **Bounds.** A frame names at most [`MAX_SERVED_ROOTS`] roots (a log with more serves none)
//!   and as many stubs as its bytes hold ([`MIN_STUB_WIRE`] each at least): stubs count against a
//!   log's bytes and no count, so a log that dropped more records than it may hold whole still
//!   serves them all, within the reply's stream bound ([`super::Store::missing_served`]). A
//!   receiver verifies one signature per stub it reads, at most one per [`MIN_STUB_WIRE`] bytes
//!   of the reply, as it verifies one per record of the records stream.
//!
//! Decided here, and why:
//! - **A stub says nothing of its cut.** It keeps what its record says, all of it bound; whether
//!   it counts in a cut is read there, from the checkpoints settled in that cut
//!   ([`checkpoint::gone`]), by the receiver as by its peer, and is neither served nor digested:
//!   a digest over it would part an earlier body between a log pruned once and one pruned twice
//!   (the random-DAG prune test, seed 1, failed so when FL-148C bound such a flag). The
//!   digest's domain moved to `/2`, and to `/3` with the stubs' version tag and frames: no
//!   build signs a checkpoint.
//! - **A version-2 stub is its frame.** Its encoding, and so the digest, is the record's shed
//!   frame past the network rather than its fields: the fields are read off the frame, so a stub
//!   whose fields and frame disagree is no stub any Mac makes, and is refused. The magic, version
//!   and network are left out of each stub (the network goes once in the frame) and the id too
//!   (the frame's SHA-256), so a version-2 stub takes at least 2 bytes fewer than its shed
//!   frame, which is the whole of a share, and a stub stays smaller than its record, whole or
//!   shed ([`Stub::bytes`]); its fields read off the frame count in the memory footprint alone.
//!   A frame is read under the reader's format as its bytes are ([`Served::decode`]), so a
//!   reader of format 1 refuses a version-2 record's stub as it refuses the record, and is
//!   verified, signature and all, once, where the receiver reads it ([`Stub::checked`]), which a
//!   [`Served`] handed in memory passes too: a stub of a record the receiver holds or dropped,
//!   which it ignores, costs it no signature check.
//! - **An earlier root is served, not a self-checking stub alone.** A version-2 stub proves its
//!   fields, not that a final checkpoint dropped its record: taken without a root that dropped
//!   it, a peer could turn into a stub a record the whole log keeps (a winner, a carry), whose
//!   value the receiver then never holds. So a stub an earlier root dropped and the later root's
//!   whole log keeps is read only by that earlier root, whatever its version: its body is a share
//!   the log holds (the later cut keeps the earlier one's shares where it does not hold them
//!   settled, and a settled cut's drops are gone and dropped again), its dropped set is what the
//!   receiver recomputes.
//! - **Earlier roots are settled.** A served root another served root nests (an earlier root,
//!   [`super::Rooting::earlier`]) is taken where it is settled by the shares the receiver holds
//!   ([`checkpoint::Judgment::is_settled`]: more than half of its recent seats signed it, whether
//!   or not each share still counts), the newest where it is final: its peer took it final and
//!   keeps it while it stays settled, which no later record undoes, where finality a later
//!   `Remove` of a signer undoes. Judged final, a Mac behind both cuts would refuse every reply
//!   of a peer whose earlier root lapsed so, until a later prune's cut held it settled; and a
//!   whole log reads what a settled checkpoint dropped as gone ([`checkpoint::gone`]), so the
//!   receiver reads its stubs as that log does. Where a stub it reads in the cut is a version-1
//!   record's, the settling shares are also more than half of the receiver's seats, as the
//!   newest's final ones are.
//! - **Order.** The roots are taken by their cuts' days, then ids, but a day says nothing of
//!   what a cut nests (a cut need only follow heads authored before its day): what the receiver
//!   holds does not hang on the order, since a prune at a root a root it took nests nests it.
//! - **The network once.** A version-2 stub's id hashes its network, which every stub of a log
//!   shares: the frame names it once, and a receiver refuses a frame of another network.
//! - **Roots go by id.** A root's body travels as its shares, records the receiver checks as any
//!   record, so the frame adds nothing a share does not sign.
//! - **The receiver's own records win.** A stub read for a record it holds or dropped is
//!   ignored, and a receiver that holds the cut reads none; so a peer can add nothing to what a
//!   receiver could compute itself.
//! - **No floor from the receiver's own past.** It judges recency at the served cut as
//!   [`Log::adopt`] does; a stricter rule from its own older view would part two receivers'
//!   judgements of one root, which finality must not.
//! - **One reply, all or nothing.** [`super::Store::ingest_served`] takes the records and roots
//!   into a candidate and replaces the store only when every root is taken, so a refused reply
//!   leaves the store as it was. So a reply that serves a root is not cut at the reply cap
//!   ([`super::Store::missing_served`]): the root's settling shares follow its cut and may wait,
//!   through the records it dropped, on its stubs, and a prefix cut short of them would be
//!   refused, and served again, the same, on every pull. The recent-seat rule leans on it too: a
//!   root judged on its recent seats alone is judged in a log that also holds the version-1
//!   stubs read for another served root's cut, which stand on their signers' word alone; that
//!   is sound only because the root whose cut holds them must pass the all-seats rule as well,
//!   and its refusal takes nothing of the reply.
//! - **A pull naming a dropped record lacks nothing.** [`super::Store::lacks_any`] reads a
//!   record a root dropped as held, so a server does not ask a whole Mac whose `have` names one
//!   to post records it would refuse as known, on every pull.

use super::checkpoint::{self, checkpoint_id};
use super::rooted::{Cursor, Rooting, Stub};
use super::{Body, Kind, Log, MemberId, Reading, Record, RecordId};
use crate::commons::finality;
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

/// What a served frame begins with ([`Served::encode`]): no record's magic.
pub const SERVED_MAGIC: &[u8; 4] = b"ALS1";

/// The most roots one frame names: a log's roots are one per side of a split it holds, and a log
/// with more serves none ([`Log::served`]).
pub const MAX_SERVED_ROOTS: usize = 256;

/// The fewest bytes a stub's encoding takes ([`Stub::encode`]: a version-1 record's tag, id,
/// signer, sequence, clock, kind, and every optional field and list empty; a version-2
/// record's frame alone is longer), by which a frame's length bounds its count.
pub const MIN_STUB_WIRE: usize = 1 + 32 + 48 + 8 + 1 + 8 + 1 + 1 + 1 + 2 + 1 + 2;

/// What a rooted log serves beside the records a peer lacks ([`Log::served`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Served {
    /// The network its log is of ([`Log::network`]): what a version-2 record's stub, which keeps
    /// its frame past the network ([`Stub::frame`]), is read and checked under.
    pub network: RecordId,
    /// Its roots' ids ([`checkpoint_id`] of their bodies), ascending: the log's roots and the
    /// earlier roots it keeps beside them ([`super::Rooting::earlier`]).
    pub roots: Vec<RecordId>,
    /// The stub of each record its roots dropped, by id.
    pub stubs: BTreeMap<RecordId, Stub>,
}

impl Served {
    /// Its one encoding, as a reply's last frame (the module's wire).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.wire_len());
        out.extend_from_slice(SERVED_MAGIC);
        out.extend_from_slice(&self.network);
        out.extend_from_slice(
            &u16::try_from(self.roots.len())
                .unwrap_or(u16::MAX)
                .to_be_bytes(),
        );
        for root in &self.roots {
            out.extend_from_slice(root);
        }
        out.extend_from_slice(
            &u32::try_from(self.stubs.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        for (id, stub) in &self.stubs {
            stub.encode(id, &mut out);
        }
        out
    }

    /// The bytes [`Served::encode`] writes.
    #[must_use]
    pub fn wire_len(&self) -> usize {
        SERVED_MAGIC.len()
            + size_of::<RecordId>()
            + 2
            + self.roots.len() * size_of::<RecordId>()
            + 4
            + self.stubs.values().map(Stub::wire_len).sum::<usize>()
    }

    /// The frame [`Served::encode`] wrote, which is the whole of `bytes`, read as `reading` reads
    /// a record: a reader of format 1 refuses a version-2 record's stub as it refuses the record
    /// ([`Stub::decode`]). No frame's signature is verified here; [`Log::take_served`] verifies
    /// each stub it reads, once.
    ///
    /// # Errors
    /// `bytes` does not begin with [`SERVED_MAGIC`], names no root or more than
    /// [`MAX_SERVED_ROOTS`], counts more stubs than its bytes can hold, names roots or stubs out
    /// of ascending order or twice, holds a stub [`Stub`] cannot read (a version-2 record's whose
    /// frame is no record of the frame's network that `reading` reads), or trails bytes past
    /// them.
    pub fn decode(bytes: &[u8], reading: Reading) -> Result<Self> {
        let mut at = 0usize;
        let mut read = Cursor { bytes, at: &mut at };
        if read.take(SERVED_MAGIC.len())? != SERVED_MAGIC {
            return Err(Error::new("a served frame begins with ALS1"));
        }
        let network = read.id()?;
        let roots = read.ids(2, MAX_SERVED_ROOTS)?;
        if roots.is_empty() || roots.windows(2).any(|w| w[0] >= w[1]) {
            return Err(Error::new(
                "a served frame names its roots once each, ascending",
            ));
        }
        let mut count = [0u8; 8];
        count[4..].copy_from_slice(read.take(4)?);
        let count = usize::try_from(u64::from_be_bytes(count)).unwrap_or(usize::MAX);
        // Stubs count against a log's bytes and no count ([`super::rooted`]'s bounds), so the
        // frame's own length bounds them: each takes at least [`MIN_STUB_WIRE`] bytes.
        if count.saturating_mul(MIN_STUB_WIRE) > bytes.len().saturating_sub(at) {
            return Err(Error::new("a served frame counts more stubs than it holds"));
        }
        let mut stubs = BTreeMap::new();
        let mut last: Option<RecordId> = None;
        for _ in 0..count {
            let (id, stub) = Stub::decode(bytes, &mut at, &network, reading)?;
            if last.is_some_and(|last| last >= id) {
                return Err(Error::new(
                    "a served frame holds its stubs once each, ascending",
                ));
            }
            last = Some(id);
            stubs.insert(id, stub);
        }
        if at != bytes.len() {
            return Err(Error::new("a served frame has trailing bytes"));
        }
        Ok(Self {
            network,
            roots,
            stubs,
        })
    }
}

/// A broken taking of a served root, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// The `dropped` digest hashes the dropped ids alone, as FL-148b's did: no stub field is
    /// bound ([`checkpoint::dropped_digest`]).
    IdsOnly,
    /// A root taken through the stubs is not judged: neither its body recomputed nor its shares
    /// counted.
    Unjudged,
    /// A served stub no root taken drops is read too.
    Uncovered,
    /// A root taken through the stubs is final by its recent seats alone, as [`Log::adopt`]
    /// judges one, which the stubs' own signers name.
    RecentAlone,
    /// A served root's reply is cut at the reply cap, its frame counted first, as the first
    /// cut of this slice served it ([`super::Store::missing_served`]).
    CappedServed,
    /// A pull's `have` is lacked where it names a record this store does not hold, dropped or
    /// not ([`super::Store::lacks_any`]), as the server read it before.
    HeldHave,
    /// A served version-2 record's stub is read as served, its fields not checked against its
    /// frame ([`Stub::checked`]).
    UncheckedFrames,
    /// A served version-2 `Delete`'s stub keeps the `carries` it was served with, not read again
    /// off its statement ([`Log::stub_delete_rep`]).
    DeleteRepsRead,
    /// Every root read through stubs needs the shares of more than half of all seats, as
    /// FL-148c's rule: self-checking stubs too.
    AllSeats,
    /// A served root another served root nests is judged as the newest is, final by the shares
    /// that still count, not settled.
    NestedFinal,
    /// A served frame's version-2 stubs are read as [`Reading::Follow`] reads them, whatever
    /// the reader's format, as this slice's first cut read them ([`Stub::decode`]).
    StubsFollow,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's served roots are taken as, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(in crate::membership_log) fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

/// Whether `log`, which reads the root `id`'s cut through served stubs, takes it: it computes
/// `plan` there, whose body is the root's (its stubs' digest, its state and its predecessor),
/// [`checkpoint::judge`] finds the root final by the shares `log` holds (or, where another
/// served root nests it, `nested`, settled by them), and, unless every stub it read in the cut
/// checks itself against its id (`self_checked`), those shares are more than half of `log`'s
/// seats too (the module's "What that trusts").
///
/// # Errors
/// As [`checkpoint::judge`].
fn judged(
    log: &Log,
    id: RecordId,
    plan: &checkpoint::Plan,
    body: &Body,
    self_checked: bool,
    nested: bool,
) -> Result<bool> {
    let judgment = checkpoint::judge(log, body)?;
    let seats: BTreeSet<MemberId> = log.membership_ref().members().keys().copied().collect();
    #[cfg(test)]
    let self_checked = self_checked && !mutant(Mutant::AllSeats);
    #[cfg(test)]
    let nested = nested && !mutant(Mutant::NestedFinal);
    let (held, signed) = if nested {
        (judgment.is_settled, &judgment.shared)
    } else {
        (judgment.is_final, &judgment.signed)
    };
    #[cfg(test)]
    let seated = mutant(Mutant::RecentAlone) || self_checked || finality::is_final(&seats, signed);
    #[cfg(not(test))]
    let seated = self_checked || finality::is_final(&seats, signed);
    Ok(checkpoint_id(&plan.body) == Some(id) && held && seated)
}

impl Log {
    /// What this log serves a peer that lacks a root's cut, beside the records it lacks: the
    /// ids of its roots and of the earlier roots it keeps ([`super::Rooting::earlier`]), and the
    /// stubs of what they dropped. `None` for a whole log; for one holding a stub none of those
    /// roots dropped, or an earlier root of which it holds no share (no body a peer could read
    /// it by), which no prune leaves: a log keeps an earlier root only for a stub of its drops
    /// that no later root dropped, so no later cut holds it settled and each keeps its shares
    /// (the module's "An earlier root is served"); and for one with more roots than a frame
    /// names ([`MAX_SERVED_ROOTS`]), which no reader would take.
    #[must_use]
    pub fn served(&self) -> Option<Served> {
        let rooting = self.rooting()?;
        let dropped: BTreeSet<&RecordId> = rooting
            .roots
            .iter()
            .chain(&rooting.earlier)
            .flat_map(|root| root.dropped.iter())
            .collect();
        let served = rooting.roots.len() + rooting.earlier.len();
        if rooting.roots.is_empty()
            || served > MAX_SERVED_ROOTS
            || rooting.stubs.keys().any(|id| !dropped.contains(id))
        {
            return None;
        }
        if !rooting.earlier.is_empty() {
            let shared: BTreeSet<RecordId> = self
                .records()
                .filter_map(|record| checkpoint_id(record.body()))
                .collect();
            if rooting
                .earlier
                .iter()
                .any(|root| !shared.contains(&root.id))
            {
                return None;
            }
        }
        let mut roots: Vec<RecordId> = rooting
            .roots
            .iter()
            .chain(&rooting.earlier)
            .map(|root| root.id)
            .collect();
        roots.sort_unstable();
        roots.dedup();
        Some(Served {
            network: *self.network(),
            roots,
            stubs: rooting.stubs.clone(),
        })
    }

    /// Whether a peer whose `have` names these ids holds every root's cut: each head of each root
    /// is named, held in the past of a named record, or a dropped record in such a past.
    pub(super) fn named_cuts(&self, have: &[RecordId]) -> bool {
        let Some(rooting) = self.rooting() else {
            return true;
        };
        let known = self.reach.closure(self.have_indexes(have));
        let named = |head: &RecordId| match self.held.get(head) {
            Some(&at) => (known[at / 64] >> (at % 64)) & 1 == 1,
            None => have.contains(head) || have.iter().any(|named| self.lies_before(head, named)),
        };
        rooting
            .roots
            .iter()
            .all(|root| root.cut().1.iter().all(named))
    }

    /// This log with `records` and the roots of `served` taken (the module's second point): the
    /// records as [`Log::merge`] takes them, then each root it holds no root nesting, its cut
    /// read from this log's own records where it holds them and otherwise through the served
    /// stubs, judged, and the log pruned at it. A root no other served root nests is judged
    /// final ([`checkpoint::judge`]), as [`Log::adopt`] judges one; one another served root
    /// nests, settled, as its peer holds it (the module's "Earlier roots are settled").
    ///
    /// # Errors
    /// The frame is of another network; a root has no share among the records this log then
    /// holds, is not final (or, nested, not settled), its cut is not held, its body is not the
    /// one this log computes there, or a stub it read is one no root it took dropped, a version-2
    /// record's that is not its frame's record ([`Stub::checked`]), or a version-2 `Delete`'s
    /// whose `carries` its statement does not read ([`Log::stub_delete_rep`]); or a record it
    /// holds no longer fits its bounds beside the stubs. Nothing is taken then.
    pub fn take_served(&self, served: &Served, records: Vec<Record>) -> Result<Self> {
        if served.network != *self.network() {
            return Err(Error::new("a served frame is of another network"));
        }
        // Each root's body, a share's: one the reply brought (it may wait on a record a root
        // dropped) or one this log holds.
        let mut bodies: BTreeMap<RecordId, Body> = BTreeMap::new();
        for record in records.iter().chain(self.records()) {
            if let Some(id) = checkpoint_id(record.body())
                && served.roots.contains(&id)
            {
                bodies.entry(id).or_insert_with(|| record.body().clone());
            }
        }
        let mut base = self.clone();
        let knows = |id: &RecordId| self.knows(id);
        for record in super::store::causal(records, knows) {
            let _ = base.insert(record);
        }
        // The roots no root of this log nests, by their cuts' days: the order decides nothing
        // held, since a prune at a root a taken root nests nests it ([`Log::pruned`]).
        let mut roots: Vec<(RecordId, Body)> = Vec::with_capacity(served.roots.len());
        for root in &served.roots {
            let body = bodies
                .remove(root)
                .ok_or_else(|| Error::new("a served root has no share among the records"))?;
            if !base.nests(&body) {
                roots.push((*root, body));
            }
        }
        if roots.is_empty() {
            return Ok(base);
        }
        let day = |body: &Body| match body {
            Body::Checkpoint { cut_ms, .. } => *cut_ms,
            _ => 0,
        };
        roots.sort_by_key(|(id, body)| (day(body), *id));
        let read: BTreeMap<RecordId, Stub> = served
            .stubs
            .iter()
            .filter(|(id, _)| !base.knows(id))
            .map(|(id, stub)| (*id, stub.clone()))
            .collect();
        // A version-2 record's stub is its frame's: every field checked against it.
        #[cfg(test)]
        let check_frames = !mutant(Mutant::UncheckedFrames);
        #[cfg(not(test))]
        let check_frames = true;
        if check_frames {
            for (id, stub) in &read {
                stub.checked(id, &served.network)?;
            }
        }
        // A log that holds every root's cut reads no stub: it recomputes, and trusts none of the
        // peer's.
        let mut taken = if read.is_empty() {
            base
        } else {
            base.with_stubs(&read)?
        };
        // And a version-2 `Delete`'s `carries`, which no frame holds, read again off its
        // statement: the reading of its past alone, each stub in it checked so too.
        #[cfg(test)]
        let check_reps = !mutant(Mutant::DeleteRepsRead);
        #[cfg(not(test))]
        let check_reps = true;
        if check_reps && read.values().any(|stub| stub.frame.is_some()) {
            let reps = taken.frame_reps();
            for (id, stub) in &read {
                if stub.kind == Kind::Delete
                    && stub.frame.is_some()
                    && taken.stub_delete_rep(id, &reps) != stub.carries
                {
                    return Err(Error::new(
                        "a served Delete's stub carries what its statement does not",
                    ));
                }
            }
        }
        // Each root another served root nests, as the stubs read place their cuts: an earlier
        // root its peer keeps ([`super::Rooting::earlier`]), which the peer took final and
        // holds settled, though a later `Remove` may since have un-counted a share of it.
        let nested: BTreeSet<RecordId> = roots
            .iter()
            .filter(|(id, inner)| {
                roots
                    .iter()
                    .any(|(other, outer)| other != id && taken.holds_cut(outer, inner))
            })
            .map(|(id, _)| *id)
            .collect();
        let mut dropped: BTreeSet<RecordId> = BTreeSet::new();
        for (id, body) in roots {
            let Body::Checkpoint { cut_ms, heads, .. } = &body else {
                return Err(Error::new("a served root is a checkpoint"));
            };
            let plan = checkpoint::plan_at(&taken, *cut_ms, heads)
                .map_err(|_| Error::new("the served stubs do not hold a served root's cut"))?;
            // Every stub read in the cut checks itself: the recent seats are its frames'.
            let self_checked = taken
                .stubs_in_past(heads)
                .iter()
                .filter_map(|id| read.get(id))
                .all(|stub| stub.frame.is_some());
            let nested = nested.contains(&id);
            #[cfg(test)]
            let judged =
                mutant(Mutant::Unjudged) || judged(&taken, id, &plan, &body, self_checked, nested)?;
            #[cfg(not(test))]
            let judged = judged(&taken, id, &plan, &body, self_checked, nested)?;
            if !judged {
                return Err(Error::new("a served root is not final by its shares"));
            }
            dropped.extend(plan.dropped.iter().copied());
            taken = taken.pruned(&plan, &body)?;
        }
        #[cfg(test)]
        let covered = mutant(Mutant::Uncovered) || read.keys().all(|id| dropped.contains(id));
        #[cfg(not(test))]
        let covered = read.keys().all(|id| dropped.contains(id));
        if !covered {
            return Err(Error::new("a served stub no served root dropped"));
        }
        Ok(taken)
    }

    /// Whether this log's roots already hold `body`'s cut: a root is `body`, or its cut holds
    /// every head of `body`'s.
    fn nests(&self, body: &Body) -> bool {
        let (Some(id), Body::Checkpoint { heads, .. }) = (checkpoint_id(body), body) else {
            return false;
        };
        self.rooting().is_some_and(|r| {
            r.roots
                .iter()
                .any(|root| root.id == id || heads.iter().all(|head| self.in_cut(root, head)))
        })
    }

    /// Whether the cut of the checkpoint `outer` holds every head of the checkpoint `inner`'s.
    fn holds_cut(&self, outer: &Body, inner: &Body) -> bool {
        let (Body::Checkpoint { heads: outer, .. }, Body::Checkpoint { heads: inner, .. }) =
            (outer, inner)
        else {
            return false;
        };
        inner.iter().all(|head| self.in_cut_of(outer, head))
    }

    /// This log, its roots kept, with the stubs `read` beside its own: what a log that does not
    /// hold a served root's cut reads that cut by, before it judges the root. Its records are
    /// placed afresh, and a record that waited on one of them lands.
    fn with_stubs(&self, read: &BTreeMap<RecordId, Stub>) -> Result<Self> {
        let mut rooting: Rooting = self.rooting().cloned().unwrap_or_default();
        rooting
            .stubs
            .extend(read.iter().map(|(id, stub)| (*id, stub.clone())));
        self.rebuilt(rooting, &BTreeSet::new())
    }
}

#[cfg(test)]
#[path = "served_tests.rs"]
pub(super) mod tests;
