// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault checkpoint as a pure function of the log (`docs/DESIGN-vault-log-2026-11-14.md`
//! step 4, tracker FL-148b): its cut, the records a prune at the cut keeps and drops, the digests
//! its body names, when it is final, and when a seat may share it. Nothing here signs, prunes or
//! writes: FL-148c applies a final checkpoint and FL-148f signs one.
//!
//! Every function reads record frames alone: headers, parents, the clear fields of a version-2
//! `Put` or `Delete` (audience, epoch, its [`Statement`]), and the weighed records. No audience
//! key is asked for, so two Macs that hold different keys compute the same body.
//!
//! - **The cut** ([`propose`]) takes the shape of the Commons table checkpoint's
//!   (`crates/aterm-db/src/checkpoint.rs`, `propose`), read from the records authored before `T`
//!   alone. `T` ([`cut_day`]) is a whole UTC day at least [`MIN_AGE_MS`] old even to a proposer
//!   whose clock runs [`budgets::WINDOW_SKEW_MS`] fast; that allowance is the proposer's own, for
//!   its clock, since the cut and the recent seats compare signed clocks with `T` itself. The
//!   records before the day are those authored before `T` with their causal pasts: a record is
//!   placed no later than any record that follows it, so one signed with a clock running a year
//!   fast holds back no cut once a record authored before `T` follows it. The recent seats are
//!   the seats of that past that signed one of its records within [`finality::HEARD_WITHIN_MS`]
//!   before `T`, and the cut is what each of their newest records there has seen: the
//!   intersection of their pasts, which is causally closed. A seat that forked (signed two
//!   records at one sequence) has two newest records, and the cut is what both have seen. So two
//!   proposers that hold the same records authored before `T` cut alike, in whatever order they
//!   took them and whatever each holds after `T`. The cut is a set of records, not of sequences,
//!   so a fork of another signer's is in the cut only where every recent seat saw that branch.
//!   The recent seats [`propose`] bounds by are those of the records before the day, a superset
//!   of the cut, and [`judge`] counts those of the cut: a seat the proposer bounds by but the
//!   judge does not count only shrinks the cut.
//! - **The keep set** ([`Plan::keep`]), a function of the cut's records alone:
//!   - every weighed record (`Genesis`, `Admit`, `Remove`), and each `Remove`'s cutoff;
//!   - every `Name` and `Node`: the label evaluator reads a seat's earlier numeric `Name`s
//!     (where its tenure of a number began) and the node-key index every `Node` (the keys a seat
//!     replaced), so keeping only each seat's newest of either would change who is who;
//!   - of the `Put`s and `Delete`s, every one but those [`superseded`] by a newer op of their own
//!     signer in the same register, as that op's own statement names them, which keeps each
//!     register's winner and siblings (no op of their signer names them) and each signer's newest
//!     op; with them every op whose value a kept op carries (the
//!     semantic representatives), the causally newest counting `Put`s of the Commons audience
//!     (its control winner among them), and every op under an epoch some seat is not yet wrapped
//!     to ([`owed_epochs`]). A version-1 op names nothing, so it goes only where a version-2 op
//!     names it;
//!   - every `Epoch` and every `Wrap` (FL-148c): a record outside the cut (one the recent seats
//!     had not all seen before the day, a write of the other side of a split, a Mac's that slept
//!     across the cut) may be sealed under any epoch, and a log that dropped that epoch would hold
//!     the record and not open it, which the whole log does; aterm-db authenticates a table record
//!     by the Commons epoch it was sealed under (`Log::historical_commons_writers`), and a `Wrap`
//!     to a Mac since removed is what marks the entries it opened exposed. They are few beside
//!     the `Put`s a prune drops: one `Epoch` and a `Wrap` per seat at each rotation;
//!   - the checkpoint chain: every share, within the cut, of the newest final checkpoint there,
//!     one share of each checkpoint its `predecessor`s name back from it (one record per final
//!     checkpoint the vault ever pruned at), and every share of each checkpoint not nested in it
//!     (it may yet be final, or be final already on a side of a split); with no newest final
//!     checkpoint, every share;
//!   - and none of the records a settled checkpoint in the cut dropped ([`gone`]): they count
//!     nowhere, so the prune drops them again, held or stubs, and keeps nothing for their sake,
//!     as a log pruned at that checkpoint holds only their stubs, which read gone in every cut
//!     that holds it settled and in no other.
//! - **`dropped`** is [`dropped_digest`] of the stubs ([`Stub::encode`], every field; a
//!   version-2 record's as its shed frame and a `Delete`'s `carries`) of the records the cut
//!   holds and the keep set does not, and, in a log an earlier checkpoint
//!   pruned (FL-148c), of those it dropped in the cut but those the whole log's prune keeps
//!   ([`keep_set`] reads a stub of an op as its record, by the epoch and names its stub keeps:
//!   the rep of an op a kept op carries, a kept `Remove`'s cutoff, an op under an owed epoch, an
//!   op this cut keeps that a later prune dropped), as the whole log's prune drops the records
//!   they stand for. A stub is a function of its record and the record's past, so every Mac that
//!   holds the cut digests the same stubs, and one that does not takes a peer's only where they
//!   digest to the body its shares signed ([`super::served`]). **`state`** is [`state_digest`]: a
//!   canonical digest of what the frames say at the cut
//!   (seats, removals with their cutoffs, the kept epochs and their wraps, the register frontier).
//!   It departs from the design's "the head of step 2": FL-146's head is a per-Mac cache, sealed
//!   under one Mac's identity and naming one file's stamp, so no two Macs could ever sign the
//!   same body over it.
//! - **Finality** ([`judge`]) is the shared rule of [`crate::commons::finality`]: more than half
//!   of the seats at the cut that signed a record within [`finality::HEARD_WITHIN_MS`] before it
//!   signed the same body, each share a record that counts (a share a removed Mac signed past its
//!   cutoff counts for nothing). A Mac that holds the cut's past recomputes the body and holds a
//!   body it computes otherwise final for no count of shares. Within a cut, a checkpoint it holds
//!   is final by the same rule ([`Plan::predecessor`]): its cut passes [`plan_at`]'s checks
//!   within the cut, its shares are a majority, and the body recomputes there. A body's
//!   recomputation is a function of its own cut, so a log, its clones and its cuts compute each
//!   once however deep the chain ([`Cuts`]). A log rooted at a checkpoint holds that root, and the predecessor its body names,
//!   recomputed: it adopted them where it held their cuts whole, and a later prune may drop
//!   records an earlier checkpoint kept, which no stub tells from one it dropped.
//! - **Settled** ([`settled`]): a checkpoint a log recomputes and more than half of whose cut's
//!   recent seats signed a share the log holds or dropped, counting or not. Finality implies it;
//!   unlike finality, which a `Remove` that un-counts a share undoes, it holds however the log
//!   grows, so what its prune dropped is gone in every log that holds it ([`gone`]). That costs
//!   what [`settled`] states: a seat alone at its cut, or a key past its seat's cutoff, settles,
//!   and so can make final the loss of its own seat's values its writes superseded, and no more.
//! - **The share rule** ([`may_share`]): a seat shares only a body it computes itself whose cut
//!   contains every cut it shared before, read from its own records. Two final checkpoints whose
//!   majorities share a seat therefore nest. Two whose recent seats do not overlap need not: a
//!   split of at least [`MIN_AGE_MS`] after a quiet [`finality::HEARD_WITHIN_MS`] leaves each side
//!   the only recent seats of its own cut, and each side's cut final. No rule a side can apply
//!   alone tells a split from dead seats (the cost [`crate::commons::finality`] states), so the
//!   newest final checkpoint is the final one whose cut contains every other final one's, the
//!   latest by its day where several do and then the lowest id; where none does there is no
//!   newest, the next body names no predecessor, and the prune keeps every share. The next cut
//!   that holds both sides' heads nests them both, and its shares make it the newest.
//!
//! A `Statement`'s `supersedes` names the ops of its register its writer's resolve read as
//! replaced at it. Replacement is a fact of the two ops, so an op once named stays superseded
//! however the log grows, and the namer need not be kept for the op to go. Nothing checks the
//! statement against the ciphertexts, so a name counts only where the frames show it could be
//! true: the named op has the namer's audience and lies in the namer's causal past, and the
//! namer holds its own epoch (it signed that `Epoch`, or a counting `Wrap` of it to the namer's
//! recipient lies in its past). And a name supersedes only an op of the namer's own signer: no
//! frame tells one register of an audience from another, so a statement naming another signer's
//! ops could drop that signer's winner of any register (a stolen key's false statement, say),
//! where a name of its own signer's op drops only what that signer's own `Delete` could replace.
//! A false statement then reaches only entries its author could overwrite or delete itself. The
//! prune of a log a final checkpoint already pruned matches the
//! prune of the whole one only where every op a kept op superseded is named by a kept op:
//! FL-148f's writer names them, and FL-148c's union and evaluation tests judge it.
//!
//! A `Statement`'s `carries` must name the value's semantic representative, the earliest op of
//! its carry chain (`register.reps` in `entries.rs`), and not merely the op the writer read the
//! value from: the keep set follows `carries` one step per kept op, and a version-1 reseal states
//! nothing, so a chain through one ends there. FL-148f's writer names the representative itself.
//! Every log, whole or rooted, reads a version-2 op by its statement alone ([`super::rooted`]): a
//! `Put`'s `carries`, and a `Delete`'s `supersedes`, which must name every op it read as live (a
//! `Delete` over `Delete`s alone is a carry of their winner, and carries nothing a statement can
//! name). Each name counts only where this module's rule lets it.

use super::rooted::Stub;
use super::{
    Body, EpochAudience, Kind, Log, MemberId, Record, RecordId, Statement, budgets, encode_body,
    entries,
};
use crate::commons::finality;
use crate::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// One UTC day, in milliseconds: a cut is a whole one.
pub const DAY_MS: u64 = 24 * 60 * 60 * 1000;
/// How old a cut is at least when it is proposed: 14 days.
pub const MIN_AGE_MS: u64 = 14 * DAY_MS;

/// What [`checkpoint_id`] hashes ahead of a checkpoint's clear fields.
const CHECKPOINT_DOMAIN: &[u8] = b"aterm/members/checkpoint/1\0";
/// What [`state_digest`] hashes first.
const STATE_DOMAIN: &[u8] = b"aterm/members/checkpoint-state/1\0";
/// What [`dropped_digest`] hashes first.
const DROPPED_DOMAIN: &[u8] = b"aterm/members/checkpoint-dropped/3\0";

/// A broken checkpoint, for the tests' negative controls.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mutant {
    /// The keep set and the state are read from the whole log, not the cut's past.
    WholeLog,
    /// The cut takes every record authored before its day, seen by the recent seats or not.
    NoSeenBound,
    /// Finality counts half of the recent seats as enough.
    HalfIsFinal,
    /// Finality counts every seat at the cut as recent, silent ones included.
    EverySeatRecent,
    /// A seat may share any cut later by its day than every cut it shared, nested or not.
    ByDayAlone,
    /// FL-148b's rule: a name supersedes another signer's op too, where a newer op of that
    /// signer is among the names.
    CrossSigner,
    /// The proposer bounds the cut by each recent seat's newest record of any date, as it holds
    /// it, and not by its newest before the day.
    NewestOfAnyDate,
    /// The proposer takes one newest record of each recent seat, the last of its highest
    /// sequence in the order this log took them, and not each of a fork's.
    OneNewest,
    /// The records before the day are the largest causally closed part of those authored before
    /// it, and a cut holds only records authored before it.
    ClosedBeforeDay,
    /// Every name of a counting statement counts: another audience's op, an op outside the
    /// namer's past, and a namer that holds no key of its own epoch.
    AnyName,
    /// A checkpoint within a cut is final by its shares alone, unchecked and not recomputed.
    SharesAlone,
    /// A share counts whether or not its record does.
    AnyShare,
    /// The newest final checkpoint is the latest by its day, then the lowest id, nested in it or
    /// not, and the prune drops every other final one's shares.
    NewestByDay,
    /// No log keeps what a checkpoint's cut says ([`Cuts`]): each evaluation computes it afresh.
    NoCutMemo,
}

#[cfg(test)]
thread_local! {
    /// The mutant this thread's checkpoints are computed as, if any.
    pub(super) static MUTANT: std::cell::Cell<Option<Mutant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn mutant(which: Mutant) -> bool {
    MUTANT.get() == Some(which)
}

/// The cut a proposer whose clock reads `now_ms` takes: the start of the latest UTC day that is
/// at least [`MIN_AGE_MS`] and [`budgets::WINDOW_SKEW_MS`] before `now_ms`. `None` before there is
/// one.
#[must_use]
pub fn cut_day(now_ms: u64) -> Option<u64> {
    let latest = now_ms.checked_sub(MIN_AGE_MS + budgets::WINDOW_SKEW_MS)?;
    Some(latest / DAY_MS * DAY_MS).filter(|&cut| cut != 0)
}

/// A checkpoint computed at a cut, with the prune it stands for.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The checkpoint's body: [`Body::Checkpoint`].
    pub body: Body,
    /// The ids of the cut's records a prune at it keeps.
    pub keep: BTreeSet<RecordId>,
    /// The ids of the cut's records a prune at it drops, sorted.
    pub dropped: Vec<RecordId>,
    /// The stub a prune makes of each of `dropped`, by id: what `dropped` digests
    /// ([`dropped_digest`]).
    pub stubs: BTreeMap<RecordId, Stub>,
}

impl Plan {
    /// The checkpoint the body follows ([`checkpoint_id`] of the newest final checkpoint in the
    /// cut), where there is one.
    #[must_use]
    pub fn predecessor(&self) -> Option<RecordId> {
        match &self.body {
            Body::Checkpoint { predecessor, .. } => *predecessor,
            _ => None,
        }
    }
}

/// How one log judges a checkpoint ([`judge`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judgment {
    /// The log computes this body itself at its heads.
    pub recomputed: bool,
    /// The seats at the cut that signed a record within [`finality::HEARD_WITHIN_MS`] before it.
    pub recent: BTreeSet<MemberId>,
    /// The seats that signed a share of this body, recent or not.
    pub signed: BTreeSet<MemberId>,
    /// The log recomputes it, and more than half of the recent seats signed it.
    pub is_final: bool,
    /// The seats whose key signed a share of this body that the log holds or dropped, whether or
    /// not the share counts ([`settled`]'s signers).
    pub shared: BTreeSet<MemberId>,
    /// The log recomputes it, and more than half of the recent seats are among `shared`: it
    /// holds the checkpoint settled ([`settled`]), which, unlike `is_final`, no later record
    /// undoes.
    pub is_settled: bool,
}

/// A checkpoint's id: SHA-256 of [`CHECKPOINT_DOMAIN`] and its clear fields, which every share of
/// it carries alike, whoever signed the share. `None` for any other body.
#[must_use]
pub fn checkpoint_id(body: &Body) -> Option<RecordId> {
    matches!(body, Body::Checkpoint { .. }).then(|| {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(CHECKPOINT_DOMAIN);
        hash.update(&encode_body(body));
        hash.finalize()
    })
}

/// The checkpoint this log proposes at `now_ms`, with its prune: at [`cut_day`]'s `T`, of the
/// records before the day (those authored before `T`, with their pasts), what each newest record
/// there of each recent seat has seen ([`plan_at`] then computes the rest). A pure function of
/// the records authored before `T` this log holds. `None` where there is no such day yet, no seat
/// signed a record within [`finality::HEARD_WITHIN_MS`] before it, or the cut has no checkpoint
/// ([`plan_at`]'s errors). A log rooted at an earlier checkpoint ([`super::rooted`]) reads each
/// record its prunes dropped off its stub (its signer, sequence, previous record, clock and
/// parents), so it proposes what the whole log does.
#[must_use]
pub fn propose(log: &Log, now_ms: u64) -> Option<Plan> {
    let cut_ms = cut_day(now_ms)?;
    let below = before_day(log, cut_ms).ok()?;
    let writers = recent_seats(&below, cut_ms);
    if writers.is_empty() {
        return None;
    }
    let mut cut: BTreeSet<RecordId> = below.records().map(|r| *r.id()).collect();
    // A rooted log's dropped records before the day are records of it too.
    cut.extend(stubs_of(&below).keys().copied());
    for writer in &writers {
        #[cfg(test)]
        let newest =
            mutant_newest(log, &below, writer).unwrap_or_else(|| newest_of(&below, writer));
        #[cfg(not(test))]
        let newest = newest_of(&below, writer);
        for newest in newest {
            #[cfg(test)]
            if mutant(Mutant::NoSeenBound) {
                continue;
            }
            let past = log.past_of(&[newest]);
            let dropped = log.stubs_in_past(&[newest]);
            cut.retain(|id| past.holds(id) || dropped.contains(id));
        }
    }
    let mut heads = cut.clone();
    for record in below.records().filter(|r| cut.contains(r.id())) {
        for parent in record.parents() {
            heads.remove(parent);
        }
    }
    for (id, stub) in stubs_of(&below) {
        if cut.contains(id) {
            for parent in &stub.parents {
                heads.remove(parent);
            }
        }
    }
    let heads: Vec<RecordId> = heads.into_iter().collect();
    plan_at(log, cut_ms, &heads).ok()
}

/// The records of `log` before the day `cut_ms`: each authored before it, with its causal past,
/// as a log of its own.
fn before_day(log: &Log, cut_ms: u64) -> Result<Log> {
    #[cfg(test)]
    if mutant(Mutant::ClosedBeforeDay) {
        let mut closed: BTreeSet<RecordId> = BTreeSet::new();
        let mut past = Vec::new();
        for record in log.records() {
            if record.authored_ms() < cut_ms && record.parents().iter().all(|p| closed.contains(p))
            {
                closed.insert(*record.id());
                past.push(record.clone());
            }
        }
        let ids: Vec<RecordId> = past.iter().map(|r| *r.id()).collect();
        return projection(log, past, &ids);
    }
    let mut authored: Vec<RecordId> = log
        .records()
        .filter(|r| r.authored_ms() < cut_ms)
        .map(|r| *r.id())
        .collect();
    authored.extend(
        stubs_of(log)
            .iter()
            .filter(|(_, stub)| stub.authored_ms < cut_ms)
            .map(|(id, _)| *id),
    );
    projection(log, log.ancestor_closure(&authored), &authored)
}

/// The stubs of a rooted log ([`super::rooted`]), by id: none for a whole log.
fn stubs_of(log: &Log) -> &BTreeMap<RecordId, Stub> {
    static NONE: BTreeMap<RecordId, Stub> = BTreeMap::new();
    #[cfg(test)]
    if super::rooted::mutant(super::rooted::Mutant::HeldProposal) {
        return &NONE;
    }
    log.rooting().map_or(&NONE, |rooting| &rooting.stubs)
}

/// The newest records of `seat` in `below`: those no other record of its there follows. One,
/// unless the seat forked. A rooted log's dropped records count by their stubs.
fn newest_of(below: &Log, seat: &MemberId) -> Vec<RecordId> {
    let mut records: Vec<(RecordId, Option<RecordId>)> = below
        .records()
        .filter(|r| signed_by(r, seat))
        .map(|r| (*r.id(), r.previous().copied()))
        .collect();
    let dropped = stubs_of(below);
    records.extend(
        dropped
            .iter()
            .filter(|(_, stub)| stub.actor == *seat)
            .map(|(id, stub)| (*id, stub.previous)),
    );
    let followed: BTreeSet<RecordId> = records.iter().filter_map(|r| r.1).collect();
    let tips: Vec<RecordId> = records
        .iter()
        .map(|r| r.0)
        .filter(|id| !followed.contains(id))
        .collect();
    tips.iter()
        .filter(|tip| {
            !tips.iter().any(|other| {
                other != *tip
                    && if dropped.is_empty() {
                        below.reaches(std::slice::from_ref(other), tip)
                    } else {
                        below.lies_before(tip, other)
                    }
            })
        })
        .copied()
        .collect()
}

/// Whether `record` is one of `seat`'s own, not the genesis it may have founded.
fn signed_by(record: &Record, seat: &MemberId) -> bool {
    record.kind() != Kind::Genesis && record.actor() == seat
}

/// The newest records of `seat` the tests' `NewestOfAnyDate` and `OneNewest` take in place of
/// [`newest_of`]'s: one record, the last of the seat's highest sequence in `log`'s order, of any
/// date or of `below`'s.
#[cfg(test)]
fn mutant_newest(log: &Log, below: &Log, seat: &MemberId) -> Option<Vec<RecordId>> {
    let of_any_date = mutant(Mutant::NewestOfAnyDate);
    if !of_any_date && !mutant(Mutant::OneNewest) {
        return None;
    }
    let newest = log
        .records()
        .filter(|r| signed_by(r, seat) && (of_any_date || below.get(r.id()).is_some()))
        .max_by_key(|r| r.sequence());
    Some(newest.map(|r| *r.id()).into_iter().collect())
}

/// The checkpoint at the cut whose heads are `heads` and whose time is `cut_ms`, with its prune:
/// a pure function of the records in the heads' causal past, which this log must hold.
///
/// # Errors
/// `cut_ms` is not a whole UTC day; `heads` is empty, names more than
/// [`super::MAX_CHECKPOINT_HEADS`] records or one this log does not hold, or names one in
/// another's past; or a head was authored at or after `cut_ms`. A record in the heads' past may
/// carry a later clock: it is placed no later than the head that follows it.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "Prune",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
pub fn plan_at(log: &Log, cut_ms: u64, heads: &[RecordId]) -> Result<Plan> {
    let at = cut_log(log, cut_ms, heads)?;
    Ok(plan_of(&at.log, cut_ms, at.heads))
}

/// The heads' causal past as a log of its own, and the heads sorted.
struct AtCut {
    log: Log,
    heads: Vec<RecordId>,
}

/// [`plan_at`]'s checks, and the past they pass.
fn cut_log(log: &Log, cut_ms: u64, heads: &[RecordId]) -> Result<AtCut> {
    let mut heads = heads.to_vec();
    heads.sort_unstable();
    heads.dedup();
    if cut_ms == 0 || cut_ms % DAY_MS != 0 {
        return Err(Error::new("a checkpoint's cut is a whole UTC day"));
    }
    if heads.is_empty() || heads.len() > super::MAX_CHECKPOINT_HEADS {
        return Err(Error::new(
            "a checkpoint names from one to 4096 heads of its cut",
        ));
    }
    // A rooted log's cut may end at records its prunes dropped ([`super::rooted`]), read off
    // their stubs.
    if heads.iter().any(|h| !log.knows(h)) {
        return Err(Error::new("this log does not hold the checkpoint's cut"));
    }
    for head in &heads {
        let others: Vec<RecordId> = heads.iter().filter(|h| *h != head).copied().collect();
        let below = if log.rooting().is_some() {
            others.iter().any(|other| log.lies_before(head, other))
        } else {
            log.reaches(&others, head)
        };
        if below {
            return Err(Error::new(
                "a checkpoint's heads are the newest records of its cut",
            ));
        }
    }
    let authored = |h: &RecordId| {
        log.get(h)
            .map(Record::authored_ms)
            .or_else(|| stubs_of(log).get(h).map(|stub| stub.authored_ms))
    };
    if heads.iter().filter_map(authored).any(|at| at >= cut_ms) {
        return Err(Error::new(
            "every head of a checkpoint's cut was authored before it",
        ));
    }
    let past = log.ancestor_closure(&heads);
    #[cfg(test)]
    if mutant(Mutant::ClosedBeforeDay) && past.iter().any(|r| r.authored_ms() >= cut_ms) {
        return Err(Error::new(
            "every record of a checkpoint's cut was authored before it",
        ));
    }
    #[cfg(test)]
    if mutant(Mutant::WholeLog) {
        return Ok(AtCut {
            log: log.clone(),
            heads,
        });
    }
    Ok(AtCut {
        log: projection(log, past, &heads)?,
        heads,
    })
}

/// A log of `past`, a causally closed set of `log`'s records in its order, with `log`'s budgets:
/// the causal past of `ends`, held or dropped, whose stubs a rooted log's projection keeps.
fn projection(log: &Log, past: Vec<Record>, ends: &[RecordId]) -> Result<Log> {
    let mut records = past.into_iter();
    let genesis = records
        .next()
        .ok_or_else(|| Error::new("a checkpoint's cut has no genesis"))?;
    let mut at = Log::new(genesis)?;
    at.budgets = log.budgets;
    at.enforce_seats = log.enforce_seats;
    // A rooted log's records follow its dropped records' places (FL-148c): the stubs in the
    // past of `ends` are this past's.
    at.rooting = log.rooting_within(&log.stubs_in_past(ends));
    // A cut of this past is a cut of `log`'s, which says the same of it.
    at.cuts = Arc::clone(&log.cuts);
    at.restub();
    for record in records {
        at.insert(record)?;
    }
    at.stub_heads();
    Ok(at)
}

/// The plan of the cut `at` holds whole, at `cut_ms` and `heads`.
fn plan_of(at: &Log, cut_ms: u64, heads: Vec<RecordId>) -> Plan {
    #[cfg(test)]
    PLANS_COMPUTED.set(PLANS_COMPUTED.get() + 1);
    let checkpoints = Checkpoints::of(at);
    let (keep, kept_stubs) = keep_set(at, &checkpoints);
    // A log rooted at an earlier checkpoint (FL-148c) drops the records its stubs in the cut
    // stand for again, as the whole log's prune drops them, but for those the whole log's keep
    // set keeps (a rep a kept op carries, a kept `Remove`'s cutoff, an op under an owed epoch):
    // so both compute one body.
    let mut dropped: BTreeSet<RecordId> = at.stubs_in_past(&heads);
    #[cfg(test)]
    let kept_stubs = if super::rooted::mutant(super::rooted::Mutant::DropEveryStub) {
        BTreeSet::new()
    } else {
        kept_stubs
    };
    dropped.retain(|id| !kept_stubs.contains(id));
    dropped.extend(
        at.records()
            .map(Record::id)
            .filter(|id| !keep.contains(*id))
            .copied(),
    );
    let dropped: Vec<RecordId> = dropped.into_iter().collect();
    let stubs = stubs_dropped(at, &dropped);
    let body = Body::Checkpoint {
        predecessor: checkpoints.newest,
        cut_ms,
        heads,
        state: state_digest(at, &keep),
        dropped: dropped_digest(&stubs),
    };
    Plan {
        body,
        keep,
        dropped,
        stubs,
    }
}

/// The stub of each of `dropped` that a prune of the cut `at` holds whole makes
/// ([`Log::stub_for`]): what [`dropped_digest`] hashes. A held `Delete` with a statement keeps
/// the rep its statement reads ([`Log::frame_reps`], a function of its own past, which the cut
/// holds).
fn stubs_dropped(at: &Log, dropped: &[RecordId]) -> BTreeMap<RecordId, Stub> {
    let stated_delete = |id: &RecordId| {
        at.get(id)
            .is_some_and(|r| r.kind() == Kind::Delete && r.statement().is_some())
    };
    let reps = if dropped.iter().any(stated_delete) {
        at.frame_reps()
    } else {
        BTreeMap::new()
    };
    dropped
        .iter()
        .filter_map(|id| Some((*id, at.stub_for(id, &reps)?)))
        .collect()
}

/// SHA-256 of [`DROPPED_DOMAIN`], the number of `stubs` as a big-endian `u64`, then each stub in
/// id order in its one encoding ([`Stub::encode`]): the `dropped` digest of a checkpoint whose
/// prune drops the records these stubs stand for. Every field a stub keeps is bound, so a Mac
/// that does not hold the cut and takes the stubs from a peer ([`super::served`]) takes those a
/// checkpoint's shares signed, field for field, or none.
#[must_use]
pub fn dropped_digest(stubs: &BTreeMap<RecordId, Stub>) -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(DROPPED_DOMAIN);
    hash.update(&(stubs.len() as u64).to_be_bytes());
    #[cfg(test)]
    if super::served::mutant(super::served::Mutant::IdsOnly) {
        for id in stubs.keys() {
            hash.update(id);
        }
        return hash.finalize();
    }
    let mut encoded = Vec::new();
    for (id, stub) in stubs {
        encoded.clear();
        stub.encode(id, &mut encoded);
        hash.update(&encoded);
    }
    hash.finalize()
}

/// The `state` digest of the cut `at` holds whole, whose prune keeps `keep`: SHA-256 of, in
/// order, every number big-endian and every list counted first:
///
/// | bytes | field |
/// |---|---|
/// | 33 | `aterm/members/checkpoint-state/1` and a NUL |
/// | 32 | the network |
/// | 4, then each | the seats ([`super::Membership::members`]), by actor: actor (48), the record that let it in (32), label (`u16` length and its bytes), node key (`0`, or `1` and 32 bytes) |
/// | 4, then each | the Macs out ([`super::Membership::removed`]), by actor: actor (48), `1` where a quorum took it out else `0`, then a `u16` count and its cutoffs ascending (32 each): each counting `Remove`'s of it and each quorum's |
/// | 4, then each | the kept epochs, by id: id (32), audience (as a record writes it), generation (`u64`), commitment (32), `1` where it counts else `0`, then a `u32` count and the recipients its kept wraps name, ascending (32 each) |
/// | 4, then each | the register frontier, by id (32): each `Put` and `Delete` no counting version-2 op names |
///
/// Every field is read from a frame or a weighed record; nothing is opened.
#[must_use]
pub fn state_digest(at: &Log, keep: &BTreeSet<RecordId>) -> [u8; 32] {
    let membership = at.membership_ref();
    let effective = at.effective();
    let mut out = Vec::new();
    out.extend_from_slice(STATE_DOMAIN);
    out.extend_from_slice(at.network());
    count(&mut out, membership.members().len());
    for (actor, member) in membership.members() {
        out.extend_from_slice(actor);
        out.extend_from_slice(&member.entry);
        let label = member.label.as_bytes();
        out.extend_from_slice(&u16::try_from(label.len()).unwrap_or(u16::MAX).to_be_bytes());
        out.extend_from_slice(label);
        match member.node_key {
            Some(key) => {
                out.push(1);
                out.extend_from_slice(&key);
            }
            None => out.push(0),
        }
    }
    let mut cutoffs: BTreeMap<MemberId, BTreeSet<RecordId>> = BTreeMap::new();
    for (i, record) in at.records().enumerate() {
        if let Body::Remove { member, cutoff } = record.body()
            && effective.counts(i)
        {
            cutoffs.entry(*member).or_default().insert(*cutoff);
        }
    }
    for quorum in membership.quorums() {
        cutoffs
            .entry(quorum.member)
            .or_default()
            .insert(quorum.cutoff);
    }
    count(&mut out, membership.removed().len());
    for (actor, removal) in membership.removed() {
        out.extend_from_slice(actor);
        out.push(u8::from(removal.quorum));
        let of = cutoffs.get(actor).cloned().unwrap_or_default();
        out.extend_from_slice(&u16::try_from(of.len()).unwrap_or(u16::MAX).to_be_bytes());
        for cutoff in of {
            out.extend_from_slice(&cutoff);
        }
    }
    let mut epochs: BTreeMap<RecordId, (usize, BTreeSet<[u8; 32]>)> = BTreeMap::new();
    for (i, record) in at.records().enumerate() {
        if record.kind() == Kind::Epoch && keep.contains(record.id()) {
            epochs.entry(*record.id()).or_default().0 = i;
        }
    }
    for record in at.records() {
        if let Body::Wrap {
            epoch, recipient, ..
        } = record.body()
            && keep.contains(record.id())
            && let Some((_, wrapped)) = epochs.get_mut(epoch)
        {
            wrapped.insert(recipient.0);
        }
    }
    count(&mut out, epochs.len());
    let order: Vec<&Record> = at.records().collect();
    for (id, (i, wrapped)) in &epochs {
        let Body::Epoch {
            audience,
            generation,
            commitment,
        } = order[*i].body()
        else {
            continue;
        };
        out.extend_from_slice(id);
        entries::encode_audience_wire(&mut out, audience);
        out.extend_from_slice(&generation.to_be_bytes());
        out.extend_from_slice(commitment);
        out.push(u8::from(effective.counts(*i)));
        count(&mut out, wrapped.len());
        for recipient in wrapped {
            out.extend_from_slice(recipient);
        }
    }
    let ops = Ops::of(at);
    let frontier: BTreeSet<RecordId> = ops
        .all
        .iter()
        .filter(|&&i| !ops.named.contains(&i))
        .filter_map(|&i| at.id_at(i))
        .collect();
    count(&mut out, frontier.len());
    for id in frontier {
        out.extend_from_slice(&id);
    }
    let mut hash = aterm_digest::Sha256::new();
    hash.update(&out);
    hash.finalize()
}

/// `n` as a big-endian `u32`.
fn count(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_be_bytes());
}

/// The newest signed clock of `seat`'s records in `log` before `cut_ms`: a record authored at the
/// cut is no record of the cut's, so it is not one of the window's either.
fn last_signed(log: &Log, seat: &MemberId, cut_ms: u64) -> Option<u64> {
    let dropped = stubs_of(log)
        .values()
        .filter(|stub| stub.actor == *seat)
        .map(|stub| stub.authored_ms);
    log.records()
        .filter(|r| signed_by(r, seat))
        .map(Record::authored_ms)
        .chain(dropped)
        .filter(|&at| at < cut_ms)
        .max()
}

/// A `Put` or `Delete` of a log, held or a rooted log's stub ([`super::rooted`]), as its frame
/// says.
struct Op<'a> {
    /// Its position ([`Log::position`]).
    at: usize,
    actor: MemberId,
    /// Read only by `Mutant::CrossSigner`, the rule the own-signer one replaced.
    #[cfg(test)]
    sequence: u64,
    kind: Kind,
    audience: &'a EpochAudience,
    epoch: RecordId,
    /// What its statement names as replaced: none for a version-1 op.
    names: &'a [RecordId],
}

/// Every `Put` and `Delete` of `at`, held or a stub's, but those a settled checkpoint dropped
/// ([`gone`]): a held one by its record's frame, a stub by what it kept of it ([`Stub::epoch`],
/// [`Stub::supersedes`]), so a rooted log reads the ops of a cut whose records a later prune
/// dropped as the whole log reads them there.
fn ops_of(at: &Log) -> Vec<Op<'_>> {
    let effective = at.effective();
    let mut out: Vec<Op<'_>> = at
        .records()
        .enumerate()
        .filter(|&(i, _)| !effective.is_gone(i))
        .filter_map(|(i, record)| {
            let (audience, epoch) = op_frame(record)?;
            Some(Op {
                at: i,
                actor: *record.actor(),
                #[cfg(test)]
                sequence: record.sequence(),
                kind: record.kind(),
                audience,
                epoch: *epoch,
                names: record
                    .statement()
                    .map_or(&[][..], |statement| statement.supersedes.as_slice()),
            })
        })
        .collect();
    for (id, stub) in stubs_of(at) {
        if !matches!(stub.kind, Kind::Put | Kind::Delete) {
            continue;
        }
        let (Some(at_stub), Some(epoch)) = (at.position(id), stub.epoch) else {
            continue;
        };
        let Some(Body::Epoch { audience, .. }) = at.get(&epoch).map(Record::body) else {
            continue;
        };
        if effective.is_gone(at_stub) {
            continue;
        }
        out.push(Op {
            at: at_stub,
            actor: stub.actor,
            #[cfg(test)]
            sequence: stub.sequence,
            kind: stub.kind,
            audience,
            epoch,
            names: &stub.supersedes,
        });
    }
    out
}

/// The `Put`s and `Delete`s of a log, by position ([`Log::position`]: a rooted log's stubs too),
/// and what their statements say of them.
struct Ops {
    /// Every `Put` and `Delete` no settled checkpoint dropped.
    all: Vec<usize>,
    /// Each one's kind and epoch, and whether it is a `Put` of the Commons.
    rows: BTreeMap<usize, (Kind, RecordId, bool)>,
    /// Those a counting version-2 op names in its statement, where the name counts.
    named: BTreeSet<usize>,
    /// Those a counting version-2 op of their own signer names, where the names count: each
    /// has a newer op of its signer in its register ([`superseded_in`]).
    superseded: BTreeSet<usize>,
}

/// A `Put`'s or `Delete`'s audience and epoch; `None` for any other body.
fn op_frame(record: &Record) -> Option<(&EpochAudience, &RecordId)> {
    match record.body() {
        Body::Put {
            audience, epoch, ..
        }
        | Body::Delete {
            audience, epoch, ..
        } => Some((audience, epoch)),
        _ => None,
    }
}

impl Ops {
    fn of(at: &Log) -> Self {
        let effective = at.effective();
        // A held op a settled checkpoint dropped is read as a log pruned there reads its stub:
        // as no op ([`gone`]).
        let ops = ops_of(at);
        let index: BTreeMap<RecordId, usize> = ops
            .iter()
            .enumerate()
            .filter_map(|(k, op)| Some((at.id_at(op.at)?, k)))
            .collect();
        // Each epoch's counting wraps: the wrap's index and its recipient.
        let mut wraps: BTreeMap<RecordId, Vec<(usize, [u8; 32])>> = BTreeMap::new();
        for (i, record) in at.records().enumerate() {
            if let Body::Wrap {
                epoch, recipient, ..
            } = record.body()
                && effective.counts(i)
            {
                wraps.entry(*epoch).or_default().push((i, recipient.0));
            }
        }
        // Whether `op` holds its own epoch by the frames: it signed the epoch, or a counting
        // wrap of it to its signer's recipient lies in its past.
        let holds_epoch = |op: &Op<'_>| {
            if at.get(&op.epoch).is_some_and(|e| *e.actor() == op.actor) {
                return true;
            }
            let Some(seat) = at.membership_ref().members().get(&op.actor) else {
                return false;
            };
            let recipient = seat.binding.recipient().0;
            wraps.get(&op.epoch).is_some_and(|of| {
                of.iter()
                    .any(|&(w, to)| to == recipient && at.position_before(w, op.at))
            })
        };
        let mut named = BTreeSet::new();
        let mut superseded = BTreeSet::new();
        for op in &ops {
            if op.names.is_empty() || !effective.counts(op.at) {
                continue;
            }
            #[cfg(test)]
            let any_name = mutant(Mutant::AnyName);
            #[cfg(not(test))]
            let any_name = false;
            if !any_name && !holds_epoch(op) {
                continue;
            }
            let names: Vec<&Op<'_>> = op
                .names
                .iter()
                .filter_map(|id| index.get(id).map(|&k| &ops[k]))
                .filter(|named| {
                    any_name
                        || (named.audience == op.audience && at.position_before(named.at, op.at))
                })
                .collect();
            named.extend(names.iter().map(|named| named.at));
            superseded.extend(superseded_in(op, &names));
        }
        Self {
            all: ops.iter().map(|op| op.at).collect(),
            rows: ops
                .iter()
                .map(|op| {
                    let commons = op.kind == Kind::Put && *op.audience == EpochAudience::Commons;
                    (op.at, (op.kind, op.epoch, commons))
                })
                .collect(),
            named,
            superseded,
        }
    }
}

/// Of the ops `names` (those `namer` names), the positions of those it supersedes: those of its
/// own signer, which it follows. A name of another signer's op supersedes nothing, whatever the
/// statement says of where that op stands: the frames cannot tell one register from another, so
/// a namer that names two ops of another signer as of its register (a stolen key naming them
/// falsely, say) would otherwise drop the older even where it is that signer's winner in a
/// register of its own, and a settled checkpoint's prune ([`gone`]) would make that drop final.
/// So a statement drops only what its own signer wrote, which a `Delete` of its signer's could
/// replace anyway.
fn superseded_in(namer: &Op<'_>, names: &[&Op<'_>]) -> Vec<usize> {
    #[cfg(test)]
    if mutant(Mutant::CrossSigner) {
        let mut newest: BTreeMap<MemberId, u64> = BTreeMap::new();
        for op in names.iter().copied().chain([namer]) {
            let at = newest.entry(op.actor).or_insert(0);
            *at = (*at).max(op.sequence);
        }
        return names
            .iter()
            .filter(|op| op.sequence < newest[&op.actor])
            .map(|op| op.at)
            .collect();
    }
    names
        .iter()
        .filter(|op| op.actor == namer.actor)
        .map(|op| op.at)
        .collect()
}

/// [`Ops::superseded`] of the records of `at`, as ids: the ops whose register holds a newer op
/// of their signer, by that op's statement, among the counting version-2 ops in `at`.
#[must_use]
pub fn superseded(at: &Log) -> BTreeSet<RecordId> {
    Ops::of(at)
        .superseded
        .into_iter()
        .filter_map(|i| at.id_at(i))
        .collect()
}

/// The newest epochs of `at` (by index) that some seat of their audience holds no counting
/// `Wrap` of yet: for `[everyone]` and the Commons, every current seat
/// ([`Log::current_recipients`]); for a host list, which the log does not name, each current
/// seat a counting `Wrap` of one of its epochs reaches.
fn owed_epochs(at: &Log) -> BTreeSet<usize> {
    let effective = at.effective();
    let order: Vec<&Record> = at.records().collect();
    let mut audiences: BTreeMap<EpochAudience, BTreeSet<RecordId>> = BTreeMap::new();
    let mut wrapped: BTreeMap<RecordId, BTreeSet<[u8; 32]>> = BTreeMap::new();
    for (i, record) in order.iter().enumerate() {
        if !effective.counts(i) {
            continue;
        }
        match record.body() {
            Body::Epoch { audience, .. } => {
                audiences
                    .entry(audience.clone())
                    .or_default()
                    .insert(*record.id());
            }
            Body::Wrap {
                epoch, recipient, ..
            } => {
                wrapped.entry(*epoch).or_default().insert(recipient.0);
            }
            _ => {}
        }
    }
    let mut owed = BTreeSet::new();
    for (audience, epochs) in &audiences {
        let current: BTreeSet<[u8; 32]> = at
            .current_recipients(audience)
            .into_iter()
            .map(|r| r.0)
            .collect();
        let seats: BTreeSet<[u8; 32]> = match audience {
            EpochAudience::Everyone | EpochAudience::Commons => current,
            EpochAudience::Hosts(_) => epochs
                .iter()
                .filter_map(|e| wrapped.get(e))
                .flatten()
                .filter(|r| current.contains(*r))
                .copied()
                .collect(),
        };
        for newest in at.newest_epochs(audience) {
            let has = wrapped.get(&newest).cloned().unwrap_or_default();
            if !seats.is_subset(&has)
                && let Some(i) = at.record_index(&newest)
            {
                owed.insert(i);
            }
        }
    }
    owed
}

/// The checkpoints a cut holds, as their shares say.
struct Checkpoints {
    /// Each body's id ([`checkpoint_id`]): the body, and its counting shares (indexes in the cut).
    bodies: BTreeMap<RecordId, (Body, Vec<usize>)>,
    /// The final ones, judged within the cut ([`final_within`]): the tests' `NewestByDay` reads
    /// them.
    #[cfg(test)]
    finals: BTreeSet<RecordId>,
    /// The newest final one: the final one whose cut contains every final one's, the latest by
    /// its day where several do, then the lowest id. `None` where no final one contains every
    /// other (the final ones of a split's two sides).
    newest: Option<RecordId>,
}

impl Checkpoints {
    fn of(at: &Log) -> Self {
        let mut bodies: BTreeMap<RecordId, (Body, Vec<usize>)> = BTreeMap::new();
        for (id, position) in counting_shares(at) {
            if let Some((_, shares)) = bodies.get_mut(&id) {
                shares.push(position);
            } else if let Some(body) = body_of(at, &id) {
                bodies.insert(id, (body, vec![position]));
            }
        }
        let mut finals = BTreeSet::new();
        for (id, (body, shares)) in &bodies {
            let signed = shares.iter().filter_map(|&i| at.actor_at(i)).collect();
            if final_within(at, id, body, &signed) {
                finals.insert(*id);
            }
        }
        let newest = newest_final(at, &bodies, &finals);
        Self {
            bodies,
            #[cfg(test)]
            finals,
            newest,
        }
    }

    /// The checkpoint records a prune keeps, by position ([`Log::position`]: a rooted log's
    /// dropped share at its stub's): every share of the newest final checkpoint, the lowest-id
    /// share of each checkpoint its predecessors name back from it, and every share of each
    /// checkpoint not nested in it; with no newest final checkpoint, every one.
    fn kept(&self, at: &Log) -> BTreeSet<usize> {
        let Some(newest) = self.newest else {
            return self
                .bodies
                .values()
                .flat_map(|(_, s)| s.iter().copied())
                .collect();
        };
        let (body, shares) = &self.bodies[&newest];
        let mut kept: BTreeSet<usize> = shares.iter().copied().collect();
        let newest_heads = cut_of(body)
            .map(|(_, heads)| heads.to_vec())
            .unwrap_or_default();
        // A rooted log's cuts may end at records its prunes dropped, placed at their stubs, as
        // [`newest_final`] reads them.
        #[cfg(test)]
        let held_nesting = super::rooted::mutant(super::rooted::Mutant::HeldNesting);
        #[cfg(not(test))]
        let held_nesting = false;
        let in_newest = |head: &RecordId| {
            if at.rooting().is_some() && !held_nesting {
                at.in_cut_of(&newest_heads, head)
            } else {
                at.reaches(&newest_heads, head)
            }
        };
        let mut chain = BTreeSet::from([newest]);
        let mut link = predecessor_of(body);
        while let Some(id) = link
            && chain.insert(id)
            && let Some((body, shares)) = self.bodies.get(&id)
        {
            if let Some(&lowest) = shares.iter().min_by_key(|&&i| at.id_at(i)) {
                kept.insert(lowest);
            }
            link = predecessor_of(body);
        }
        for (id, (body, shares)) in &self.bodies {
            if chain.contains(id) {
                continue;
            }
            #[cfg(test)]
            if mutant(Mutant::NewestByDay) && self.finals.contains(id) {
                continue;
            }
            let nested = cut_of(body).is_some_and(|(_, heads)| heads.iter().all(in_newest));
            if !nested {
                kept.extend(shares.iter().copied());
            }
        }
        kept
    }
}

/// Each share `at` holds that counts, with the checkpoint it signs and its position
/// ([`Log::position`]): a held share, and a rooted log's dropped one at its stub
/// ([`Stub::shares`]), counted as the evaluators count it there.
fn counting_shares(at: &Log) -> Vec<(RecordId, usize)> {
    let effective = at.effective();
    #[cfg(test)]
    let counts = |i: usize| effective.counts(i) || mutant(Mutant::AnyShare);
    #[cfg(not(test))]
    let counts = |i: usize| effective.counts(i);
    let mut out: Vec<(RecordId, usize)> = at
        .records()
        .enumerate()
        .filter(|&(i, _)| counts(i))
        .filter_map(|(i, record)| Some((checkpoint_id(record.body())?, i)))
        .collect();
    out.extend(
        dropped_shares(at)
            .filter(|&(_, position, _)| counts(position))
            .map(|(id, position, _)| (id, position)),
    );
    out
}

/// A rooted log's dropped shares ([`Stub::shares`]): the checkpoint each signs, its stub's
/// position and its signer. None for a whole log.
fn dropped_shares(at: &Log) -> impl Iterator<Item = (RecordId, usize, MemberId)> + '_ {
    at.rooting()
        .into_iter()
        .flat_map(|rooting| rooting.stubs.iter())
        .filter_map(|(stub_id, stub)| Some((stub.shares?, at.position(stub_id)?, stub.actor)))
}

/// The body of the checkpoint `id` as `at` holds it: a held share's, a root's or an earlier
/// root's ([`super::rooted`]), or, where a prune dropped every share of it, read off a dropped
/// share's frame ([`Stub::share_body`]). `None` where `at` holds no share of it, held or dropped.
fn body_of(at: &Log, id: &RecordId) -> Option<Body> {
    at.records()
        .find(|record| checkpoint_id(record.body()).as_ref() == Some(id))
        .map(|record| record.body().clone())
        .or_else(|| {
            let rooting = at.rooting()?;
            rooting
                .roots
                .iter()
                .chain(&rooting.earlier)
                .find(|root| root.id == *id)
                .map(|root| root.body.clone())
                .or_else(|| {
                    rooting
                        .stubs
                        .values()
                        .filter(|stub| stub.shares.as_ref() == Some(id))
                        .find_map(|stub| stub.share_body(at.network()))
                })
        })
}

/// Whether the checkpoint `body` (its id `id`) is final within the cut `at`, by the shares of
/// `signed`: its cut passes [`plan_at`]'s checks in `at`, more than half of that cut's recent
/// seats signed it, and it recomputes at its heads.
fn final_within(at: &Log, id: &RecordId, body: &Body, signed: &BTreeSet<MemberId>) -> bool {
    let Some((_, heads)) = cut_of(body) else {
        return false;
    };
    #[cfg(test)]
    if mutant(Mutant::SharesAlone) {
        let cut_ms = cut_of(body).map_or(0, |(cut_ms, _)| cut_ms);
        if heads.iter().any(|h| at.get(h).is_none()) {
            return false;
        }
        return projection(at, at.ancestor_closure(heads), heads)
            .is_ok_and(|past| majority(&recent_in(&past, cut_ms), signed));
    }
    if heads.iter().any(|head| !at.knows(head)) {
        return false;
    }
    if !cut_facts(at, *id, body, false)
        .recent
        .is_some_and(|recent| majority(&recent, signed))
    {
        return false;
    }
    // A root of a rooted log, and the predecessor its body names, recomputed where the log
    // adopted it ([`Log::adopt`]); a log pruned at a later root cannot always recompute an
    // earlier checkpoint, whose kept records that prune may drop.
    if at.rooting().is_some_and(|rooting| {
        rooting
            .roots
            .iter()
            .any(|root| root.id == *id || predecessor_of(&root.body) == Some(*id))
    }) {
        return true;
    }
    // A body's recomputation is a function of its own cut ([`Cuts`]), so each is computed once
    // however deep the chain.
    cut_facts(at, *id, body, true)
        .dropped
        .is_some_and(|dropped| dropped.is_some())
}

/// The newest of `finals` ([`Checkpoints::newest`]).
fn newest_final(
    at: &Log,
    bodies: &BTreeMap<RecordId, (Body, Vec<usize>)>,
    finals: &BTreeSet<RecordId>,
) -> Option<RecordId> {
    let cut = |id: &RecordId| cut_of(&bodies[id].0).unwrap_or((0, &[]));
    let latest = |id: &&RecordId| (cut(id).0, std::cmp::Reverse(**id));
    #[cfg(test)]
    if mutant(Mutant::NewestByDay) {
        return finals.iter().max_by_key(latest).copied();
    }
    // A rooted log's cut may end at records its prunes dropped, placed at their stubs.
    let reaches = |holds: &[RecordId], head: &RecordId| {
        if at.rooting().is_some() {
            holds
                .iter()
                .any(|outer| outer == head || at.lies_before(head, outer))
        } else {
            at.reaches(holds, head)
        }
    };
    finals
        .iter()
        .filter(|outer| {
            let (_, holds) = cut(outer);
            finals
                .iter()
                .all(|inner| cut(inner).1.iter().all(|h| reaches(holds, h)))
        })
        .max_by_key(latest)
        .copied()
}

/// A checkpoint's cut and heads; `None` for any other body.
fn cut_of(body: &Body) -> Option<(u64, &[RecordId])> {
    match body {
        Body::Checkpoint { cut_ms, heads, .. } => Some((*cut_ms, heads)),
        _ => None,
    }
}

/// A checkpoint's predecessor; `None` for any other body.
fn predecessor_of(body: &Body) -> Option<RecordId> {
    match body {
        Body::Checkpoint { predecessor, .. } => *predecessor,
        _ => None,
    }
}

/// The seats of `at` that signed one of its records within [`finality::HEARD_WITHIN_MS`] before
/// `cut_ms` ([`finality::recent`]).
fn recent_seats(at: &Log, cut_ms: u64) -> BTreeSet<MemberId> {
    let seats: BTreeSet<MemberId> = at.membership_ref().members().keys().copied().collect();
    finality::recent(&seats, cut_ms, |seat| last_signed(at, seat, cut_ms))
}

/// The seats at a cut, `at` (its causal past), that finality counts: [`recent_seats`].
fn recent_in(at: &Log, cut_ms: u64) -> BTreeSet<MemberId> {
    #[cfg(test)]
    if mutant(Mutant::EverySeatRecent) {
        return at.membership_ref().members().keys().copied().collect();
    }
    recent_seats(at, cut_ms)
}

/// [`finality::is_final`], or its tests' mutant.
fn majority(recent: &BTreeSet<MemberId>, signed: &BTreeSet<MemberId>) -> bool {
    #[cfg(test)]
    if mutant(Mutant::HalfIsFinal) {
        let shares = recent.intersection(signed).count();
        return !recent.is_empty() && shares * 2 >= recent.len();
    }
    finality::is_final(recent, signed)
}

/// The records a prune of the cut `at` holds whole keeps, by id (the module's keep set), and, of
/// a rooted log's stubs in the cut ([`super::rooted`]), those the whole log's prune keeps. A stub
/// of an op is read as its record is ([`ops_of`]): a later prune may have dropped an op this
/// cut's prune keeps (a winner here that an op past this cut superseded), and a log rooted there
/// keeps it here, as a stub, as the whole log keeps its record, so it computes this cut's
/// checkpoint as the whole log does.
fn keep_set(at: &Log, checkpoints: &Checkpoints) -> (BTreeSet<RecordId>, BTreeSet<RecordId>) {
    let effective = at.effective();
    let order: Vec<&Record> = at.records().collect();
    let ops = Ops::of(at);
    let owed = owed_epochs(at);
    let mut keep = vec![false; order.len()];
    for (i, record) in order.iter().enumerate() {
        keep[i] = match record.kind() {
            Kind::Genesis | Kind::Admit | Kind::Remove | Kind::Name | Kind::Node => true,
            // A record outside the cut may be sealed under any epoch (the module's keep set).
            Kind::Epoch | Kind::Wrap => true,
            // An op is kept below, by what [`Ops`] reads of it.
            Kind::Put | Kind::Delete | Kind::Checkpoint => false,
        };
    }
    let mut kept_stubs: BTreeSet<RecordId> = BTreeSet::new();
    let keep_at = |keep: &mut Vec<bool>, kept_stubs: &mut BTreeSet<RecordId>, p: usize| match keep
        .get_mut(p)
    {
        Some(slot) => *slot = true,
        None => kept_stubs.extend(at.id_at(p)),
    };
    #[cfg(test)]
    let stubs_for_good = super::rooted::mutant(super::rooted::Mutant::StubsDroppedForGood);
    #[cfg(not(test))]
    let stubs_for_good = false;
    for &p in &ops.all {
        let (_, epoch, _) = ops.rows[&p];
        let owes = at.record_index(&epoch).is_some_and(|e| owed.contains(&e));
        if stubs_for_good && p >= keep.len() && !owes {
            continue;
        }
        // Every op no newer op of its signer superseded, and every op under an epoch a seat is
        // not yet wrapped to.
        if !ops.superseded.contains(&p) || owes {
            keep_at(&mut keep, &mut kept_stubs, p);
        }
    }
    // The Commons control's candidates: the causally newest counting Commons `Put`s.
    let commons: Vec<usize> = ops
        .all
        .iter()
        .copied()
        .filter(|&p| ops.rows[&p].2 && effective.counts(p))
        .collect();
    for &p in &commons {
        if stubs_for_good && p >= keep.len() {
            continue;
        }
        if !commons.iter().any(|&q| at.position_before(p, q)) {
            keep_at(&mut keep, &mut kept_stubs, p);
        }
    }
    // A share the whole log's prune keeps that an earlier prune dropped stays a stub.
    for p in checkpoints.kept(at) {
        keep_at(&mut keep, &mut kept_stubs, p);
    }
    let stubs = stubs_of(at);
    let op_stub = |id: &RecordId| {
        stubs
            .get(id)
            .is_some_and(|stub| matches!(stub.kind, Kind::Put | Kind::Delete))
    };
    // A record a settled checkpoint dropped counts nowhere ([`gone`]): this prune drops it too,
    // held or a stub, and keeps nothing for its sake, as a log pruned at that checkpoint holds
    // only its stub; a log pruned here reads it gone where that checkpoint is settled, as the
    // whole log does.
    let gone = |id: &RecordId| at.position(id).is_some_and(|p| effective.is_gone(p));
    for (i, slot) in keep.iter_mut().enumerate() {
        *slot &= !effective.is_gone(i);
    }
    kept_stubs.retain(|id| !gone(id));
    // Close: what a kept record names (the op whose value an op carries, a `Remove`'s cutoff);
    // every `Epoch` and `Wrap` is kept already.
    loop {
        let mut grew = false;
        let mut mark =
            |keep: &mut Vec<bool>, kept_stubs: &mut BTreeSet<RecordId>, id: &RecordId| {
                if gone(id) {
                    return;
                }
                if let Some(i) = at.record_index(id)
                    && !keep[i]
                {
                    keep[i] = true;
                    grew = true;
                } else if stubs.contains_key(id) && kept_stubs.insert(*id) {
                    grew = true;
                }
            };
        for i in 0..order.len() {
            if !keep[i] {
                continue;
            }
            match order[i].body() {
                // A `Put` carries what its statement names; a `Delete` carries nothing, as its
                // stub keeps no statement's carry ([`Stub::carries`]).
                Body::Put { .. } => {
                    if let Some(Statement {
                        carries: Some(carried),
                        ..
                    }) = order[i].statement()
                        && (at
                            .get(carried)
                            .is_some_and(|r| matches!(r.kind(), Kind::Put | Kind::Delete))
                            || op_stub(carried))
                    {
                        mark(&mut keep, &mut kept_stubs, carried);
                    }
                }
                Body::Remove { cutoff, .. } => mark(&mut keep, &mut kept_stubs, cutoff),
                _ => {}
            }
        }
        // A kept dropped `Put` carries what its statement names; a `Delete` states no carry.
        let carried: Vec<RecordId> = kept_stubs
            .iter()
            .filter_map(|id| stubs.get(id))
            .filter(|stub| stub.kind == Kind::Put)
            .filter_map(|stub| stub.carries)
            .filter(|c| {
                at.get(c)
                    .is_some_and(|r| matches!(r.kind(), Kind::Put | Kind::Delete))
                    || op_stub(c)
            })
            .collect();
        for c in carried {
            mark(&mut keep, &mut kept_stubs, &c);
        }
        if !grew {
            break;
        }
    }
    let held = order
        .iter()
        .zip(keep)
        .filter(|(_, kept)| *kept)
        .map(|(record, _)| *record.id())
        .collect();
    (held, kept_stubs)
}

/// A checkpoint `log` holds settled ([`settled`]): the ids its prune drops.
#[derive(Clone, Debug)]
pub(super) struct Settled {
    /// [`checkpoint_id`] of its body, which the tests compare across logs.
    #[cfg(test)]
    pub(super) id: RecordId,
    /// What a prune at it drops, sorted ([`Plan::dropped`]).
    pub(super) dropped: Arc<Vec<RecordId>>,
}

/// What each checkpoint's cut says, by its id ([`CutFacts`]), shared by a log, its clones and the
/// projections of its cuts ([`projection`]). Each answer is a function of the cut alone (the
/// records and stubs in its heads' past), which every log of the family that holds those heads
/// holds alike, so it is kept as the log grows; a prune starts a log, and this cache, afresh
/// ([`Log::rooted_at`]). Without it each evaluation computed a settled checkpoint's plan once for
/// every checkpoint whose cut nests it, through each cut's own evaluation ([`gone`]): a chain of
/// nested checkpoints cost about 2.5 times more for each one it grew by.
pub(super) type Cuts = Arc<Mutex<BTreeMap<CutKey, CutFacts>>>;

/// A [`Cuts`] key: the checkpoint's id, and in the tests the mutants it was computed as.
#[cfg(not(test))]
pub(super) type CutKey = RecordId;
#[cfg(test)]
pub(super) type CutKey = (RecordId, u8, u8);

fn cut_key(id: RecordId) -> CutKey {
    #[cfg(test)]
    {
        let rooted = super::rooted::MUTANT.get().map_or(0, |m| m as u8 + 1);
        (id, MUTANT.get().map_or(0, |m| m as u8 + 1), rooted)
    }
    #[cfg(not(test))]
    id
}

/// What a checkpoint's cut says ([`Cuts`]).
#[derive(Clone, Debug, Default)]
pub(super) struct CutFacts {
    /// The seats finality counts there ([`recent_in`]); `None` where the cut fails [`plan_at`]'s
    /// checks.
    recent: Option<BTreeSet<MemberId>>,
    /// Once asked: what the cut's plan drops where it recomputes the checkpoint, `None` where it
    /// computes another body.
    dropped: Option<Option<Arc<Vec<RecordId>>>>,
}

/// The [`CutFacts`] of the checkpoint `id`, whose body is `body`, in `log`, which holds every head
/// of its cut: from [`Cuts`], or computed (the plan only where `plan` asks) and kept there.
fn cut_facts(log: &Log, id: RecordId, body: &Body, plan: bool) -> CutFacts {
    let key = cut_key(id);
    let lock = || {
        log.cuts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    #[cfg(test)]
    let cached = if mutant(Mutant::NoCutMemo) {
        None
    } else {
        lock().get(&key).cloned()
    };
    #[cfg(not(test))]
    let cached = lock().get(&key).cloned();
    if let Some(facts) = &cached
        && (!plan || facts.recent.is_none() || facts.dropped.is_some())
    {
        return facts.clone();
    }
    let mut facts = cached.unwrap_or_default();
    let Some((cut_ms, heads)) = cut_of(body) else {
        return facts;
    };
    let cut = cut_log(log, cut_ms, heads).ok();
    if facts.recent.is_none() {
        facts.recent = cut.as_ref().map(|cut| recent_in(&cut.log, cut_ms));
    }
    if plan && let Some(cut) = cut {
        let computed = plan_of(&cut.log, cut_ms, cut.heads);
        facts.dropped =
            Some((checkpoint_id(&computed.body) == Some(id)).then(|| Arc::new(computed.dropped)));
    }
    lock().insert(key, facts.clone());
    facts
}

#[cfg(test)]
thread_local! {
    /// How many plans this thread computed ([`plan_of`]).
    pub(super) static PLANS_COMPUTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The checkpoints `log` holds settled ([`Settled`]): each whose body it knows ([`body_of`]: a
/// held share's, a root's, or a dropped share's, read off its frame) and computes itself at its
/// heads ([`plan_at`]), and more than half of whose cut's
/// recent seats signed a share of it that `log` holds or dropped ([`Stub::shares`]), whether or
/// not the share counts. A share counts for [`judge`]'s finality only where its record does, which
/// a later `Remove` can undo; settled asks only that a recent seat's key signed it, so it is a
/// fact of the records once held, and a log that holds a checkpoint settled holds it settled
/// however it grows, as a prune needs. [`judge`]'s finality implies it, so every root of a rooted
/// log ([`Log::adopt`]) is settled.
///
/// Settledness is as weak as finality's recent seats, and weaker in one way: a seat alone at its
/// cut settles it (a side of a split, a vault whose other seats are dead, or a key that signs its
/// own head with a clock days ahead, so that it alone is recent), and a share a stolen key signed
/// past its seat's cutoff still settles. What that buys a thief is bounded by what a plan drops,
/// which the cut's own records decide and every Mac that holds them recomputes: an older op its
/// own signer's newer op superseded ([`superseded_in`]: never another signer's, never a winner)
/// and an older checkpoint's shares. So a stolen key can make final, at worst, the loss of its own
/// seat's values its writes superseded: a `Remove` of it then voids those writes and brings back
/// no value they replaced (the entry is unset until set again), the cost the design accepts
/// (`docs/DESIGN-vault-log-2026-11-14.md`, section 5).
pub(super) fn settled(log: &Log) -> Vec<Settled> {
    let mut signers: BTreeMap<RecordId, BTreeSet<MemberId>> = BTreeMap::new();
    for record in log.records() {
        if let Some(id) = checkpoint_id(record.body()) {
            signers.entry(id).or_default().insert(*record.actor());
        }
    }
    for (id, _, actor) in dropped_shares(log) {
        signers.entry(id).or_default().insert(actor);
    }
    let mut out = Vec::new();
    for (id, signed) in signers {
        let Some(body) = body_of(log, &id) else {
            continue;
        };
        let Some((_, heads)) = cut_of(&body) else {
            continue;
        };
        if heads.iter().any(|head| !log.knows(head)) {
            continue;
        }
        let recent = cut_facts(log, id, &body, false).recent;
        if !recent.is_some_and(|recent| majority(&recent, &signed)) {
            continue;
        }
        if let Some(Some(dropped)) = cut_facts(log, id, &body, true).dropped {
            out.push(Settled {
                #[cfg(test)]
                id,
                dropped,
            });
        }
    }
    out
}

/// The positions in `log` ([`Log::position`]), held or dropped, of every record a checkpoint
/// `log` holds settled ([`settled`]) drops: none of them counts, in this log or any other, as in
/// a log rooted at that checkpoint, which holds none of them. `None` where `log` holds no
/// settled checkpoint, so a log with none reads every record as its evaluation alone says.
///
/// A rooted log reads it so too, at every cut: whether a stub counts there is whether a
/// checkpoint settled there drops it, which the cut's own shares say, held or dropped, as in
/// the whole log ([`body_of`] reads a checkpoint every share of which a prune dropped off a
/// share's frame). No stub says it of itself: a stub a later cut holds gone may count in an
/// earlier cut that the checkpoint making it gone is not settled in (its shares lie between
/// the two cuts), and an earlier checkpoint's plan keeps it there, as the whole log's does. So
/// in the log rooted at a settled cut every stub is gone (the root, and each earlier root, is
/// settled there and drops it), and in an earlier cut each reads as its record does.
///
/// So a `Remove`, or a quorum, whose cutoff lies behind a settled cut revives nothing the prune
/// at it dropped: it takes out its member and stops counting its records past the cutoff, those
/// in the cut too, and an older op of the member's own that one of them superseded (a register's
/// older value: a name supersedes only its own signer's op, [`superseded_in`]) stays dropped, in
/// the whole log as in the rooted one. The entry a voided write held is then unset until it is
/// set again, as the design accepted (`docs/DESIGN-vault-log-2026-11-14.md`, section 5). The rule
/// only stops records counting, and only records the cut's own plan drops: a superseded op or an
/// older share, never a winner, a sibling or a weighed record, so no settled cut makes a record
/// count that a `Remove` took out.
pub(super) fn gone(log: &Log) -> Option<super::eval::Gone> {
    #[cfg(test)]
    if super::rooted::mutant(super::rooted::Mutant::Revives) {
        return None;
    }
    let settled = settled(log);
    #[cfg(test)]
    let last_cut: Vec<RecordId> =
        if super::rooted::mutant(super::rooted::Mutant::GoneFromTheLastCut) {
            super::rooted::LAST_CUT_GONE.with_borrow(|gone| {
                stubs_of(log)
                    .keys()
                    .filter(|id| gone.contains(*id))
                    .copied()
                    .collect()
            })
        } else {
            Vec::new()
        };
    #[cfg(not(test))]
    let last_cut: Vec<RecordId> = Vec::new();
    if settled.is_empty() && last_cut.is_empty() {
        return None;
    }
    Some(
        settled
            .iter()
            .flat_map(|settled| settled.dropped.iter())
            .chain(&last_cut)
            .filter_map(|id| log.position(id))
            .collect(),
    )
}

/// The ids of the records that count nowhere in the cut of the checkpoint `body` in `log`
/// ([`gone`] of that cut), held or stubs: what FL-148C's prune at it kept at each stub, which
/// the tests' `GoneFromTheLastCut` reads.
///
/// # Errors
/// `body` is no checkpoint, or `log` does not hold its cut ([`plan_at`]'s errors).
#[cfg(test)]
pub(super) fn gone_in_cut(log: &Log, body: &Body) -> Result<BTreeSet<RecordId>> {
    let (cut_ms, heads) = cut_of(body).ok_or_else(|| Error::new("not a checkpoint"))?;
    let cut = cut_log(log, cut_ms, heads)?;
    let effective = cut.log.effective();
    Ok(cut
        .log
        .records()
        .map(|record| *record.id())
        .chain(stubs_of(&cut.log).keys().copied())
        .filter(|id| cut.log.position(id).is_some_and(|p| effective.is_gone(p)))
        .collect())
}

/// How this log judges the checkpoint `body` ([`Judgment`]): it recomputes the body at its heads
/// ([`plan_at`]), and counts the shares of exactly this body among every counting record it holds
/// against the seats at the cut heard within [`finality::HEARD_WITHIN_MS`] before it.
///
/// # Errors
/// `body` is no checkpoint, or this log cannot compute a body at its heads ([`plan_at`]'s
/// errors): a Mac without the cut's past reads it through the stubs a peer served with it, and
/// judges it here only once they compute it ([`super::served`]).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "Judge",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
pub fn judge(log: &Log, body: &Body) -> Result<Judgment> {
    let (cut_ms, heads) = cut_of(body).ok_or_else(|| Error::new("not a checkpoint"))?;
    let at = cut_log(log, cut_ms, heads)?;
    let id = checkpoint_id(body);
    let recomputed = checkpoint_id(&plan_of(&at.log, cut_ms, at.heads.clone()).body) == id;
    let signed: BTreeSet<MemberId> = counting_shares(log)
        .into_iter()
        .filter(|(share, _)| Some(*share) == id)
        .filter_map(|(_, position)| log.actor_at(position))
        .collect();
    let recent = recent_in(&at.log, cut_ms);
    let is_final = recomputed && majority(&recent, &signed);
    let shared: BTreeSet<MemberId> = log
        .records()
        .filter(|record| checkpoint_id(record.body()) == id)
        .map(|record| *record.actor())
        .chain(
            dropped_shares(log)
                .filter(|&(share, _, _)| Some(share) == id)
                .map(|(_, _, actor)| actor),
        )
        .collect();
    let is_settled = recomputed && majority(&recent, &shared);
    Ok(Judgment {
        recomputed,
        recent,
        signed,
        is_final,
        shared,
        is_settled,
    })
}

/// Whether `seat` may share the checkpoint `body` in `log`: this log computes the body itself at
/// its heads, and its cut holds every head of every checkpoint `seat` signed before, read from
/// its own records in `log`. So a seat's shares never shrink, and two final checkpoints whose
/// majorities share a seat nest (the module's share rule says what a split does).
///
/// # Errors
/// As [`judge`].
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareA1",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareA2",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareA3",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareB1",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareB2",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareB3",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareC1",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareC2",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "VaultCheckpoint",
        action = "ShareC3",
        project = "aterm_secrets::membership_log::checkpoint::tests::model_transition"
    )
)]
pub fn may_share(log: &Log, seat: &MemberId, body: &Body) -> Result<bool> {
    let (cut_ms, heads) = cut_of(body).ok_or_else(|| Error::new("not a checkpoint"))?;
    let at = cut_log(log, cut_ms, heads)?;
    if checkpoint_id(&plan_of(&at.log, cut_ms, at.heads.clone()).body) != checkpoint_id(body) {
        return Ok(false);
    }
    let id = checkpoint_id(body);
    Ok(log
        .records()
        .filter(|r| r.actor() == seat && checkpoint_id(r.body()).is_some_and(|c| Some(c) != id))
        .filter_map(|r| cut_of(r.body()))
        .all(|(shared_ms, shared)| {
            #[cfg(test)]
            if mutant(Mutant::ByDayAlone) {
                return shared_ms < cut_ms;
            }
            let _ = shared_ms;
            shared.iter().all(|h| log.reaches(&at.heads, h))
        }))
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
pub(super) mod tests;
